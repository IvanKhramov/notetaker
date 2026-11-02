#!/usr/bin/env python3
"""Render actual egui screens with fixtures, no app worker, audio, or user config."""
from pathlib import Path
import os
import re
import subprocess

root = Path(__file__).resolve().parent.parent
harness = root / "target/preview-harness"
output = root / "target/onboarding-previews"
(harness / "src").mkdir(parents=True, exist_ok=True)
output.mkdir(parents=True, exist_ok=True)
manifest = (root / "crates/app/Cargo.toml").read_text()
manifest = manifest.replace('name = "notetaker-app"', 'name = "onboarding-preview"')
version = re.search(r'\[workspace.package\]\nversion = "([^"]+)"', (root / "Cargo.toml").read_text())[1]
for key, value in [("version", f'"{version}"'), ("edition", '"2024"'), ("license", '"MIT OR Apache-2.0"')]:
    manifest = manifest.replace(f"{key}.workspace = true", f"{key} = {value}")
manifest = manifest.replace("[lints]\nworkspace = true", "[workspace]")
manifest = re.sub(r'path = "\.\./([^"]+)"', lambda m: f'path = "{root / "crates" / m[1]}"', manifest)
(harness / "Cargo.toml").write_text(manifest)
(harness / "Cargo.lock").write_text((root / "Cargo.lock").read_text())
modules = []
for path in sorted((root / "crates/app/src").glob("*.rs")):
    if path.stem in {"main", "desktop", "macos", "updates", "updates_macos"}:
        continue
    text = path.read_text()
    if path.stem == "state":
        text = text.replace("    fn with_channels(", "    pub(crate) fn with_channels(")
        text = text.replace("    fn new(data: DocumentData)", "    pub(crate) fn new(data: DocumentData)")
    if path.stem == "quickstart":
        text = text.replace("    step: usize,", "    pub(crate) step: usize,\n    pub(crate) expanded: bool,")
        text = text.replace('egui::CollapsingHeader::new("Календарь")', 'egui::CollapsingHeader::new("Календарь").default_open(self.expanded)')
        text = text.replace('egui::CollapsingHeader::new("AI для конспектов")', 'egui::CollapsingHeader::new("AI для конспектов").default_open(self.expanded)')
    if path.stem == "ui":
        text = text.replace("    pub fn new() -> Self", "    pub fn new(state: AppState) -> Self")
        text = text.replace("state: AppState::new(),", "state,")
        text += '''
impl NotetakerApp {
    pub fn fixture(state: AppState, screen: usize) -> Self {
        let mut app = Self::new(state);
        app.home = screen != 4 && screen != 5 && screen != 12;
        if screen < 3 { app.quickstart.open = true; app.quickstart.step = screen; }
        if screen == 13 { app.quickstart.open = true; app.quickstart.step = 2; app.quickstart.expanded = true; }
        if (6..12).contains(&screen) {
            app.show_settings = true;
            app.settings_panel.section = crate::settings::Section::ALL[screen - 6];
        }
        if screen == 5 { app.state.tab = Tab::Transcript; }
        app
    }
    pub fn pill(ui: &mut egui::Ui) { recording_pill(ui, Duration::from_secs(123)); }
}
'''
    if path.stem in {"ui", "state", "quickstart"}:
        path = harness / "src" / path.name
        path.write_text(text)
    modules.append(f'#[path = "{path}"] mod {path.stem};')
main = r'''
#![allow(dead_code)]
MODULES
use eframe::egui;
struct Preview { app: ui::NotetakerApp, frames: usize, output: std::path::PathBuf, screen: usize }
impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Use the real UI, but never call its logic/worker polling.
        self.app.ui(ui, frame);
        if self.screen == 12 {
            egui::Window::new("Запись").anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -100.0]).title_bar(false)
                .show(ui.ctx(), ui::NotetakerApp::pill);
        }
        self.frames += 1;
        if self.frames == 10 {
            let output = self.output.clone(); let context = ui.ctx().clone();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                let bytes: Vec<_> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
                image::save_buffer(&output, &bytes, image.size[0] as u32, image.size[1] as u32, image::ColorType::Rgba8).unwrap();
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            })));
        }
        ui.ctx().request_repaint();
    }
}
fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let screen: usize = args[1].parse().unwrap();
    let dark = args[2] == "dark"; let output = args[3].clone();
    let width: f32 = args[4].parse().unwrap(); let height: f32 = args[5].parse().unwrap();
    let zoom: f32 = args[6].parse().unwrap();
    eframe::run_native("Preview", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([width, height]), ..Default::default()
    }, Box::new(move |cc| {
        theme::install(&cc.egui_ctx);
        if dark { theme::Theme::Dark } else { theme::Theme::Light }.apply(&cc.egui_ctx);
        cc.egui_ctx.set_zoom_factor(zoom);
        let (tx, _) = std::sync::mpsc::channel(); let (_, rx) = std::sync::mpsc::channel();
        let mut state = state::AppState::with_channels(tx, rx);
        state.ready = true;
        state.settings.user_name = "Иван".into();
        state.settings.ui_zoom = zoom;
        state.settings.library = std::env::current_dir().unwrap().join("target/preview-harness/fixture/library");
        state.settings.models_folder = Some(state.settings.library.with_file_name("models"));
        state.settings_draft = state.settings.clone();
        state.quickstart.models = quickstart::Models { gigaam: quickstart::Status::Ready, parakeet: quickstart::Status::Ready,
            auto: quickstart::Status::Ready, diarization: quickstart::Status::Ready, semantic: quickstart::Status::Ready };
        if screen == 0 { state.downloading = true; state.quickstart.models = Default::default(); }
        let library = notetaker_core::Library::open(&state.settings.library).unwrap();
        for title in ["Планирование недели", "Обсуждение нового интерфейса", "Итоги встречи с командой"] {
            let meta = notetaker_core::MeetingMeta::new(title, chrono::Utc::now());
            let meeting = library.create_meeting(meta.clone()).unwrap();
            let mut data = worker::DocumentData::read(meeting.clone()).unwrap();
            data.notes = "## Решения\nУпростить первый запуск. Сделать текст крупнее.\n\n## Дальше\nПроверить запись и календарь на macOS и Windows.".into();
            data.enhanced = format!("{}\n\n## Контекст\nКоманда обсудила новый интерфейс заметок. Настройка не должна мешать первой встрече. Модели загружаются автоматически.\n\n## Задачи\n- [ ] Проверить маленькое окно\n- [ ] Подготовить следующий выпуск", data.notes);
            for (index, text) in ["Первый запуск должен быть коротким. Давайте оставим только самое нужное.",
                "Согласен. Запись должна работать без выбора моделей. Остальное можно подключить позже.",
                "Сделаем основной текст крупнее и проверим маленькое окно.",
                "Календарь и AI оставим необязательными. Это поможет быстрее начать первую встречу."].into_iter().enumerate() {
                data.transcript.segments.push(notetaker_core::Segment { id: uuid::Uuid::now_v7(), speaker: notetaker_core::SpeakerId(if index % 2 == 0 { "me" } else { "speaker_1" }.into()),
                    channel: if index % 2 == 0 { notetaker_core::Channel::Mic } else { notetaker_core::Channel::System },
                    start_ms: index as u64 * 10000, end_ms: index as u64 * 10000 + 9000, text: text.into(),
                    source: notetaker_core::Source::Final, edited: false, lang: Some("ru".into()) });
            }
            data.transcript.speaker_names.insert("speaker_1".into(), "Алексей".into());
            data.transcript.self_speaker = Some("me".into());
            state.rows.push(worker::MeetingRow { meeting: meeting.clone(), meta, snippet: String::new(), anchor: None });
            state.selected = Some(meeting.path().to_owned());
            state.documents.insert(meeting.path().to_owned(), state::Document::new(data));
        }
        state.connections.load(&state.llm);
        Ok(Box::new(Preview { app: ui::NotetakerApp::fixture(state, screen), frames: 0, output: output.into(), screen }))
    }))
}
'''.replace("MODULES", "\n".join(modules))
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
names = ["welcome", "name", "connections", "home", "note", "transcript", "general", "recording", "calendars", "ai", "search", "storage", "recording-pill", "connections-expanded"]
for width, height, zoom in [(1280, 800, 1.0), (960, 640, 1.0), (960, 640, 1.5)]:
    for index, name in enumerate(names):
        for theme in ("light", "dark"):
            target = output / f"{name}-{theme}-{width}x{height}-{int(zoom*100)}.png"
            subprocess.run(["xvfb-run", "-a", str(root / "target/debug/onboarding-preview"), str(index), theme, str(target), str(width), str(height), str(zoom)], cwd=root, env=env, check=True, timeout=30)
            print(target, flush=True)

# A local, self-contained gallery. The inline viewer uses these same PNG files.
options = "".join(f'<option value="{name}">{name}</option>' for name in names)
images = "".join(f'<img data-key="{name}-{theme}-{size}-{zoom}" src="{output}/{name}-{theme}-{size}-{zoom}.png" alt="{name}, {theme}, {size}, {zoom}%" hidden>'
    for name in names for theme in ("light", "dark") for size, zoom in (("1280x800", 100), ("960x640", 100), ("960x640", 150)))
html = '<!doctype html><html><head><meta charset="utf-8"><style>:root{font-family:var(--font-sans, sans-serif)}[hidden]{display:none}img{width:100%;height:auto;border-radius:var(--radius, 8px)}nav{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px}select{font:inherit;color:var(--foreground);background:var(--card);border:1px solid var(--border);border-radius:6px;padding:6px}</style></head><body><nav><select id="screen">' + options + '</select><select id="theme"><option>light</option><option>dark</option></select><select id="size"><option>1280x800-100</option><option>960x640-100</option><option>960x640-150</option></select></nav>' + images + '<script>function show(){const key=screenSelect.value+"-"+themeSelect.value+"-"+sizeSelect.value;document.querySelectorAll("img").forEach(img=>img.hidden=img.dataset.key!==key)}const screenSelect=document.getElementById("screen"),themeSelect=document.getElementById("theme"),sizeSelect=document.getElementById("size");document.querySelectorAll("select").forEach(s=>s.onchange=show);show();</script></body></html>'
(output / "index.html").write_text(html)
