#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai;
mod attachments;
mod chat;
mod config;
#[cfg(windows)]
mod desktop;
mod detection;
mod state;
mod theme;
mod ui;
mod worker;

fn main() -> eframe::Result {
    eframe::run_native(
        "notetaker",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1120.0, 760.0])
                .with_min_inner_size([720.0, 480.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(ui::NotetakerApp::new()))
        }),
    )
}
