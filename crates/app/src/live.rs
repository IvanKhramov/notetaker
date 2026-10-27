//! Recording-only assistance; all provider work stays off the egui thread.
use crate::{
    state::{AppState, Phase},
    worker::Command,
};
use notetaker_core::Bookmark;
use notetaker_llm::live::{LiveSchedule, complete_live, recap_request, summary_request};
use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Finished {
    recap: bool,
    consumed: usize,
    result: Result<String, String>,
}
#[derive(Default)]
pub struct Assist {
    pub schedule: LiveSchedule,
    pub provider: Option<notetaker_llm::ProviderConfig>,
    pub bookmarks_pending: usize,
    pub summary: String,
    pub updated: Option<Instant>,
    pub pending: String,
    pub recap: Option<String>,
    pub error: Option<String>,
    pub label: Option<(uuid::Uuid, Instant, String)>,
    result: Option<Receiver<Finished>>,
}
impl Assist {
    fn poll_result(&mut self, active: bool) {
        if let Some(done) = self.result.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(done) => Some(done),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Finished {
                recap: false,
                consumed: 0,
                result: Err("Поток запроса завершился без ответа".into()),
            }),
        }) {
            self.result = None;
            self.schedule.finish();
            if active {
                match done.result {
                    Ok(text) if done.recap => self.recap = Some(text),
                    Ok(text) => {
                        self.summary = text;
                        self.updated = Some(Instant::now());
                        self.pending.drain(..done.consumed);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
    }
}
impl AppState {
    pub fn poll_live(&mut self) {
        self.recording.assist.poll_result(
            self.recording.phase == Phase::Recording && !self.recording.stop_requested,
        );
        if self
            .recording
            .assist
            .label
            .as_ref()
            .is_some_and(|(_, at, label)| {
                label.trim().is_empty() && at.elapsed() >= Duration::from_secs(5)
            })
        {
            self.recording.assist.label = None;
        }
        if self.recording.phase != Phase::Recording || self.recording.stop_requested {
            self.save_bookmark_label();
            self.recording.assist.schedule.stop();
        } else if self.settings.live_summary {
            self.request_live(false, false);
        }
    }
    pub fn request_live(&mut self, recap: bool, manual: bool) {
        if self.recording.phase != Phase::Recording || self.recording.stop_requested {
            return;
        }
        let Some(doc) = self
            .recording
            .path
            .as_ref()
            .and_then(|p| self.documents.get(p))
        else {
            return;
        };
        let now_ms = self.recording.elapsed().as_millis() as u64;
        let Some(config) = self.recording.assist.provider.clone() else {
            if manual {
                self.recording.assist.error =
                    Some("Настройте провайдер конспекта, разрешённый режимом встречи".into());
            }
            return;
        };
        let mode = doc.data.meta.mode;
        let assist = &mut self.recording.assist;
        let has_text = if recap {
            !doc.data.transcript.segments.is_empty()
        } else {
            !assist.pending.trim().is_empty()
        };
        if !has_text && manual {
            assist.error = Some("Пока нет новых строк транскрипта".into());
        }
        let Some(cancel) = assist.schedule.begin(
            now_ms,
            self.settings.live_summary_minutes,
            has_text,
            true,
            manual,
        ) else {
            return;
        };
        let request = if recap {
            recap_request(
                &doc.data.transcript,
                now_ms,
                self.settings.recap_minutes,
                config.context_tokens,
            )
        } else {
            summary_request(&assist.summary, &assist.pending, config.context_tokens)
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                assist.error = Some(format!("{error:#}"));
                assist.schedule.finish();
                return;
            }
        };
        assist.error = None;
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let provider = notetaker_llm::create_provider(config, mode, cancel.clone())?;
                complete_live(provider.as_ref(), mode, &request, &cancel)
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Finished {
                recap,
                consumed: request.consumed,
                result,
            });
        });
    }
    pub fn add_bookmark(&mut self) {
        self.save_bookmark_label();
        if self.recording.phase != Phase::Recording || self.recording.stop_requested {
            return;
        }
        let Some(doc) = self
            .recording
            .path
            .as_ref()
            .and_then(|p| self.documents.get(p))
        else {
            return;
        };
        let bookmark = Bookmark {
            id: uuid::Uuid::now_v7(),
            at_ms: self.recording.elapsed().as_millis() as u64,
            label: None,
        };
        self.recording.assist.label = Some((bookmark.id, Instant::now(), String::new()));
        self.recording.assist.bookmarks_pending += 1;
        self.send(Command::Bookmark {
            meeting: doc.data.meeting.clone(),
            bookmark,
        });
    }
    pub fn save_bookmark_label(&mut self) {
        let Some((id, _, label)) = self.recording.assist.label.take() else {
            return;
        };
        if label.trim().is_empty() {
            return;
        }
        let Some(doc) = self
            .recording
            .path
            .as_ref()
            .and_then(|p| self.documents.get(p))
        else {
            return;
        };
        self.recording.assist.bookmarks_pending += 1;
        self.send(Command::BookmarkLabel {
            meeting: doc.data.meeting.clone(),
            id,
            label: label.trim().into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_delta_is_committed_only_for_successful_active_summary() {
        let mut assist = Assist {
            pending: "first\nsecond\n".into(),
            ..Default::default()
        };
        assist
            .schedule
            .begin(180_000, 3, true, true, false)
            .unwrap();
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        tx.send(Finished {
            recap: false,
            consumed: 6,
            result: Ok("Кратко".into()),
        })
        .unwrap();
        assist.poll_result(true);
        assert_eq!(assist.pending, "second\n");
        assert_eq!(assist.summary, "Кратко");
        assert!(!assist.schedule.busy());
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        tx.send(Finished {
            recap: true,
            consumed: 1000,
            result: Ok("Обзор".into()),
        })
        .unwrap();
        assist.poll_result(true);
        assert_eq!(assist.pending, "second\n");
        assert_eq!(assist.recap.as_deref(), Some("Обзор"));
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        tx.send(Finished {
            recap: false,
            consumed: 7,
            result: Err("ошибка".into()),
        })
        .unwrap();
        assist.poll_result(true);
        assert_eq!(assist.pending, "second\n");
        assert_eq!(assist.summary, "Кратко");
    }
    #[test]
    fn late_completion_after_stop_is_discarded_and_worker_failure_frees_slot() {
        let mut assist = Assist {
            pending: "delta".into(),
            ..Default::default()
        };
        let cancel = assist.schedule.begin(0, 3, true, true, true).unwrap();
        assist.schedule.stop();
        assert!(cancel.is_cancelled());
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        tx.send(Finished {
            recap: false,
            consumed: 5,
            result: Ok("late".into()),
        })
        .unwrap();
        assist.poll_result(false);
        assert!(assist.summary.is_empty());
        assert_eq!(assist.pending, "delta");
        assert!(!assist.schedule.busy());
        assist.schedule.begin(0, 3, true, true, true).unwrap();
        let (tx, rx) = mpsc::channel();
        assist.result = Some(rx);
        drop(tx);
        assist.poll_result(true);
        assert!(!assist.schedule.busy());
        assert!(assist.error.is_some());
    }
}
