//! Core Audio HAL input and a private global process tap (macOS 14.4+).
#![allow(unsafe_code)]

use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use objc2::{AnyThread, runtime::AnyObject};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_audio::*;
use objc2_core_audio_types::*;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

use crate::{
    AudioFrame, AudioSource, CaptureEvent, CaptureMessage, Channel, DeviceInfo, FRAME_SAMPLES,
    FallbackReason, Inventory, LiveOptions, Normalizer, SourceTask, sample_offset,
    source::{recover_default, send},
};

const GLOBAL: u32 = kAudioObjectPropertyScopeGlobal;
const INPUT: u32 = kAudioObjectPropertyScopeInput;
const PERMISSION: &str = "Проверьте System Settings > Privacy & Security: Microphone и System Audio Recording для notetaker, затем перезапустите приложение";

fn status(code: i32, operation: &str) -> Result<()> {
    ensure!(
        code == 0,
        "{operation}: Core Audio OSStatus {code}. {PERMISSION}"
    );
    Ok(())
}
fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}
fn property<T: Copy>(object: u32, selector: u32, scope: u32) -> Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>() as u32;
    // SAFETY: HAL writes at most size bytes into a live, aligned T.
    unsafe {
        status(
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address(selector, scope)),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(value.as_mut_ptr()).unwrap().cast(),
            ),
            "Чтение свойства устройства",
        )?;
    }
    ensure!(
        size as usize == size_of::<T>(),
        "Неверный размер свойства Core Audio"
    );
    // Only plain HAL scalars, CF pointers and ASBD are read here.
    Ok(unsafe { value.assume_init() })
}
fn objects(object: u32, selector: u32) -> Result<Vec<u32>> {
    let address = address(selector, GLOBAL);
    let mut size = 0;
    unsafe {
        status(
            AudioObjectGetPropertyDataSize(
                object,
                NonNull::from(&address),
                0,
                ptr::null(),
                NonNull::from(&mut size),
            ),
            "Размер списка устройств",
        )?;
        ensure!(
            (size as usize).is_multiple_of(size_of::<u32>()),
            "Неверный размер списка Core Audio"
        );
        let mut values = vec![0; size as usize / size_of::<u32>()];
        if values.is_empty() {
            return Ok(values);
        }
        status(
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(values.as_mut_ptr()).unwrap().cast(),
            ),
            "Список устройств",
        )?;
        values.truncate(size as usize / size_of::<u32>());
        Ok(values)
    }
}
fn string_property(object: u32, selector: u32) -> Result<String> {
    let raw: *const CFString = property(object, selector, GLOBAL)?;
    // Core Audio CF properties follow the Create rule; release after conversion.
    let value = unsafe {
        CFRetained::from_raw(NonNull::new(raw.cast_mut()).context("Пустое имя устройства")?)
    };
    Ok(value.to_string())
}
fn default_device(channel: Channel) -> Result<u32> {
    let selector = match channel {
        Channel::Mic => kAudioHardwarePropertyDefaultInputDevice,
        Channel::System => kAudioHardwarePropertyDefaultOutputDevice,
    };
    let device = property(kAudioObjectSystemObject as u32, selector, GLOBAL)?;
    ensure!(device != 0, "Нет устройства по умолчанию: {channel:?}");
    Ok(device)
}

/// Device activity is a hint. Process input activity excludes our own capture.
pub fn microphone_processes() -> Result<Vec<u32>> {
    let device = default_device(Channel::Mic)?;
    if property::<u32>(device, kAudioDevicePropertyDeviceIsRunningSomewhere, GLOBAL)? == 0 {
        return Ok(vec![]);
    }
    let mut pids = Vec::new();
    for process in objects(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyProcessObjectList,
    )? {
        if property::<u32>(process, kAudioProcessPropertyPID, GLOBAL).ok()
            != Some(std::process::id())
            && property::<u32>(process, kAudioProcessPropertyIsRunningInput, GLOBAL).unwrap_or(0)
                != 0
        {
            pids.push(property(process, kAudioProcessPropertyPID, GLOBAL)?);
        }
    }
    Ok(pids)
}

pub fn inventory() -> Result<Inventory> {
    let system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );
    let mut result = Inventory {
        processes: system
            .processes()
            .values()
            .map(|p| crate::ProcessInfo {
                pid: p.pid().as_u32(),
                parent_pid: p.parent().map(|id| id.as_u32()),
                exe: p.name().to_string_lossy().into_owned(),
            })
            .collect(),
        ..Inventory::default()
    };
    for id in objects(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDevices,
    )? {
        for (channel, scope) in [
            (Channel::Mic, INPUT),
            (Channel::System, kAudioObjectPropertyScopeOutput),
        ] {
            if !property::<AudioStreamBasicDescription>(id, kAudioDevicePropertyStreamFormat, scope)
                .is_ok_and(|f| f.mChannelsPerFrame > 0)
            {
                continue;
            }
            match string_property(id, kAudioObjectPropertyName) {
                Ok(name) => result.devices.push(DeviceInfo {
                    id: id.to_string(),
                    name,
                    channel,
                    is_default: default_device(channel).ok() == Some(id),
                }),
                Err(error) => result.warnings.push(format!("{error:#}")),
            }
        }
    }
    result.warnings.push("macOS: записывается весь системный звук, кроме notetaker. Выбор отдельного приложения недоступен. При тишине проверьте разрешение System Audio Recording.".into());
    Ok(result)
}

pub struct LiveSource {
    channel: Channel,
    options: LiveOptions,
    task: SourceTask,
}
pub fn live_sources(options: LiveOptions) -> Result<[LiveSource; 2]> {
    ensure!(options.process_id != Some(0), "PID 0 cannot be captured");
    Ok([Channel::Mic, Channel::System].map(|channel| LiveSource {
        channel,
        options: options.clone(),
        task: SourceTask::default(),
    }))
}
impl AudioSource for LiveSource {
    fn start(&mut self, sender: SyncSender<CaptureMessage>) -> Result<()> {
        let channel = self.channel;
        let options = self.options.clone();
        self.task.start(channel, sender, move |stop, sender| {
            if channel == Channel::Mic {
                microphone_permission(stop)?;
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if options.process_id.is_some() && channel == Channel::System {
                send(
                    sender,
                    stop,
                    CaptureMessage::Event(CaptureEvent::Fallback {
                        at: options.origin.elapsed(),
                        reason: FallbackReason::ActivationFailed {
                            message: "macOS: используется весь системный звук, кроме notetaker"
                                .into(),
                        },
                    }),
                );
            }
            recover_default(
                |use_default, healthy| {
                    capture(channel, &options, use_default, stop, sender, healthy)
                },
                |_| {
                    send(
                        sender,
                        stop,
                        CaptureMessage::Event(CaptureEvent::Discontinuity {
                            channel,
                            at: options.origin.elapsed(),
                        }),
                    );
                },
            )
        })
    }
    fn stop(&mut self) -> Result<()> {
        self.task.stop()
    }
}

fn microphone_permission(stop: &AtomicBool) -> Result<()> {
    unsafe {
        let media = AVMediaTypeAudio.context("Нет AVMediaTypeAudio")?;
        if AVCaptureDevice::authorizationStatusForMediaType(media)
            == AVAuthorizationStatus::NotDetermined
        {
            let (tx, rx) = mpsc::sync_channel(1);
            let block = block2::RcBlock::new(move |allowed: objc2::runtime::Bool| {
                let _ = tx.try_send(allowed.as_bool());
            });
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Ok(allowed) = rx.recv_timeout(Duration::from_millis(100)) {
                    ensure!(allowed, "Нет доступа к микрофону. {PERMISSION}");
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Разрешение микрофона не получено. {PERMISSION}"
                );
            }
        }
        ensure!(
            AVCaptureDevice::authorizationStatusForMediaType(media)
                == AVAuthorizationStatus::Authorized,
            "Нет доступа к микрофону. {PERMISSION}"
        );
    }
    Ok(())
}

// Fixed-size packets keep allocation and resampling off Core Audio's IO thread.
const MAX_SAMPLES: usize = 8192;
struct Packet {
    samples: [f32; MAX_SAMPLES],
    count: usize,
    host: u64,
}
struct Callback {
    sender: SyncSender<Packet>,
    failed: Arc<AtomicBool>,
    channels: usize,
}
unsafe extern "C-unwind" fn input(
    _: u32,
    _: NonNull<AudioTimeStamp>,
    data: NonNull<AudioBufferList>,
    time: NonNull<AudioTimeStamp>,
    _: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    context: *mut c_void,
) -> i32 {
    // SAFETY: Stream owns Callback until after AudioDeviceStop/DestroyIOProcID.
    // HAL owns the variable-length buffer list and all its planes for this call.
    unsafe {
        let state = &*context.cast::<Callback>();
        let list = data.as_ref();
        let buffers =
            std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize);
        if buffers.is_empty() {
            return 0;
        }
        let channels: usize = buffers.iter().map(|b| b.mNumberChannels as usize).sum();
        if channels != state.channels {
            state.failed.store(true, Ordering::Relaxed);
            return 0;
        }
        if channels == 0
            || buffers
                .iter()
                .any(|b| b.mNumberChannels == 0 || b.mData.is_null())
        {
            return 0;
        }
        let frames = buffers[0].mDataByteSize as usize
            / (size_of::<f32>() * buffers[0].mNumberChannels as usize);
        if frames == 0 {
            return 0;
        }
        if frames * channels > MAX_SAMPLES
            || buffers.iter().any(|b| {
                b.mDataByteSize as usize != frames * b.mNumberChannels as usize * size_of::<f32>()
            })
        {
            state.failed.store(true, Ordering::Relaxed);
            return 0;
        }
        let mut packet = Packet {
            samples: [0.0; MAX_SAMPLES],
            count: frames * channels,
            host: if time
                .as_ref()
                .mFlags
                .contains(AudioTimeStampFlags::HostTimeValid)
            {
                time.as_ref().mHostTime
            } else {
                0
            },
        };
        let mut offset = 0;
        for buffer in buffers {
            let width = buffer.mNumberChannels as usize;
            let samples = std::slice::from_raw_parts(buffer.mData.cast::<f32>(), frames * width);
            for frame in 0..frames {
                packet.samples[frame * channels + offset..frame * channels + offset + width]
                    .copy_from_slice(&samples[frame * width..frame * width + width]);
            }
            offset += width;
        }
        if state.sender.try_send(packet).is_err() {
            state.failed.store(true, Ordering::Relaxed);
        }
    }
    0
}

struct Stream {
    device: u32,
    tap: u32,
    aggregate: bool,
    proc: AudioDeviceIOProcID,
    callback: Box<Callback>,
}
impl Stream {
    fn new(device: u32, sender: SyncSender<Packet>, failed: Arc<AtomicBool>) -> Self {
        Self {
            device,
            tap: 0,
            aggregate: false,
            proc: None,
            callback: Box::new(Callback {
                sender,
                failed,
                channels: 0,
            }),
        }
    }
    fn tap(&mut self) -> Result<AudioStreamBasicDescription> {
        unsafe {
            let processes = objects(
                kAudioObjectSystemObject as u32,
                kAudioHardwarePropertyProcessObjectList,
            )?;
            let own = processes
                .into_iter()
                .find(|&p| {
                    property::<u32>(p, kAudioProcessPropertyPID, GLOBAL).ok()
                        == Some(std::process::id())
                })
                .context("Не найден Core Audio process object notetaker")?;
            let excluded = NSArray::from_retained_slice(&[NSNumber::new_u32(own)]);
            let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &excluded,
            );
            description.setPrivate(true);
            status(
                AudioHardwareCreateProcessTap(Some(&description), &mut self.tap),
                "Создание системного tap",
            )?;
            let format = property(self.tap, kAudioTapPropertyFormat, GLOBAL)?;
            let uid = description.UUID().UUIDString();
            let subkeys = [
                NSString::from_str(kAudioSubTapUIDKey.to_str()?),
                NSString::from_str(kAudioSubTapDriftCompensationKey.to_str()?),
            ];
            let yes = NSNumber::new_bool(true);
            let subtap = NSDictionary::from_slices(
                &[&*subkeys[0], &*subkeys[1]],
                &[&*uid as &AnyObject, &*yes],
            );
            let taps = NSArray::from_retained_slice(&[subtap]);
            let subdevices = NSArray::<NSDictionary<NSString, AnyObject>>::new();
            let id = NSUUID::new().UUIDString();
            let name = NSString::from_str("notetaker system audio");
            let keys: Vec<_> = [
                kAudioAggregateDeviceUIDKey,
                kAudioAggregateDeviceNameKey,
                kAudioAggregateDeviceIsPrivateKey,
                kAudioAggregateDeviceTapAutoStartKey,
                kAudioAggregateDeviceTapListKey,
                kAudioAggregateDeviceSubDeviceListKey,
            ]
            .iter()
            .map(|k| NSString::from_str(k.to_str().expect("ASCII Core Audio key")))
            .collect();
            let keys: Vec<_> = keys.iter().map(|k| &**k).collect();
            let values: [&AnyObject; 6] = [&*id, &*name, &*yes, &*yes, &*taps, &*subdevices];
            let dictionary = NSDictionary::from_slices(&keys, &values);
            // NSDictionary and CFDictionary are toll-free bridged; all keys are strings.
            let cf = &*ptr::from_ref(&*dictionary).cast::<CFDictionary>();
            let mut aggregate = 0;
            status(
                AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate)),
                "Создание устройства tap",
            )?;
            self.device = aggregate;
            self.aggregate = true;
            Ok(format)
        }
    }
    fn start(&mut self) -> Result<()> {
        // Aggregate creation is asynchronous. Do not start an unready device.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !property::<u32>(self.device, kAudioDevicePropertyDeviceIsAlive, GLOBAL)
            .is_ok_and(|alive| alive != 0)
        {
            ensure!(
                Instant::now() < deadline,
                "Устройство Core Audio не готово. {PERMISSION}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            status(
                AudioDeviceCreateIOProcID(
                    self.device,
                    Some(input),
                    ptr::from_mut(&mut *self.callback).cast(),
                    NonNull::from(&mut self.proc),
                ),
                "Подключение аудиопотока",
            )?;
            status(
                AudioDeviceStart(self.device, self.proc),
                "Запуск аудиопотока",
            )
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            if self.proc.is_some() {
                let _ = AudioDeviceStop(self.device, self.proc);
                let _ = AudioDeviceDestroyIOProcID(self.device, self.proc);
            }
            if self.aggregate {
                let _ = AudioHardwareDestroyAggregateDevice(self.device);
            }
            if self.tap != 0 {
                let _ = AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}

fn capture(
    channel: Channel,
    options: &LiveOptions,
    use_default: bool,
    stop: &AtomicBool,
    sender: &SyncSender<CaptureMessage>,
    healthy: &mut bool,
) -> Result<()> {
    if channel == Channel::Mic {
        unsafe {
            ensure!(
                AVCaptureDevice::authorizationStatusForMediaType(
                    AVMediaTypeAudio.context("Нет AVMediaTypeAudio")?
                ) == AVAuthorizationStatus::Authorized,
                "Нет доступа к микрофону. {PERMISSION}"
            );
        }
    }
    let default = default_device(channel)?;
    let device = if channel == Channel::Mic && !use_default {
        options
            .microphone_id
            .as_deref()
            .map(str::parse)
            .transpose()
            .context("Неверный ID микрофона")?
            .unwrap_or(default)
    } else {
        default
    };
    let (tx, rx) = mpsc::sync_channel(64);
    let failed = Arc::new(AtomicBool::new(false));
    let mut stream = Stream::new(device, tx, Arc::clone(&failed));
    let format: AudioStreamBasicDescription = if channel == Channel::System {
        stream.tap()?
    } else {
        property(device, kAudioDevicePropertyStreamFormat, INPUT)?
    };
    ensure!(
        format.mFormatID == kAudioFormatLinearPCM
            && format.mFormatFlags & kAudioFormatFlagIsFloat != 0
            && format.mBitsPerChannel == 32
            && format.mFormatFlags & kAudioFormatFlagIsBigEndian == 0,
        "Устройство должно предоставлять 32-bit float PCM"
    );
    let rate = format.mSampleRate as u32;
    let channels = format.mChannelsPerFrame as usize;
    stream.callback.channels = channels;
    let mut normalizer = Normalizer::new(rate, channels)?;
    let anchor_host = unsafe { AudioConvertHostTimeToNanos(AudioGetCurrentHostTime()) };
    let anchor_at = options.origin.elapsed();
    stream.start()?;
    let mut pending = VecDeque::new();
    let mut start = None;
    let mut produced = 0;
    let mut input_end: Option<Duration> = None;
    let mut checked = Instant::now();
    let mut received = Instant::now();
    let mut warned_timestamp = false;
    while !stop.load(Ordering::Relaxed) {
        ensure!(
            !failed.load(Ordering::Relaxed),
            "Потеря аудиопакетов или неподдерживаемый размер буфера"
        );
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(packet) => {
                received = Instant::now();
                ensure!(
                    packet.count.is_multiple_of(channels),
                    "Изменился формат аудиопотока"
                );
                let length =
                    Duration::from_secs_f64((packet.count / channels) as f64 / f64::from(rate));
                let at = if packet.host != 0 {
                    let host = unsafe { AudioConvertHostTimeToNanos(packet.host) };
                    if host >= anchor_host {
                        anchor_at + Duration::from_nanos(host - anchor_host)
                    } else {
                        anchor_at.saturating_sub(Duration::from_nanos(anchor_host - host))
                    }
                } else {
                    if !warned_timestamp {
                        send(
                            sender,
                            stop,
                            CaptureMessage::Event(CaptureEvent::TimestampError {
                                channel,
                                at: options.origin.elapsed(),
                            }),
                        );
                        warned_timestamp = true;
                    }
                    options.origin.elapsed().saturating_sub(length)
                };
                if input_end.is_some_and(|end| end.abs_diff(at) > Duration::from_millis(50)) {
                    send(
                        sender,
                        stop,
                        CaptureMessage::Event(CaptureEvent::Discontinuity { channel, at }),
                    );
                    normalizer = Normalizer::new(rate, channels)?;
                    pending.clear();
                    start = None;
                    produced = 0;
                }
                input_end = Some(at + length);
                let start = *start.get_or_insert(at);
                pending.extend(normalizer.push(&packet.samples[..packet.count])?);
                *healthy = true;
                while pending.len() >= FRAME_SAMPLES {
                    let frame = AudioFrame {
                        channel,
                        samples: pending.drain(..FRAME_SAMPLES).collect(),
                        captured_at: start + sample_offset(produced),
                    };
                    produced += FRAME_SAMPLES as u64;
                    if !send(sender, stop, CaptureMessage::Frame(frame)) {
                        return Ok(());
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Аудиопоток завершился. {PERMISSION}")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        ensure!(
            received.elapsed() < Duration::from_secs(15),
            "Нет аудиопакетов. {PERMISSION}"
        );
        if checked.elapsed() >= Duration::from_secs(1) {
            ensure!(
                default_device(channel)? == default,
                "Сменилось устройство по умолчанию"
            );
            ensure!(
                property::<u32>(stream.device, kAudioDevicePropertyDeviceIsAlive, GLOBAL)? != 0,
                "Устройство отключено"
            );
            let current: AudioStreamBasicDescription = if channel == Channel::System {
                property(stream.tap, kAudioTapPropertyFormat, GLOBAL)?
            } else {
                property(device, kAudioDevicePropertyStreamFormat, INPUT)?
            };
            ensure!(current == format, "Изменился формат устройства");
            if channel == Channel::Mic {
                unsafe {
                    ensure!(
                        AVCaptureDevice::authorizationStatusForMediaType(
                            AVMediaTypeAudio.context("Нет AVMediaTypeAudio")?
                        ) == AVAuthorizationStatus::Authorized,
                        "Отозван доступ к микрофону. {PERMISSION}"
                    );
                }
            }
            checked = Instant::now();
        }
    }
    // stop() joins before the recorder drains its queue; never block on the tail.
    pending.extend(normalizer.finish()?);
    if let Some(start) = start {
        while pending.len() >= FRAME_SAMPLES {
            let frame = AudioFrame {
                channel,
                samples: pending.drain(..FRAME_SAMPLES).collect(),
                captured_at: start + sample_offset(produced),
            };
            produced += FRAME_SAMPLES as u64;
            if sender.try_send(CaptureMessage::Frame(frame)).is_err() {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct Planes {
        count: u32,
        buffers: [AudioBuffer; 2],
    }

    #[test]
    fn callback_interleaves_planes_and_reports_queue_overflow() {
        let left = [0.1_f32, 0.2];
        let right = [0.3_f32, 0.4];
        let planes = Planes {
            count: 2,
            buffers: [left.as_ptr(), right.as_ptr()].map(|data| AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: 8,
                mData: data.cast_mut().cast(),
            }),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = Callback {
            sender,
            failed: Arc::clone(&failed),
            channels: 2,
        };
        // AudioTimeStamp contains only numeric fields; all-zero means no flags.
        let mut time: AudioTimeStamp = unsafe { std::mem::zeroed() };
        time.mHostTime = 123;
        time.mFlags = AudioTimeStampFlags::HostTimeValid;
        let data = NonNull::from(&planes).cast::<AudioBufferList>();
        let context = ptr::from_mut(&mut callback).cast();
        unsafe {
            input(
                0,
                NonNull::from(&time),
                data,
                NonNull::from(&time),
                data,
                NonNull::from(&time),
                context,
            );
        }
        let packet = receiver.try_recv().unwrap();
        assert_eq!(&packet.samples[..packet.count], &[0.1, 0.3, 0.2, 0.4]);
        assert_eq!(packet.host, 123);
        assert!(!failed.load(Ordering::Relaxed));
        for _ in 0..2 {
            unsafe {
                input(
                    0,
                    NonNull::from(&time),
                    data,
                    NonNull::from(&time),
                    data,
                    NonNull::from(&time),
                    context,
                );
            }
        }
        assert!(failed.load(Ordering::Relaxed));
    }
}
