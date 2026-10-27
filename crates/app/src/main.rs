#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai;
mod attachments;
mod chat;
mod config;
#[cfg(windows)]
mod desktop;
mod detection;
mod icon;
mod live;
mod maintenance;
mod people;
mod quickstart;
mod speakers;
mod state;
mod theme;
mod ui;
#[cfg(windows)]
mod updates;
mod worker;

fn main() -> eframe::Result {
    #[cfg(windows)]
    if std::env::args_os().skip(1).any(|arg| arg == "--quit") {
        std::process::exit(
            if notetaker_runtime::windows::Instance::request_quit().is_ok() {
                0
            } else {
                2
            },
        );
    }
    #[cfg(windows)]
    let _instance = match notetaker_runtime::windows::Instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            // Do not silently start a second copy if the synchronization fails.
            return Err(eframe::Error::AppCreation(error.into()));
        }
    };
    let _ = notetaker_runtime::diagnostics::init();
    #[cfg(windows)]
    let _ = notetaker_runtime::windows::capture_app_stderr();
    #[cfg(windows)]
    updates::cleanup_finished();
    let result = eframe::run_native(
        "notetaker",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1120.0, 760.0])
                .with_icon(icon::window())
                .with_min_inner_size([720.0, 480.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(ui::NotetakerApp::new()))
        }),
    );
    notetaker_runtime::diagnostics::event(if result.is_ok() {
        "app exit"
    } else {
        "app exit error"
    });
    result
}
