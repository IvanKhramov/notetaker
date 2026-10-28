#[cfg(any(windows, target_os = "macos"))]
pub fn image(recording: bool) -> image::RgbaImage {
    let bytes: &[u8] = if recording {
        include_bytes!("../assets/notetaker-recording-32.png")
    } else {
        include_bytes!("../assets/notetaker-32.png")
    };
    image::load_from_memory(bytes)
        .expect("embedded app icon")
        .into_rgba8()
}

pub fn window() -> eframe::egui::IconData {
    let image = image::load_from_memory(include_bytes!("../assets/notetaker-256.png"))
        .expect("embedded window icon")
        .into_rgba8();
    eframe::egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}
