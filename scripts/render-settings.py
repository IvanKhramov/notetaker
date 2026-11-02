#!/usr/bin/env python3
"""Render the real egui settings with isolated fixtures and no worker thread."""
from pathlib import Path
import os
import re
import subprocess

root = Path(__file__).resolve().parent.parent
harness = root / "target/preview-harness"
output = root / "target/settings-previews"
(harness / "src").mkdir(parents=True, exist_ok=True)
output.mkdir(parents=True, exist_ok=True)
manifest = (root / "crates/app/Cargo.toml").read_text()
manifest = manifest.replace('name = "notetaker-app"', 'name = "settings-preview"')
for key, value in [("version", '"0.16.0"'), ("edition", '"2024"'), ("license", '"MIT OR Apache-2.0"')]:
    manifest = manifest.replace(f"{key}.workspace = true", f"{key} = {value}")
manifest = manifest.replace("[lints]\nworkspace = true", "[workspace]")
manifest = re.sub(r'path = "\.\./([^"]+)"', lambda match: f'path = "{root / "crates" / match[1]}"', manifest)
(harness / "Cargo.toml").write_text(manifest)
# Use the workspace lockfile so preview dependencies match the application.
(harness / "Cargo.lock").write_text((root / "Cargo.lock").read_text())
source = root / "crates/app/src"
modules = []
for path in sorted(source.glob("*.rs")):
    if path.stem in {"main", "desktop", "macos", "updates", "updates_macos"}:
        continue
    if path.stem == "state":
        state = path.read_text().replace("    fn with_channels(", "    pub(crate) fn with_channels(")
        path = harness / "src/state.rs"
        path.write_text(state)
    modules.append(f'#[path = "{path}"] mod {path.stem};')
main = r'''
#![allow(dead_code)]
MODULES
use eframe::egui;
struct Preview {
    state: state::AppState,
    panel: settings::Panel,
    maintenance: maintenance::Maintenance,
    organization: organization::Panel,
    frames: usize,
    output: std::path::PathBuf,
}
impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| { ui.label("notetaker"); });
        self.panel.show(&ctx, &mut true, &mut self.state, &mut self.maintenance, &mut self.organization);
        self.frames += 1;
        if self.frames == 6 {
            let output = self.output.clone();
            let context = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                let bytes: Vec<_> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                image::save_buffer(&output, &bytes, image.size[0] as u32, image.size[1] as u32, image::ColorType::Rgba8).unwrap();
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            })));
        }
        ctx.request_repaint();
    }
}
fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let index: usize = args[1].parse().unwrap();
    let dark = args[2] == "dark";
    let output = args[3].clone();
    eframe::run_native("Settings preview", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1020.0, 760.0]),
        ..Default::default()
    }, Box::new(move |cc| {
        theme::install(&cc.egui_ctx);
        if dark { theme::Theme::Dark } else { theme::Theme::Light }.apply(&cc.egui_ctx);
        let (tx, _) = std::sync::mpsc::channel();
        let (_, rx) = std::sync::mpsc::channel();
        let mut state = state::AppState::with_channels(tx, rx);
        state.ready = true;
        state.settings.user_name = "Иван".into();
        state.settings.calendars.push(notetaker_llm::calendar::Subscription { id: "calendar-preview".into(), name: "Рабочий".into() });
        state.settings.library = std::env::current_dir().unwrap().join("target/preview-harness/fixture/library");
        state.settings_draft = state.settings.clone();
        Ok(Box::new(Preview { state, panel: settings::Panel { section: settings::Section::ALL[index], ..Default::default() }, maintenance: Default::default(), organization: Default::default(), frames: 0, output: output.into() }))
    }))
}
'''.replace("MODULES", "\n".join(modules))
# Panel private input maps remain private in the application.
main = main.replace('settings::Panel { section: settings::Section::ALL[index], ..Default::default() }', '{ let mut panel = settings::Panel::default(); panel.section = settings::Section::ALL[index]; panel }')
(harness / "src/main.rs").write_text(main)
env = os.environ.copy()
env["CARGO_TARGET_DIR"] = str(root / "target")
for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
    directory = harness / "fixture" / key.lower()
    directory.mkdir(parents=True, exist_ok=True)
    env[key] = str(directory)
env["LIBGL_ALWAYS_SOFTWARE"] = "1"
env["LD_LIBRARY_PATH"] = str(harness / "libs/usr/lib/x86_64-linux-gnu") + ":" + env.get("LD_LIBRARY_PATH", "")
subprocess.run(["cargo", "build", "--manifest-path", str(harness / "Cargo.toml")], cwd=root, env=env, check=True)
for index, section in enumerate(("general", "recording", "calendars", "ai", "search", "storage")):
    for theme in ("light", "dark"):
        target = output / f"{section}-{theme}.png"
        subprocess.run(["xvfb-run", "-a", str(root / "target/debug/settings-preview"), str(index), theme, str(target)], cwd=root, env=env, check=True, timeout=30)
        print(target)
