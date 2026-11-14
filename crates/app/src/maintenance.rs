//! Settings/notifications for desktop distribution, kept separate from meetings.
use eframe::egui;
use notetaker_runtime::{diagnostics, report};
use std::{
    path::Path,
    sync::mpsc::{self, Receiver},
};

pub struct Maintenance {
    #[cfg(any(windows, unix))]
    native_requests: Option<Receiver<(bool, bool)>>,
    crashed: bool,
    message: Option<String>,
    report: Option<Receiver<anyhow::Result<std::path::PathBuf>>>,
    was_busy: bool,
    #[cfg(any(windows, target_os = "macos"))]
    autostart: bool,
    #[cfg(any(windows, target_os = "macos"))]
    updates: crate::updates::Updates,
}
impl Default for Maintenance {
    fn default() -> Self {
        let message = diagnostics::directory().and_then(|logs| {
            let flag = logs.join("update-failed.flag");
            if flag.is_file() {
                let failure = std::fs::read_to_string(&flag).unwrap_or_default();
                let code = failure
                    .trim()
                    .strip_prefix("code=")
                    .and_then(|code| code.parse::<i32>().ok());
                let reason = match code {
                    Some(7) => "Не удалось закрыть работающее приложение. Завершите запись и чистовой проход, закройте приложение и повторите установку.",
                    Some(2 | 5) => "Установка отменена. Повторите установку вручную со страницы релиза.",
                    _ if failure.starts_with("spawn_error=") => "Не удалось запустить установщик. Повторите установку вручную со страницы релиза.",
                    _ => "Повторите установку вручную со страницы релиза.",
                };
                let code = code.map(|code| format!(" Код установщика: {code}.")).unwrap_or_default();
                let _ = std::fs::remove_file(flag);
                Some(format!("Не удалось установить обновление.{code} {reason}\nЛог установщика: {}", logs.join("installer-update.log").display()))
            } else { None }
        });
        Self {
            #[cfg(any(windows, unix))]
            native_requests: None,
            crashed: diagnostics::previous_crash(),
            message,
            report: None,
            was_busy: false,
            #[cfg(any(windows, target_os = "macos"))]
            autostart: notetaker_runtime::platform::autostart(),
            #[cfg(any(windows, target_os = "macos"))]
            updates: Default::default(),
        }
    }
}
impl Maintenance {
    pub fn tick(&mut self, ctx: &egui::Context, enabled: bool, busy: bool) -> bool {
        if self.was_busy != busy {
            diagnostics::event(if busy {
                "app task started"
            } else {
                "app task finished"
            });
            self.was_busy = busy;
        }
        if let Some(receiver) = &self.report
            && let Ok(result) = receiver.try_recv()
        {
            self.report = None;
            self.message = Some(match result {
                Ok(path) => {
                    let _ = reveal(&path);
                    format!("Отчёт сохранён: {}", path.display())
                }
                Err(_) => {
                    "Не удалось собрать отчёт. Проверьте свободное место и доступ к рабочему столу."
                        .into()
                }
            });
        }
        #[cfg(any(windows, unix))]
        {
            let requests = self.native_requests.get_or_insert_with(|| {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    loop {
                        let quit = notetaker_runtime::platform::Instance::quit_requested();
                        let show = notetaker_runtime::platform::Instance::show_requested();
                        if quit || show {
                            if tx.send((quit, show)).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });
                rx
            });
            let (quit, show) = requests
                .try_iter()
                .fold((false, false), |(q, s), (quit, show)| {
                    (q || quit, s || show)
                });
            if quit {
                if busy {
                    notetaker_runtime::platform::Instance::refuse_quit();
                    self.message = Some("Перед установкой или удалением завершите запись и чистовой проход, затем повторите попытку.".into());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                } else {
                    return true;
                }
            }
            if show {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.updates.tick(ctx, enabled, busy)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (ctx, enabled);
            false
        }
    }
    /// Starts an update check now; its result appears as the update prompt or a message.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn check_updates_now(&mut self) {
        self.updates.check_now();
    }
    #[cfg(any(windows, target_os = "macos"))]
    pub fn updates_checking(&self) -> bool {
        self.updates.checking()
    }
    /// "Проверить обновления" button with its in-progress state; true when clicked.
    pub fn check_updates_button(&mut self, ui: &mut egui::Ui) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let checking = self.updates_checking();
            let label = if checking {
                "Проверяю…"
            } else {
                "Проверить обновления"
            };
            if ui
                .add_enabled(
                    !checking,
                    crate::theme::styled_button(
                        crate::theme::Palette::of(ui),
                        label,
                        crate::theme::ButtonStyle::Secondary,
                    ),
                )
                .clicked()
            {
                self.check_updates_now();
                return true;
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = ui;
        false
    }
    pub fn dialogs(&mut self, ctx: &egui::Context, busy: bool) {
        if self.crashed {
            let close = crate::theme::modal(ctx, "Прошлый запуск завершился ошибкой")
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Диагностика сохранена в папке логов.");
                    if crate::theme::button(
                        ui,
                        "Открыть папку логов",
                        crate::theme::ButtonStyle::Ghost,
                    )
                    .clicked()
                    {
                        self.open_logs();
                    }
                    if crate::theme::button(ui, "Закрыть", crate::theme::ButtonStyle::Ghost)
                        .clicked()
                    {
                        self.crashed = false;
                        diagnostics::acknowledge_crash();
                    }
                });
            if close {
                self.crashed = false;
                diagnostics::acknowledge_crash();
            }
        }
        if let Some(message) = self.message.clone() {
            let close = crate::theme::modal(ctx, "Диагностика")
                .default_width(460.0)
                .show(ctx, |ui| {
                    ui.label(message);
                    if crate::theme::button(ui, "Закрыть", crate::theme::ButtonStyle::Ghost)
                        .clicked()
                    {
                        self.message = None;
                    }
                });
            if close {
                self.message = None;
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        self.updates.ui(ctx, busy);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = busy;
    }
    pub fn settings(&mut self, ui: &mut egui::Ui, settings: &mut crate::config::Settings) {
        ui.add_space(16.0);
        crate::theme::section_caption(ui, "Диагностика");
        crate::theme::setting_row(ui, "Папка логов", |ui| {
            if crate::theme::button(ui, "Открыть ›", crate::theme::ButtonStyle::Ghost).clicked()
            {
                self.open_logs();
            }
        });
        let collect =
            crate::theme::setting_row(ui, "Диагностический отчёт", |ui| {
                ui.add_enabled(
                    self.report.is_none(),
                    crate::theme::styled_button(
                        crate::theme::Palette::of(ui),
                        "Собрать ›",
                        crate::theme::ButtonStyle::Ghost,
                    ),
                )
                .clicked()
            });
        if collect {
            // Serialize only app settings; LLM credentials/config never enter here.
            let config = toml::Value::try_from(settings.clone())
                .unwrap_or_else(|_| toml::Value::Table(Default::default()));
            let context = ui.ctx().clone();
            let (tx, rx) = mpsc::channel();
            self.report = Some(rx);
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<_> {
                    let home = directories::UserDirs::new()
                        .ok_or_else(|| anyhow::anyhow!("Нет домашнего каталога"))?;
                    report::collect(&config, home.desktop_dir().unwrap_or(home.home_dir()))
                })();
                let _ = tx.send(result);
                context.request_repaint();
            });
        }
        ui.small("Отчёт содержит версии, логи и общие настройки. Тексты встреч, ключи, адреса календарей и пути библиотеки исключены.");
    }
    fn open_logs(&mut self) {
        if let Some(path) = diagnostics::directory() {
            let result = std::fs::create_dir_all(&path)
                .map_err(anyhow::Error::from)
                .and_then(|_| open(&path));
            if result.is_err() {
                self.message = Some("Не удалось открыть папку логов.".into());
            }
        }
    }
    pub fn windows_settings(&mut self, ui: &mut egui::Ui) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            if crate::theme::toggle(
                ui,
                &mut self.autostart,
                if cfg!(target_os = "macos") {
                    "Запускать при входе в macOS"
                } else {
                    "Запускать при входе в Windows"
                },
            )
            .changed()
                && notetaker_runtime::platform::set_autostart(self.autostart).is_err()
            {
                self.autostart = notetaker_runtime::platform::autostart();
                self.message = Some("Не удалось изменить автозапуск.".into());
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let mut disabled = false;
            ui.add_enabled_ui(false, |ui| {
                crate::theme::toggle(ui, &mut disabled, "Запускать при входе в систему");
            });
        }
    }
}

fn open(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        notetaker_runtime::windows::open(path)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
        .arg(path)
        .spawn()?;
        Ok(())
    }
}
fn reveal(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        notetaker_runtime::windows::reveal(path)
    }
    #[cfg(not(windows))]
    {
        open(path.parent().unwrap_or(path))
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HelperStartup {
    Pending,
    Ready,
    Failed,
}

#[cfg(any(windows, test))]
pub(crate) fn helper_startup(
    ready: bool,
    running: bool,
    elapsed: std::time::Duration,
) -> HelperStartup {
    // A ready file from an exited helper is not a live handoff acknowledgement.
    if !running {
        HelperStartup::Failed
    } else if ready {
        HelperStartup::Ready
    } else if elapsed > std::time::Duration::from_secs(15) {
        HelperStartup::Failed
    } else {
        HelperStartup::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::{HelperStartup, helper_startup};
    use std::time::Duration;

    #[test]
    fn ready_helper_wins_over_startup_timeout() {
        assert_eq!(
            helper_startup(true, true, Duration::from_secs(30)),
            HelperStartup::Ready
        );
    }

    #[test]
    fn startup_timeout_only_applies_to_unready_running_helpers() {
        assert_eq!(
            helper_startup(false, true, Duration::from_secs(15)),
            HelperStartup::Pending
        );
        assert_eq!(
            helper_startup(false, true, Duration::from_secs(16)),
            HelperStartup::Failed
        );
    }

    #[test]
    fn exited_helper_is_rejected_even_with_a_ready_file() {
        assert_eq!(
            helper_startup(true, false, Duration::from_secs(1)),
            HelperStartup::Failed
        );
        assert_eq!(
            helper_startup(false, false, Duration::ZERO),
            HelperStartup::Failed
        );
    }
}
