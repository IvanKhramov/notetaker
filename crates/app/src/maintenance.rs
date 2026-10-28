//! Settings/notifications for desktop distribution, kept separate from meetings.
use eframe::egui;
use notetaker_runtime::{diagnostics, report};
use std::{
    path::Path,
    sync::mpsc::{self, Receiver},
};

pub struct Maintenance {
    crashed: bool,
    message: Option<String>,
    report: Option<Receiver<anyhow::Result<std::path::PathBuf>>>,
    was_busy: bool,
    #[cfg(windows)]
    autostart: bool,
    #[cfg(windows)]
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
            crashed: diagnostics::previous_crash(),
            message,
            report: None,
            was_busy: false,
            #[cfg(windows)]
            autostart: notetaker_runtime::windows::autostart(),
            #[cfg(windows)]
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
        #[cfg(windows)]
        {
            if notetaker_runtime::windows::Instance::quit_requested() {
                if busy {
                    notetaker_runtime::windows::Instance::refuse_quit();
                    self.message = Some("Перед установкой или удалением завершите запись и чистовой проход, затем повторите попытку.".into());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                } else {
                    return true;
                }
            }
            if notetaker_runtime::windows::Instance::show_requested() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            }
            self.updates.tick(ctx, enabled, busy)
        }
        #[cfg(not(windows))]
        {
            let _ = (ctx, enabled);
            false
        }
    }
    pub fn dialogs(&mut self, ctx: &egui::Context, busy: bool) {
        if self.crashed {
            crate::theme::window(ctx, "Прошлый запуск завершился ошибкой")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Диагностика сохранена в папке логов.");
                    if ui.button("Открыть папку логов").clicked() {
                        self.open_logs();
                    }
                    if ui.button("Закрыть").clicked() {
                        self.crashed = false;
                        diagnostics::acknowledge_crash();
                    }
                });
        }
        if let Some(message) = self.message.clone() {
            crate::theme::window(ctx, "Диагностика")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(message);
                    if ui.button("Закрыть").clicked() {
                        self.message = None;
                    }
                });
        }
        #[cfg(windows)]
        self.updates.ui(ctx, busy);
        #[cfg(not(windows))]
        let _ = busy;
    }
    pub fn settings(&mut self, ui: &mut egui::Ui, settings: &mut crate::config::Settings) {
        ui.add_space(16.0);
        crate::theme::section_caption(ui, "Обновления и диагностика");
        ui.add_enabled_ui(cfg!(windows), |ui| {
            ui.checkbox(&mut settings.check_updates, "Проверять обновления");
        });
        self.windows_settings(ui);
        if ui.button("Открыть папку логов").clicked() {
            self.open_logs();
        }
        if ui
            .add_enabled(self.report.is_none(), egui::Button::new("Собрать отчёт"))
            .clicked()
        {
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
        #[cfg(windows)]
        {
            if ui
                .checkbox(&mut self.autostart, "Запускать при входе в Windows")
                .changed()
                && notetaker_runtime::windows::set_autostart(self.autostart).is_err()
            {
                self.autostart = notetaker_runtime::windows::autostart();
                self.message = Some("Не удалось изменить автозапуск.".into());
            }
        }
        #[cfg(not(windows))]
        {
            let mut disabled = false;
            ui.add_enabled(
                false,
                egui::Checkbox::new(&mut disabled, "Запускать при входе в Windows"),
            );
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
