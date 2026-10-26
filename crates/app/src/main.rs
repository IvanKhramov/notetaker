//! Desktop application entry point.

fn main() -> eframe::Result {
    eframe::run_native(
        "notetaker",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(NotetakerApp))),
    )
}

struct NotetakerApp;

impl eframe::App for NotetakerApp {
    fn ui(&mut self, _ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {}
}
