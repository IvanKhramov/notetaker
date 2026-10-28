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

use anyhow::{Context, Result, ensure};
use objc2::{AnyThread, runtime::AnyObject};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_audio::*;
use objc2_core_audio_types::*;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use rtrb::{Producer, RingBuffer};

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
fn objects(object: u32, selector: u32, scope: u32) -> Result<Vec<u32>> {
    let address = address(selector, scope);
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

fn stream_formats(device: u32, scope: u32) -> Result<Vec<AudioStreamBasicDescription>> {
    objects(device, kAudioDevicePropertyStreams, scope)?
        .into_iter()
        .map(|stream| property(stream, kAudioStreamPropertyVirtualFormat, GLOBAL))
        .collect()
}

fn input_rate(formats: &[AudioStreamBasicDescription]) -> Result<u32> {
    let first = formats.first().context("Нет входных аудиопотоков")?;
    for (index, format) in formats.iter().enumerate() {
        let width = if format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
            1
        } else {
            format.mChannelsPerFrame
        };
        ensure!(
            format.mFormatID == kAudioFormatLinearPCM
                && format.mFormatFlags & kAudioFormatFlagIsFloat != 0
                && format.mBitsPerChannel == 32
                && format.mFormatFlags & kAudioFormatFlagIsBigEndian == 0
                && format.mChannelsPerFrame > 0
                && format.mBytesPerFrame == width * size_of::<f32>() as u32
                && format.mSampleRate.is_finite()
                && format.mSampleRate >= 1.0
                && format.mSampleRate <= f64::from(u32::MAX)
                && format.mSampleRate.fract() == 0.0,
            "Входной поток {index}: неподдерживаемый формат {format:?}; нужен 32-bit float PCM без padding с целой частотой"
        );
        ensure!(
            format.mSampleRate == first.mSampleRate,
            "Входной поток {index}: частота {} отличается от {}; потоки с разными частотами не поддерживаются",
            format.mSampleRate,
            first.mSampleRate
        );
    }
    Ok(first.mSampleRate as u32)
}

/// Process input activity covers all devices and excludes our own capture.
pub fn microphone_processes() -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    for process in objects(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyProcessObjectList,
        GLOBAL,
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
        GLOBAL,
    )? {
        for (channel, scope) in [
            (Channel::Mic, INPUT),
            (Channel::System, kAudioObjectPropertyScopeOutput),
        ] {
            if !stream_formats(id, scope)
                .is_ok_and(|formats| formats.iter().any(|f| f.mChannelsPerFrame > 0))
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
    producer: Producer<Packet>,
    failed: Arc<AtomicBool>,
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
    // HAL serializes this IOProc and owns the buffer list and its planes.
    // Only this callback accesses the producer until the IOProc is destroyed.
    unsafe {
        let state = &mut *context.cast::<Callback>();
        let list = data.as_ref();
        let buffers =
            std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize);
        // HAL can leave unused streams null. Mix only buffers with input data.
        let active = || {
            buffers
                .iter()
                .filter(|b| !b.mData.is_null() && b.mDataByteSize != 0)
        };
        let Some(first) = active().next() else {
            return 0;
        };
        if active().any(|b| b.mNumberChannels == 0) {
            state.failed.store(true, Ordering::Relaxed);
            return 0;
        }
        let channels: usize = active().map(|b| b.mNumberChannels as usize).sum();
        let frames =
            first.mDataByteSize as usize / (size_of::<f32>() * first.mNumberChannels as usize);
        if frames == 0
            || frames > MAX_SAMPLES
            || active().any(|b| {
                b.mDataByteSize as usize != frames * b.mNumberChannels as usize * size_of::<f32>()
            })
        {
            state.failed.store(true, Ordering::Relaxed);
            return 0;
        }
        let mut packet = Packet {
            samples: [0.0; MAX_SAMPLES],
            count: frames,
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
        for buffer in active() {
            let width = buffer.mNumberChannels as usize;
            let samples = std::slice::from_raw_parts(buffer.mData.cast::<f32>(), frames * width);
            for (mono, frame) in packet.samples[..frames]
                .iter_mut()
                .zip(samples.chunks_exact(width))
            {
                *mono += frame.iter().sum::<f32>() / channels as f32;
            }
        }
        // rtrb only moves into preallocated storage and updates atomic indices.
        if state.producer.push(packet).is_err() {
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
    fn new(device: u32, producer: Producer<Packet>, failed: Arc<AtomicBool>) -> Self {
        Self {
            device,
            tap: 0,
            aggregate: false,
            proc: None,
            callback: Box::new(Callback { producer, failed }),
        }
    }
    fn tap(&mut self) -> Result<AudioStreamBasicDescription> {
        unsafe {
            let processes = objects(
                kAudioObjectSystemObject as u32,
                kAudioHardwarePropertyProcessObjectList,
                GLOBAL,
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
    let (tx, mut rx) = RingBuffer::new(64);
    let failed = Arc::new(AtomicBool::new(false));
    let mut stream = Stream::new(device, tx, Arc::clone(&failed));
    let formats = if channel == Channel::System {
        vec![stream.tap()?]
    } else {
        stream_formats(device, INPUT)?
    };
    let rate = input_rate(&formats)?;
    // The callback mixes every active buffer to mono before queueing.
    let mut normalizer = Normalizer::new(rate, 1)?;
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
        match rx.pop() {
            Ok(packet) => {
                received = Instant::now();
                let length = Duration::from_secs_f64(packet.count as f64 / f64::from(rate));
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
                    normalizer = Normalizer::new(rate, 1)?;
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
            // Do not wake the consumer from the real-time callback: waking a
            // channel or parked thread can take a lock in the OS/runtime.
            Err(rtrb::PopError::Empty) => std::thread::park_timeout(Duration::from_millis(5)),
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
            let current = if channel == Channel::System {
                vec![property(stream.tap, kAudioTapPropertyFormat, GLOBAL)?]
            } else {
                stream_formats(device, INPUT)?
            };
            ensure!(current == formats, "Изменился формат устройства");
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
    struct Planes<const N: usize> {
        count: u32,
        buffers: [AudioBuffer; N],
    }

    #[test]
    fn callback_mixes_planes_and_reports_ring_overflow() {
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
        let (producer, mut consumer) = RingBuffer::new(1);
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = Callback {
            producer,
            failed: Arc::clone(&failed),
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
        let packet = consumer.pop().unwrap();
        assert_eq!(&packet.samples[..packet.count], &[0.2, 0.3]);
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

    fn run_callback<const N: usize>(buffers: [AudioBuffer; N]) -> (Option<Packet>, bool) {
        let planes = Planes {
            count: N as u32,
            buffers,
        };
        let (producer, mut consumer) = RingBuffer::new(1);
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = Callback {
            producer,
            failed: Arc::clone(&failed),
        };
        // SAFETY: a zero timestamp has no valid fields, which input handles.
        let time: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let data = NonNull::from(&planes).cast::<AudioBufferList>();
        unsafe {
            input(
                0,
                NonNull::from(&time),
                data,
                NonNull::from(&time),
                data,
                NonNull::from(&time),
                ptr::from_mut(&mut callback).cast(),
            );
        }
        (consumer.pop().ok(), failed.load(Ordering::Relaxed))
    }

    fn buffer(samples: &[f32], channels: u32) -> AudioBuffer {
        AudioBuffer {
            mNumberChannels: channels,
            mDataByteSize: std::mem::size_of_val(samples) as u32,
            mData: samples.as_ptr().cast_mut().cast(),
        }
    }

    #[test]
    fn callback_mixes_multiple_streams_with_different_channel_counts() {
        let stereo = [0.1, 0.2, 0.3, 0.4];
        let mono = [0.6, 0.8];
        let (packet, failed) = run_callback([buffer(&stereo, 2), buffer(&mono, 1)]);
        assert!(!failed);
        let packet = packet.unwrap();
        assert_eq!(packet.count, 2);
        for (actual, expected) in packet.samples[..2].iter().zip([0.3, 0.5]) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn callback_skips_unused_streams() {
        let samples = [0.2, 0.4];
        let unused = AudioBuffer {
            mNumberChannels: 2,
            mDataByteSize: 16,
            mData: ptr::null_mut(),
        };
        let (packet, failed) = run_callback([unused, buffer(&samples, 1)]);
        assert!(!failed);
        let packet = packet.unwrap();
        assert_eq!(&packet.samples[..packet.count], &samples);
    }

    #[test]
    fn callback_rejects_mismatched_or_invalid_buffer_sizes() {
        let samples = [0.2, 0.4];
        for buffers in [
            [buffer(&samples, 1), buffer(&samples[..1], 1)],
            [buffer(&samples, 0), buffer(&samples, 1)],
            [buffer(&samples[..1], 2), buffer(&samples, 1)],
        ] {
            let (packet, failed) = run_callback(buffers);
            assert!(packet.is_none());
            assert!(failed);
        }
    }

    fn float_format(channels: u32) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription {
            mSampleRate: 48_000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4 * channels,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4 * channels,
            mChannelsPerFrame: channels,
            mBitsPerChannel: 32,
            mReserved: 0,
        }
    }

    #[test]
    fn validates_every_stream_format_and_a_common_sample_rate() {
        let stereo = float_format(2);
        let mono = float_format(1);
        assert_eq!(input_rate(&[stereo, mono]).unwrap(), 48_000);
        let mut planar = stereo;
        planar.mFormatFlags |= kAudioFormatFlagIsNonInterleaved;
        planar.mBytesPerFrame = 4;
        planar.mBytesPerPacket = 4;
        assert_eq!(input_rate(&[planar, mono]).unwrap(), 48_000);
        assert!(input_rate(&[]).is_err());
        let mut unsupported = mono;
        unsupported.mFormatFlags = kAudioFormatFlagIsSignedInteger;
        assert!(input_rate(&[stereo, unsupported]).is_err());
        let mut other_rate = mono;
        other_rate.mSampleRate = 44_100.0;
        assert!(input_rate(&[stereo, other_rate]).is_err());
        let mut padded = stereo;
        padded.mBytesPerFrame = 12;
        assert!(input_rate(&[padded]).is_err());
    }
}
