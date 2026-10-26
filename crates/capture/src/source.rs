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
