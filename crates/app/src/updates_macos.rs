//! macOS updates are manual DMG replacements. Never install or quit automatically.
use eframe::egui;
use notetaker_runtime::update::Release;
use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(60 * 60);

pub struct Updates {
    next_check: Instant,
    receiver: Option<Receiver<anyhow::Result<Option<Release>>>>,
    release: Option<Release>,
    message: Option<String>,
    /// A user-requested check reports every outcome and ignores the setting.
    manual: bool,
    /// "Позже" hides this tag from automatic checks; a manual check shows it again.
    dismissed: Option<String>,
}
impl Default for Updates {
    fn default() -> Self {
        Self {
            next_check: Instant::now(),
            receiver: None,
            release: None,
            message: None,
            manual: false,
            dismissed: None,
        }
    }
}
impl Updates {
    pub fn check_now(&mut self) {
        self.manual = true;
        self.next_check = Instant::now();
    }
    pub fn checking(&self) -> bool {
        self.manual && self.release.is_none()
    }
    pub fn tick(&mut self, ctx: &egui::Context, enabled: bool, _busy: bool) -> bool {
        if !enabled && !self.manual {
            self.release = None;
        }
        if (enabled || self.manual) && self.receiver.is_none() && Instant::now() >= self.next_check
        {
            self.next_check = Instant::now() + INTERVAL;
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
                    Ok(
                        (release.newer_than(env!("CARGO_PKG_VERSION"))? && release.dmg().is_ok())
                            .then_some(release),
                    )
                })();
                let _ = tx.send(release);
                ctx.request_repaint();
            });
        }
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(result) => {
                    self.receiver = None;
                    self.checked(result, enabled);
                }
                Err(mpsc::TryRecvError::Disconnected) => self.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if enabled && self.receiver.is_none() {
            // An idle egui window does not repaint; wake it for the next hourly check.
            ctx.request_repaint_after(self.next_check.saturating_duration_since(Instant::now()));
        }
        false
    }
    fn checked(&mut self, result: anyhow::Result<Option<Release>>, enabled: bool) {
        match result {
            Ok(Some(release))
                if self.manual
                    || (enabled && self.dismissed.as_deref() != Some(&release.tag_name)) =>
            {
                self.release = Some(release);
            }
            Ok(None) if self.manual => {
                self.manual = false;
                self.message = Some(format!(
                    "Установлена последняя версия {}.",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            Err(_) if self.manual => {
                self.manual = false;
                self.message = Some(
                    "Не удалось проверить обновления. Проверьте соединение и повторите.".into(),
                );
            }
            // Automatic checks stay quiet offline, on rate limits and for a dismissed tag.
            _ => {}
        }
    }
    pub fn ui(&mut self, ctx: &egui::Context, _busy: bool) {
        if let Some(message) = self.message.clone() {
            let close = crate::theme::modal(ctx, "Обновление").show(ctx, |ui| {
                ui.label(message);
                if crate::theme::button(ui, "Закрыть", crate::theme::ButtonStyle::Ghost).clicked()
                {
                    self.message = None;
                }
            });
            if close {
                self.message = None;
            }
        }
        let Some(release) = self.release.clone() else {
            return;
        };
        match crate::theme::update_prompt(
            ctx,
            format!("Доступен notetaker {}", release.tag_name),
            "Скачайте DMG, завершите запись и закройте notetaker. Замените приложение в Applications.",
            "Открыть релиз / скачать DMG",
            true,
            true,
            false,
        ) {
            crate::theme::UpdateAction::Install => {
                let _ = notetaker_runtime::unix::open_url(&release.notes_url());
            }
            crate::theme::UpdateAction::Later => {
                self.dismissed = Some(release.tag_name.clone());
                self.release = None;
                self.manual = false;
            }
            _ => {}
        }
    }
}
