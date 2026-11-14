#!/usr/bin/env python3
"""Render actual egui screens with fixtures, no app worker, audio, or user config."""
from pathlib import Path
import os
import re
import subprocess
import shutil
import time
import argparse
from concurrent.futures import ThreadPoolExecutor

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--screens", nargs="+", help="Render only the named screens")
args = parser.parse_args()
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
(harness / "Cargo.lock").write_text((root / "Cargo.lock").read_text().replace('name = "notetaker-app"', 'name = "onboarding-preview"'))
modules = []
for path in sorted((root / "crates/app/src").glob("*.rs")):
    if path.stem in {"main", "desktop", "macos", "updates", "updates_macos", "icons", "test_ui"}:
        continue
    text = path.read_text()
    if path.stem == "state":
        text = text.replace("    fn with_channels(", "    pub(crate) fn with_channels(")
        text = text.replace("    fn new(data: DocumentData)", "    pub(crate) fn new(data: DocumentData)")
    if path.stem == "quickstart":
        text = text.replace("    step: usize,", "    pub(crate) step: usize,\n    pub(crate) expanded: bool,")
        text = text.replace('crate::theme::disclosure("Календарь")', 'crate::theme::disclosure("Календарь").default_open(self.expanded)')
        text = text.replace('crate::theme::disclosure("AI для конспектов")', 'crate::theme::disclosure("AI для конспектов").default_open(self.expanded)')
    if path.stem == "people":
        text += """
impl Panel {
    pub fn fixture_merge(&mut self) {let people=&self.snapshot.as_ref().unwrap().people;assert!(people.len()>=2);self.pending_merge=Some((people[0].clone(),people[1].clone()));}
    pub fn fixture(&mut self, settings: &crate::config::Settings, screen: usize) {
        let snapshot=Snapshot::read(settings).unwrap();
        self.root=settings.library.clone();
        self.view=Some(match screen {23=>View::Person(String::new()),24=>View::Series(String::new()),25=>View::Digest,_=>View::People});
        if screen==23 {
            let person=snapshot.people.first().cloned().expect("person fixture requires indexed people");
            self.view=Some(View::Person(person.id.clone()));
            self.person=Some(Library::open(&settings.library).unwrap().person_page(person,false).unwrap());
        }
        self.snapshot=Some(snapshot);
    }
}
"""
    if path.stem == "organization":
        text += """
impl Panel {
    pub fn fixture_delete(&mut self){self.delete_dialog=Some("Команда".into());}
    pub fn fixture_select(&mut self){self.edit_folder("Команда");}
    pub fn fixture(&mut self,settings:&crate::config::Settings) {
    self.root=settings.library.clone(); self.snapshot=Some(Snapshot::read(&Library::open(&self.root).unwrap()).unwrap()); self.settings_open=true;
} }
"""
    if path.stem == "maintenance":
        text += 'impl Maintenance { pub fn fixture(&mut self) { self.message=Some("Диагностика сохранена в папке логов. Локальные файлы доступны для проверки.".into()); } }'
    if path.stem == "trackers":
        text += """
impl Panel { pub fn fixture(&mut self,state:&AppState) {
    self.open=true;
    if let Some(task)=state.tasks.first() { self.previews.push(notetaker_llm::trackers::Preview {
        task:task.clone(),config:Default::default(),local:true,endpoint:"https://tracker.example/api/tasks".into(),
        body:serde_json::json!({"title":task.text,"description":"Проверить интерфейс перед выпуском"}),
        context:notetaker_llm::trackers::ContextData {meeting_title:task.meeting_title.clone(),meeting_date:"14.11.2026".into(),meeting_link:String::new(),summary_excerpt:String::new(),owner_email:None},
    }); }
} }
"""
    if path.stem == "theme":
        text=text.replace('"../assets/',f'"{root / "crates/app/assets"}/')
        text=text.replace('#[path = "icons.rs"]',f'#[path = "{root / "crates/app/src/icons.rs"}"]')
        text=text.replace('#[cfg(any(windows, target_os = "macos"))]','')
        text=text.replace('self.default_open,', 'self.default_open || ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::unique("preview-expand-settings")).unwrap_or(false)),')
    if path.stem == "ui":
        text = text.replace("    pub fn new() -> Self", "    pub fn new(state: AppState) -> Self")
        text = text.replace("state: AppState::new(),", "state,")
        expected = 'egui::Popup::menu(&menu_response)'
        assert text.count(expected) == 1, "meeting menu fixture hook changed"
        text = text.replace(expected, expected + '.open(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::unique("fixture-meeting-menu")).unwrap_or(false)) || egui::Popup::is_id_open(ui.ctx(), egui::Popup::menu(&menu_response).get_id()))')
        text += '''
impl NotetakerApp {
    pub fn fixture(state: AppState, screen: usize) -> Self {
        let mut app = Self::new(state);
        app.home = ![4,5,12,14,15].contains(&screen);
        if screen < 3 { app.quickstart.open = true; app.quickstart.step = screen; }
        if screen == 13 { app.quickstart.open = true; app.quickstart.step = 2; app.quickstart.expanded = true; }
        if (6..12).contains(&screen) {
            app.show_settings = true;
            app.settings_panel.section = crate::settings::Section::ALL[screen - 6];
        }
        if screen == 3 {
            app.state.calendar_events.push(notetaker_core::calendar::CalendarEvent {
                link:notetaker_core::calendar::EventLink {uid:"preview-event".into(),title:"Встреча с командой".into(),attendees:Vec::new(),call_url:Some("https://meet.google.com/example".into())},
                start:chrono::Utc::now()+chrono::Duration::minutes(30),end:chrono::Utc::now()+chrono::Duration::hours(1),all_day:false,organizer:None,
            });
        }
        if screen == 14 { app.confirm_delete = true; }
        if screen == 16 { app.state.notices.push((ToastLevel::Success, "Конспект скопирован".into())); }
        if screen == 17 { app.state.rows.clear(); }
        if screen == 18 { app.show_settings = true; }
        if screen == 19 { app.state.recording.phase = Phase::Recording; app.state.recording.levels=[0.65,0.3]; app.state.recording.started=Some(Instant::now()-Duration::from_secs(123)); }
        if screen == 20 { app.chat_page=true; app.state.chat.open(None,String::new()); }
        if screen == 21 { app.state.tasks_open=true; }
        if (22..=25).contains(&screen) { app.people.fixture(&app.state.settings,screen); }
        if screen == 26 { app.organization.fixture(&app.state.settings); }
        if screen == 27 { app.maintenance.fixture(); }
        if screen == 28 { app.trackers.fixture(&app.state); }
        if [29,30,31,32].contains(&screen) { app.home=false; }
        if screen == 29 { app.show_record=true; }
        if screen == 30 { app.rename=Some("Итоги встречи с командой".into()); }
        if screen == 31 { app.confirm_regenerate=true; }
        if screen == 32 { if let Some(document)=app.state.selected.as_ref().and_then(|p|app.state.documents.get_mut(p)) { document.data.unfinished=true; } }
        if screen == 35 {app.organization.fixture(&app.state.settings);app.organization.fixture_delete();}
        if screen == 36 {app.people.fixture(&app.state.settings,23);app.people.fixture_merge();}
        if screen == 37 {app.organization.fixture(&app.state.settings);app.organization.fixture_select();}
        if screen == 38 {app.show_settings=true;app.settings_panel.section=crate::settings::Section::Ai;}
        if screen == 5 { app.state.tab = Tab::Transcript; }
        if screen == 39 || screen == 41 {
            app.home=false; app.state.tab=Tab::Transcript;
            let path=app.state.selected.clone().unwrap();
            let document=app.state.documents.get_mut(&path).unwrap();
            let template=document.data.transcript.segments[0].clone();
            document.data.transcript.segments=(0..5000).map(|index|{
                let mut turn=template.clone();turn.id=uuid::Uuid::now_v7();turn.start_ms=index*1000;turn.end_ms=index*1000+900;
                turn.text=format!("Реплика {}. Проверяем длинную встречу, переносы строк и сохранение позиции при прокрутке.",index+1);turn
            }).collect();
            if screen==39 {document.speaker_ui.scroll_to=Some(document.data.transcript.segments[2499].id);}
            if screen==41 {app.state.recording.path=Some(path);app.state.recording.phase=crate::state::Phase::Recording;app.state.recording.started=Some(std::time::Instant::now()-Duration::from_secs(123));app.state.recording.levels=[0.65,0.3];}
        }
        if screen == 40 {
            app.home=true; let template=&app.state.rows[0];
            app.state.rows=(0..5000).map(|index| {
                let mut meta=template.meta.clone();meta.id=uuid::Uuid::now_v7();meta.title=format!("Встреча {}",index+1);meta.started_at-=chrono::Duration::minutes(index*5);
                crate::worker::MeetingRow {meeting:template.meeting.clone(),meta,snippet:String::new(),anchor:None}
            }).collect();
        }

        app
    }
    pub fn fixture_jump_settled(&self) -> bool { self.state.selected.as_ref().and_then(|path| self.state.documents.get(path)).is_none_or(|document| document.speaker_ui.scroll_to.is_none()) }
    pub fn pill(ui: &mut egui::Ui) { recording_pill(ui, Duration::from_secs(123), 0.3); }
}
'''
    if path.stem in {"ui", "state", "quickstart", "people", "organization", "maintenance", "trackers", "theme"}:
        path = harness / "src" / path.name
        path.write_text(text)
    modules.append(f'#[path = "{path}"] mod {path.stem};')
main = r'''
#![allow(dead_code)]
MODULES
use eframe::egui;
struct Preview { app: ui::NotetakerApp, frames: usize, output: std::path::PathBuf, screen: usize, started: Option<f64>, captured: bool }
impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Use the real UI, but never call its logic/worker polling.
        // A stationary Xvfb pointer must not open an unrelated hover submenu.
        ui.ctx().input_mut(|input| input.pointer = Default::default());
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("fixture-meeting-menu"), self.screen == 15));
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("preview-expand-settings"),self.screen==37 || self.screen==38));
        self.app.ui(ui, frame);
        if self.screen==33 {
            let ctx=ui.ctx().clone();
            theme::modal(&ctx,"Элементы интерфейса").default_width(520.0).show(&ctx,|ui| {
                ui.horizontal(|ui| {
                    theme::button(ui,"Новая запись",theme::ButtonStyle::Primary);
                    theme::button(ui,"Копировать",theme::ButtonStyle::Secondary);
                    theme::button(ui,"Удалить",theme::ButtonStyle::Danger);
                });
                ui.add_enabled(false,theme::styled_button(theme::Palette::of(ui),"Недоступно",theme::ButtonStyle::Secondary));
                ui.horizontal(|ui|{let mut checked=true;let mut unchecked=false;ui.add(theme::Checkbox::new(&mut checked,"Включён"));ui.add(theme::Checkbox::new(&mut unchecked,"Выключен"));});
                let mut on=true;theme::toggle(ui,&mut on,"Переключатель");
                theme::disclosure("Дополнительные параметры").default_open(true).show(ui,|ui| {ui.label("Параметры записи и распознавания");});
                let mut selected=0;theme::segmented(ui,&mut selected,&[(0,"Конспект"),(1,"Транскрипт")]);
                theme::select("widget-select").selected_text("Выбранный вариант").show_ui(ui,|ui| {theme::select_value(ui,&mut selected,0,"Выбранный вариант");});
                let mut value=String::new();let response=ui.add(theme::text_field(&mut value).hint_text("Название встречи"));response.request_focus();
            });
        }
        if self.screen==34 {
            theme::update_prompt(ui.ctx(),"Доступно обновление".into(),"Новая версия готова к установке.","Установить",true,true,true);
        }
        if self.screen == 12 {
            egui::Area::new(egui::Id::unique("recording-pill-preview")).anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -100.0])
                .show(ui.ctx(), ui::NotetakerApp::pill);
        }
        self.frames += 1;
        let now=ui.input(|input|input.time);
        let started=*self.started.get_or_insert(now);
        let settled = self.screen != 39 || self.app.fixture_jump_settled();
        if self.frames >= 10 && now-started >= 0.4 && !self.captured && settled {
            self.captured=true;
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
        state.settings.theme = if dark { theme::Theme::Dark } else { theme::Theme::Light };
        state.settings.library = std::path::PathBuf::from(std::env::var_os("NOTETAKER_PREVIEW_FIXTURES").expect("preview fixture root")).join(std::path::Path::new(&output).file_stem().unwrap()).join("library");
        state.settings.models_folder = Some(state.settings.library.with_file_name("models"));
        state.settings_draft = state.settings.clone();
        state.quickstart.models = quickstart::Models { gigaam: quickstart::Status::Ready, parakeet: quickstart::Status::Ready,
            auto: quickstart::Status::Ready, diarization: quickstart::Status::Ready, semantic: quickstart::Status::Ready };
        if screen == 0 { state.downloading = true; state.quickstart.models = Default::default(); }
        let library = notetaker_core::Library::open(&state.settings.library).unwrap();
        let mut folders=library.load_folders().unwrap();folders.value.insert("Команда".into(),Default::default());library.save_folders(&folders.value,folders.revision).unwrap();
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
            meeting.write_transcript(&data.transcript).unwrap();
            meeting.write_notes(&data.notes).unwrap();
            meeting.write_enhanced(&data.enhanced).unwrap();
            state.rows.push(worker::MeetingRow { meeting: meeting.clone(), meta, snippet: String::new(), anchor: None });
            state.selected = Some(meeting.path().to_owned());
            state.documents.insert(meeting.path().to_owned(), state::Document::new(data));
        }
        library.reindex_incremental().unwrap();
        state.tasks=library.list_tasks(None,None,false).unwrap();
        assert!(!state.tasks.is_empty(),"task fixtures must contain indexed tasks");
        state.connections.load(&state.llm);
        Ok(Box::new(Preview { app: ui::NotetakerApp::fixture(state, screen), frames: 0, output: output.into(), screen, started:None,captured:false }))
    }))
}
'''.replace("MODULES", "\n".join(modules))
(harness / "src/main.rs").write_text(main)
env = os.environ.copy()
env["CARGO_TARGET_DIR"] = str(root / "target")
env["CARGO_INCREMENTAL"] = "0"
fixture_root = harness / "fixture" / f"run-{time.time_ns()}"
fixture_root.mkdir(parents=True)
env["NOTETAKER_PREVIEW_FIXTURES"] = str(fixture_root)
for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
    directory = harness / "fixture" / key.lower()
    directory.mkdir(parents=True, exist_ok=True)
    env[key] = str(directory)
env["LIBGL_ALWAYS_SOFTWARE"] = "1"
env["LD_LIBRARY_PATH"] = str(harness / "libs/usr/lib/x86_64-linux-gnu") + ":" + env.get("LD_LIBRARY_PATH", "")
if shutil.disk_usage(root).free < 2 * 1024**3:
    raise SystemExit("Less than 2 GiB free; stop before building previews")
# Resolve the derived harness root against the copied workspace lock before its locked build.
subprocess.run(["cargo", "metadata", "--offline", "--format-version", "1", "--manifest-path", str(harness / "Cargo.toml")], cwd=root, env=env, check=True, stdout=subprocess.DEVNULL)
subprocess.run(["cargo", "build", "--locked", "--manifest-path", str(harness / "Cargo.toml")], cwd=root, env=env, check=True)
names = ["welcome", "name", "connections", "home", "note", "transcript", "general", "recording", "calendars", "ai", "search", "storage", "recording-pill", "connections-expanded", "delete-confirm", "meeting-menu", "toast", "empty-library", "narrow-settings", "recording-bar", "chat", "tasks", "people", "person", "series", "digest", "folders", "diagnostics", "tracker-preview", "new-recording", "rename", "regenerate", "capture-warning", "widgets", "update-card", "folder-delete", "merge-people", "folders-advanced", "ai-expanded", "performance-transcript", "performance-meetings", "performance-live"]
def render(spec):
    index, name, theme, width, height, zoom = spec
    actual_width = 760 if name == "narrow-settings" and zoom == 1.0 else width
    target = output / f"{name}-{theme}-{width}x{height}-{int(zoom*100)}.png"
    if shutil.disk_usage(root).free < 2 * 1024**3:
        raise RuntimeError("Less than 2 GiB free; stop rendering")
    subprocess.run(["xvfb-run", "-a", str(root / "target/debug/onboarding-preview"), str(index), theme, str(target), str(actual_width), str(height), str(zoom)], cwd=root, env=env, check=True, timeout=30, stdout=subprocess.DEVNULL)
    print(target, flush=True)

if args.screens:
    unknown = set(args.screens) - set(names)
    if unknown:
        parser.error(f"Unknown screens: {sorted(unknown)}")
specs = [(index,name,theme,width,height,zoom)
    for width,height,zoom in [(1280,800,1.0),(960,640,1.0),(960,640,1.5)]
    for index,name in enumerate(names) if not args.screens or name in args.screens
    for theme in ("light","dark")]
# Isolated fixture libraries let software rendering run concurrently without shared data writes.
with ThreadPoolExecutor(max_workers=3) as executor:
    list(executor.map(render,specs))

# A local, self-contained gallery. The inline viewer uses these same PNG files.
options = "".join(f'<option value="{name}">{name}</option>' for name in names)
images = "".join(f'<img data-key="{name}-{theme}-{size}-{zoom}" src="{output}/{name}-{theme}-{size}-{zoom}.png" alt="{name}, {theme}, {size}, {zoom}%" hidden>'
    for name in names for theme in ("light", "dark") for size, zoom in (("1280x800", 100), ("960x640", 100), ("960x640", 150)))
html = '<!doctype html><html><head><meta charset="utf-8"><style>:root{font-family:var(--font-sans, sans-serif)}[hidden]{display:none}img{width:100%;height:auto;border-radius:var(--radius, 8px)}nav{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px}select{font:inherit;color:var(--foreground);background:var(--card);border:1px solid var(--border);border-radius:6px;padding:6px}</style></head><body><nav><select id="screen">' + options + '</select><select id="theme"><option>light</option><option>dark</option></select><select id="size"><option>1280x800-100</option><option>960x640-100</option><option>960x640-150</option></select></nav>' + images + '<script>function show(){const key=screenSelect.value+"-"+themeSelect.value+"-"+sizeSelect.value;document.querySelectorAll("img").forEach(img=>img.hidden=img.dataset.key!==key)}const screenSelect=document.getElementById("screen"),themeSelect=document.getElementById("theme"),sizeSelect=document.getElementById("size");document.querySelectorAll("select").forEach(s=>s.onchange=show);show();</script></body></html>'
(output / "index.html").write_text(html)
