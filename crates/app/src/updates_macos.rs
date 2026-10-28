//! macOS updates are manual DMG replacements. Never install or quit automatically.
use eframe::egui;
use notetaker_runtime::update::Release;
use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

pub struct Updates {
    next_check: Instant,
    receiver: Option<Receiver<Option<Release>>>,
    release: Option<Release>,
}
impl Default for Updates {
    fn default() -> Self {
        Self {
            next_check: Instant::now(),
            receiver: None,
            release: None,
        }
    }
}
impl Updates {
    pub fn tick(&mut self, ctx: &egui::Context, enabled: bool, _busy: bool) -> bool {
        if !enabled {
            self.release = None;
        }
        if enabled && self.receiver.is_none() && Instant::now() >= self.next_check {
            self.next_check = Instant::now() + Duration::from_secs(24 * 60 * 60);
            let (tx, rx) = mpsc::channel();
            self.receiver = Some(rx);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let release = (|| -> anyhow::Result<Option<Release>> {
                    let client = reqwest::blocking::Client::builder()
                        .https_only(true)
                        .timeout(Duration::from_secs(30))
                        .user_agent(concat!("notetaker/", env!("CARGO_PKG_VERSION")))
                        .build()?;
                    let release: Release = client
                        .get("https://api.github.com/repos/IvanKhramov/notetaker/releases/latest")
                        .send()?
                        .error_for_status()?
                        .json()?;
                    Ok(release
                        .newer_than(env!("CARGO_PKG_VERSION"))?
                        .then_some(release))
                })()
                .ok()
                .flatten();
                let _ = tx.send(release);
                ctx.request_repaint();
            });
        }
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(release) => {
                    self.receiver = None;
                    if enabled {
                        self.release = release;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        false
    }
    pub fn ui(&mut self, ctx: &egui::Context, _busy: bool) {
        let Some(release) = &self.release else {
            return;
        };
        let mut later = false;
        crate::theme::window(ctx, format!("Доступен notetaker {}", release.tag_name)).collapsible(false).show(ctx, |ui| {
            ui.label("Скачайте DMG, завершите запись и закройте notetaker. Замените приложение в Applications.");
            if ui.button("Открыть релиз / скачать DMG").clicked() {
                let _ = notetaker_runtime::unix::open_url(&release.notes_url());
            }
            later = ui.button("Позже").clicked();
        });
        if later {
            self.release = None;
        }
    }
}
