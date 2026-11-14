use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{CaptureEvent, CaptureMessage, Channel};
use anyhow::{Result, anyhow};

/// The caller owns the bounded channel and drains it until `Stopped` or EOF.
/// `stop` cancels blocked sends, joins the worker and surfaces capture errors.
pub trait AudioSource {
    fn start(&mut self, sender: SyncSender<CaptureMessage>) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

#[derive(Default)]
pub struct SourceTask {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<()>>>,
}

impl SourceTask {
    pub(crate) fn start<F>(
        &mut self,
        channel: Channel,
        sender: SyncSender<CaptureMessage>,
        run: F,
    ) -> Result<()>
    where
        F: FnOnce(&AtomicBool, &SyncSender<CaptureMessage>) -> Result<()> + Send + 'static,
    {
        anyhow::ensure!(
            self.handle.is_none(),
            "source already started; stop it before restarting"
        );
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop);
        self.handle = Some(
            thread::Builder::new()
                .name(format!("capture-{channel:?}"))
                .spawn(move || {
                    let result = run(&stop, &sender);
                    if let Err(error) = &result {
                        send(
                            &sender,
                            &stop,
                            CaptureMessage::Event(CaptureEvent::Error {
                                channel,
                                message: format!("{error:#}"),
                            }),
                        );
                    }
                    send(
                        &sender,
                        &stop,
                        CaptureMessage::Event(CaptureEvent::Stopped { channel }),
                    );
                    result
                })?,
        );
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| anyhow!("capture worker panicked"))??;
        }
        Ok(())
    }
}

impl Drop for SourceTask {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub(crate) fn send(
    sender: &SyncSender<CaptureMessage>,
    stop: &AtomicBool,
    mut message: CaptureMessage,
) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(value)) => {
                message = value;
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// Recovery policy has no platform or clock dependencies.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Default)]
struct Recovery {
    lost: bool,
    failures: u32,
}
#[cfg(any(windows, target_os = "macos", test))]
impl Recovery {
    fn failed(&mut self) -> (bool, Duration) {
        let notify = !self.lost;
        self.lost = true;
        let delay = Duration::from_secs(1 << self.failures.min(5));
        self.failures = self.failures.saturating_add(1);
        (notify, delay)
    }
    fn healthy(&mut self) -> bool {
        let restored = self.lost;
        self.lost = false;
        self.failures = 0;
        restored
    }
}

/// Reopen the microphone on the default endpoint with cancellable backoff.
/// System capture retains its bounded recovery policy.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn recover_default(
    stop: &AtomicBool,
    retry: bool,
    mut run: impl FnMut(bool, &mut dyn FnMut()) -> Result<()>,
    mut notice: impl FnMut(bool),
) -> Result<()> {
    let mut recovery = Recovery::default();
    let mut use_default = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut healthy = false;
        let result = run(use_default, &mut || {
            healthy = true;
            if recovery.healthy() {
                notice(true);
            }
        });
        match result {
            Err(_) if retry || !use_default || healthy => {
                let (notify, delay) = recovery.failed();
                if notify {
                    notice(false);
                }
                use_default = true;
                let started = std::time::Instant::now();
                while started.elapsed() < delay {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn reopened_stream_reports_restoration_once() {
        let mut attempt = 0;
        let mut notices = Vec::new();
        recover_default(
            &AtomicBool::new(false),
            true,
            |default, healthy| {
                attempt += 1;
                if attempt == 1 {
                    assert!(!default);
                    return Err(anyhow!("unplugged"));
                }
                assert!(default);
                healthy();
                healthy();
                Ok(())
            },
            |restored| notices.push(restored),
        )
        .unwrap();
        assert_eq!(attempt, 2);
        assert_eq!(notices, [false, true]);
    }

    #[test]
    fn loss_is_reported_once_backoff_is_bounded_and_restoration_resets_it() {
        let mut state = Recovery::default();
        assert!(!state.healthy());
        assert_eq!(state.failed(), (true, Duration::from_secs(1)));
        assert_eq!(state.failed(), (false, Duration::from_secs(2)));
        for _ in 0..100 {
            assert!(state.failed().1 <= Duration::from_secs(32));
        }
        assert!(state.healthy());
        assert!(!state.healthy());
        assert_eq!(state.failed(), (true, Duration::from_secs(1)));
    }
    #[test]
    fn cancellation_prevents_opening_device() {
        recover_default(
            &AtomicBool::new(true),
            true,
            |_, _| panic!("opened after stop"),
            |_| panic!("notice after stop"),
        )
        .unwrap();
    }
    #[test]
    fn successful_stream_does_not_retry() {
        recover_default(
            &AtomicBool::new(false),
            true,
            |default, healthy| {
                assert!(!default);
                healthy();
                Ok(())
            },
            |_| panic!("unexpected notice"),
        )
        .unwrap();
    }
}
