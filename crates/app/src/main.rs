#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai;
mod attachments;
mod autosave;
mod chat;
mod config;
#[cfg(any(windows, target_os = "macos"))]
mod desktop;
mod detection;
mod icon;
mod live;
#[cfg(target_os = "macos")]
mod macos;
mod maintenance;
mod organization;
mod people;
mod quickstart;
mod settings;
mod speakers;
mod state;
#[cfg(test)]
mod test_ui;
mod theme;
mod trackers;
mod ui;
#[cfg(windows)]
mod updates;
#[cfg(target_os = "macos")]
#[path = "updates_macos.rs"]
mod updates;
mod virtual_rows;
mod worker;

// Installer self-check. CI runners have no OpenGL, so it must not open a window; it
// also never constructs AppState, workers or the tray, and touches no profile data.
fn smoke_ui(ui: &mut eframe::egui::Ui) {
    ui.label("notetaker installer smoke");
}
/// Lays out one frame with the embedded fonts and theme, then checks that the
/// executables the app starts are installed next to it.
fn smoke_check() -> Result<(), String> {
    let ctx = eframe::egui::Context::default();
    theme::install(&ctx);
    ctx.run_ui(Default::default(), smoke_ui)
        .drop_without_applying_deltas();
    for name in ["notetaker", "notetaker-asr"] {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let path = notetaker_runtime::paths::current_sibling(&file).map_err(|e| e.to_string())?;
        if !path.is_file() {
            return Err(format!("missing {}", path.display()));
        }
    }
    Ok(())
}

fn main() -> eframe::Result {
    if std::env::var_os("NOTETAKER_SMOKE").is_some_and(|v| v == "1") {
        std::process::exit(match smoke_check() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("smoke check failed: {error}");
                3
            }
        });
    }
    #[cfg(any(windows, unix))]
    if std::env::args_os().skip(1).any(|arg| arg == "--quit") {
        std::process::exit(
            if notetaker_runtime::platform::Instance::request_quit().is_ok() {
                0
            } else {
                2
            },
        );
    }
    #[cfg(any(windows, unix))]
    let _instance = match notetaker_runtime::platform::Instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            // Do not silently start a second copy if the synchronization fails.
            return Err(eframe::Error::AppCreation(error.into()));
        }
    };
    let _ = notetaker_runtime::diagnostics::init();
    #[cfg(any(windows, unix))]
    let _ = notetaker_runtime::platform::capture_app_stderr();
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
            #[cfg(target_os = "macos")]
            macos::install_termination_handler(&cc.egui_ctx)?;
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

#[cfg(test)]
mod smoke_tests {
    #[test]
    fn smoke_ui_renders_without_application_state() {
        let ctx = eframe::egui::Context::default();
        let mut output = crate::test_ui::frame(&ctx, vec![], &mut super::smoke_ui);
        // Headless renderer: acknowledge texture uploads without a GPU backend.
        output.textures_delta.clear();
        assert!(
            crate::test_ui::text_rect(&output.shapes, "notetaker installer smoke").width() > 0.0
        );
    }
}
