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

/// Retry each interruption on the default device, but stop if that attempt
/// cannot reach a healthy drain. A later device change gets its own retry.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn recover_default(
    mut run: impl FnMut(bool, &mut bool) -> Result<()>,
    mut interrupted: impl FnMut(&anyhow::Error),
) -> Result<()> {
    let mut use_default = false;
    loop {
        let mut healthy = false;
        match run(use_default, &mut healthy) {
            Err(error) if !use_default || healthy => {
                interrupted(&error);
                use_default = true;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn each_device_change_reopens_but_failed_default_is_not_retried_forever() {
        let mut attempts = Vec::new();
        let mut issues = 0;
        let result = recover_default(
            |default, healthy| {
                attempts.push(default);
                *healthy = attempts.len() < 3;
                Err(anyhow!(if *healthy {
                    "device changed"
                } else {
                    "no default device"
                }))
            },
            |_| issues += 1,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no default device")
        );
        assert_eq!(attempts, [false, true, true]);
        assert_eq!(issues, 2);
    }

    #[test]
    fn initial_open_failure_can_recover_on_default() {
        let mut attempts = 0;
        recover_default(
            |default, _| {
                attempts += 1;
                if default {
                    Ok(())
                } else {
                    Err(anyhow!("selected microphone removed"))
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(attempts, 2);
    }
}
