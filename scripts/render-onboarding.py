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
parser.add_argument("--source-ref", help="Read app sources from a Git revision for before/after comparisons")
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
sources = sorted((root / "crates/app/src").glob("*.rs"))
if args.source_ref:
    sources = [root / name for name in subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", args.source_ref, "crates/app/src"], cwd=root, text=True
    ).splitlines() if name.endswith(".rs")]
for path in sources:
    if path.stem in {"main", "desktop", "macos", "updates", "updates_macos", "icons", "test_ui"}:
        continue
    text = (subprocess.check_output(["git", "show", f"{args.source_ref}:{path.relative_to(root)}"], cwd=root, text=True)
            if args.source_ref else path.read_text())
    if path.stem == "icon":
        text = text.replace('"../assets/', f'"{root / "crates/app/assets"}/')
    if path.stem == "state":
        text = text.replace("    fn with_channels(", "    pub(crate) fn with_channels(")
        text = text.replace("    fn new(data: DocumentData)", "    pub(crate) fn new(data: DocumentData)")
        text += "impl Recording {pub fn fixture_silence(&mut self) {self.silent_since[0]=Some(std::time::Instant::now()-std::time::Duration::from_secs(25));}}"
    if path.stem == "quickstart":
        text = text.replace('index == 1 && cfg!(target_os = "macos")', 'index == 1')
        text += 'impl Panel {pub fn fixture_sound(&mut self, scene: usize) {self.probe_status=Some([Status::Ready, sound_status(if scene==78 {0} else {100}, scene!=80, true)]);}}'
        text = text.replace("    step: usize,", "    pub(crate) step: usize,\n    pub(crate) expanded: bool,")
        text = text.replace('crate::theme::disclosure("Календарь")', 'crate::theme::disclosure("Календарь").default_open(self.expanded)')
        text = text.replace('crate::theme::disclosure("AI для конспектов")', 'crate::theme::disclosure("AI для конспектов").default_open(self.expanded)')
    if path.stem == "settings":
        text = text.replace('.id_salt(("settings-page", self.section as u8))', '.id_salt(("settings-page", self.section as u8)).vertical_scroll_offset(ui.ctx().data(|data| data.get_temp::<f32>(egui::Id::unique("fixture-recording-settings-offset")).unwrap_or(0.0)))')
    if path.stem == "people":
        text = text.replace('.id_salt("people-screen")', '.id_salt("people-screen").vertical_scroll_offset(ui.ctx().data(|data| data.get_temp::<f32>(egui::Id::unique("fixture-person-offset")).unwrap_or(0.0)))')
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
    if path.stem == "speakers":
        text = text.replace('let form_key = egui::Id::unique(("speaker-rename-form", meta.id, id));', 'let form_key = egui::Id::unique(("speaker-rename-form", meta.id, id)); if ui.ctx().data(|data|data.get_temp::<bool>(egui::Id::unique("fixture-rename-form")).unwrap_or(false)) {ui.data_mut(|data| {data.insert_temp(form_key,true);data.insert_temp(form_key.with("focus"),true);});}')
        text = text.replace(".close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)", '.open({let show=ui.ctx().data_mut(|data| {let id=egui::Id::unique("fixture-speaker-menu");let show=data.get_temp::<bool>(id).unwrap_or(false);data.insert_temp(id,false);show});show || egui::Popup::is_id_open(ui.ctx(), egui::Popup::menu(anchor).get_id())}).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)')
    if path.stem == "calendar_ui":
        text = text.replace('theme::disclosure("Пропущенные события")', 'theme::disclosure("Пропущенные события").default_open(true)')
        text = text.replace('theme::disclosure("Где взять ссылку")', 'theme::disclosure("Где взять ссылку").default_open(!ui.ctx().data(|d|d.get_temp::<bool>(egui::Id::unique("calendar-status-preview")).unwrap_or(false)))')
    if path.stem == "theme":
        text=text.replace('"../assets/',f'"{root / "crates/app/assets"}/')
        text=text.replace('#[path = "icons.rs"]',f'#[path = "{root / "crates/app/src/icons.rs"}"]')
        text=text.replace('#[cfg(any(windows, target_os = "macos"))]','')
        text=text.replace('self.default_open,', 'self.default_open || ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::unique("preview-expand-settings")).unwrap_or(false)),')
    if path.stem == "ui":
        expected = 'egui::Popup::menu(&menu_response)'
        assert text.count(expected) == 1, "meeting menu fixture hook changed"
        text = text.replace(expected, expected + '.open(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::unique("fixture-meeting-menu")).unwrap_or(false)) || egui::Popup::is_id_open(ui.ctx(), egui::Popup::menu(&menu_response).get_id()))')
        text += '''
impl NotetakerApp {
    pub fn fixture(state: AppState, screen: usize) -> Self {
        let scene=screen;
        let screen=match screen {92=>3,93=>3,94=>10,_=>screen};
        let screen=match screen {89=>3,90=>19,91=>7,_=>screen};
        let screen=match screen {87=>0,88=>6,_=>screen};
        let screen=match screen {83=>51,84|85|86=>3,_=>screen};
        let screen = if (78..=80).contains(&screen) {0} else {screen};
        let screen = match screen {59=>5,60=>4,61=>11,62=>23,63=>24,64=>4,65=>5,66=>7,67=>3,68=>7,69=>7,70=>5,71=>5,72=>5,_=>screen};
        let mut app = Self::with_state(state);
        if scene==92 || scene==93 {
            app.state.quickstart.models.set(&app.state.settings.model.clone(),crate::quickstart::Status::Ready);
            if scene==93 {app.state.downloading=true;app.state.quickstart.download_model=Some("diarization".into());let at=Instant::now();let mut transfer=crate::quickstart::Transfer::waiting(at);for line in ["NOTETAKER_DOWNLOAD_PLAN pyannote-3.0 6958444 1","NOTETAKER_DOWNLOAD_PLAN eres2net-base 39593761 0","NOTETAKER_DOWNLOAD_PLAN_END 1","NOTETAKER_DOWNLOAD eres2net-base 10000000 39593761"]{transfer.update(line,at);}app.state.quickstart.transfer=Some(transfer);}
        }
        if scene==94 {app.state.quickstart.optional_errors.insert("semantic".into(),"Не удалось подключиться к серверу модели".into());}

        if (89..=91).contains(&scene) {
            app.state.permissions.channels=[notetaker_capture::Permission::Granted,notetaker_capture::Permission::Unknown];
            if scene==89 || scene==91 {app.state.permissions.channels[0]=notetaker_capture::Permission::Denied;}
            if scene==90 {app.state.permissions.channels[1]=notetaker_capture::Permission::Denied;}
            if scene==91 {app.state.permissions.failed[1]=true;}
        }

        if (83..=85).contains(&scene) {
            app.state.downloading=true; app.state.quickstart.models=Default::default();app.state.quickstart.download_model=Some("auto".into());
            let at=Instant::now()-std::time::Duration::from_secs(if scene==85 {31} else {0});
            let mut transfer=crate::quickstart::Transfer::waiting(at);transfer.update("NOTETAKER_DOWNLOAD_PLAN gigaam-v3 170197019 0",at);transfer.update("NOTETAKER_DOWNLOAD_PLAN silero-vad 2327524 0",at);transfer.update("NOTETAKER_DOWNLOAD_PLAN_END 1",at);transfer.update("NOTETAKER_DOWNLOAD gigaam-v3 84000000 170197019",at);app.state.quickstart.transfer=Some(transfer);
        }
        if scene==86 {
            app.state.settings.calendars=vec![notetaker_llm::calendar::Subscription {id:"partial-fixture".into(),name:"Тестовый календарь".into()}];
            let now=chrono::Utc::now();
            app.state.calendar_events=vec![notetaker_core::calendar::CalendarEvent {link:notetaker_core::calendar::EventLink {uid:"partial-preview".into(),title:"Синтетическая встреча".into(),attendees:vec![],call_url:Some("https://space.ktalk.ru/room123".into())},start:now-chrono::Duration::minutes(2),end:now+chrono::Duration::minutes(30),all_day:false,organizer:None}];
            app.state.calendar_reports=vec![notetaker_llm::calendar::SubscriptionStatus {skipped:vec![notetaker_core::calendar::SkippedEvent {title:"Неизвестный формат".into(),reason:notetaker_core::calendar::SkipReason::Format}],..Default::default()}];
        }

        app.home = ![4,5,12,14,15].contains(&screen);
        if screen < 3 { app.quickstart.open = true; app.quickstart.step = screen; }
        if scene==87 {app.state.downloading=true;app.state.quickstart.models=Default::default();let at=Instant::now()-std::time::Duration::from_secs(10);let mut transfer=crate::quickstart::Transfer::waiting(at);for line in ["NOTETAKER_DOWNLOAD_PLAN gigaam-v3 170197019 0","NOTETAKER_DOWNLOAD_PLAN silero-vad 2327524 0","NOTETAKER_DOWNLOAD_PLAN_END 1","NOTETAKER_DOWNLOAD gigaam-v3 84000000 170197019"] { transfer.update(line,Instant::now()); }app.state.quickstart.transfer=Some(transfer);}
        if scene==88 {app.state.settings_draft.user_aliases=String::new();}
        if (78..=80).contains(&scene) {app.quickstart.fixture_sound(scene);app.state.downloading=true;app.state.quickstart.models=Default::default();}
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
        if (42..=45).contains(&screen) {
            app.home=screen==43 || screen==44;
            if screen==44 {app.ask_text="/".into();}
            if screen==45 {app.state.recording.path=app.state.selected.clone();app.state.recording.phase=Phase::Recording;app.state.recording.started=Some(Instant::now()-Duration::from_secs(123));}
        }
        if (46..=50).contains(&screen) {
            app.home=false;
            let path=app.state.selected.clone().unwrap();
            if screen==46 || screen==47 {
                app.state.recording.path=Some(path.clone());
                app.state.recording.phase=Phase::Recording;
                app.state.recording.started=Some(Instant::now()-Duration::from_secs(123));
            }
            if screen==46 {
                app.state.recording.fixture_silence();
            }
            if screen==47 {
                app.state.recording.apply(&notetaker_pipeline::PipelineEvent::FinalPassProgress {completed:3,total:8});
            }
            if screen==48 || screen==49 {app.show_record=true;app.state.llm_configured=screen==48;app.state.record_mode=notetaker_core::Mode::Local;}
            if screen==50 {
                app.state.documents.get_mut(&path).unwrap().data.enhanced.clear();
                app.state.skipped_summaries.insert(path,"AI не подключён (Настройки → AI)".into());app.state.settings.auto_summary=true;
                app.state.llm_configured=false;app.state.tab=Tab::Enhanced;
            }
        }
        if (51..=54).contains(&screen) {
            app.home=true;
            app.state.settings.calendars.push(notetaker_llm::calendar::Subscription {id:"calendar-preview".into(),name:"Команда".into()});
            app.state.settings_draft=app.state.settings.clone();
            let now=chrono::Utc::now();
            let event=notetaker_core::calendar::CalendarEvent {
                link:notetaker_core::calendar::EventLink {uid:"entry".into(),title:"Планирование с командой".into(),attendees:vec![notetaker_core::calendar::Attendee{name:"Иван".into(),email:String::new()},notetaker_core::calendar::Attendee{name:"Алексей".into(),email:String::new()}],call_url:if screen==52 {None} else {Some("https://meet.google.com/example".into())}},
                start:now+chrono::Duration::minutes(if screen==53 {30} else {3}),end:now+chrono::Duration::minutes(60),all_day:false,organizer:None};
            app.state.calendar_events=vec![event.clone()];
            if screen==51 || screen==52 {app.state.calendar_reminder=Some((event.link.uid.clone(),event.start));}
            if screen==52 {app.state.recording.path=app.state.selected.clone();app.state.recording.phase=Phase::Recording;app.state.recording.started=Some(Instant::now()-Duration::from_secs(123));}
            if screen==54 {app.show_settings=true;app.settings_panel.section=crate::settings::Section::Calendars;}
        }
        if (55..=58).contains(&screen) {
            app.home=false;
            let path=app.state.selected.clone().unwrap();
            let document=app.state.documents.get_mut(&path).unwrap();
            document.data.notes=String::new();
            document.data.transcript.speaker_names.insert("me".into(),"Иван".into());
            document.data.enhanced="- [x] Проверить резервную копию\n- [ ] Обновить кластер".into();
            app.state.tab=Tab::Enhanced;
            app.state.templates=vec!["general".into(),"standup".into(),"one-on-one".into()];
            app.state.llm_configured=true;
            if screen==56 {document.template="standup".into();document.template_pending=true;}
            if screen==58 {
                app.state.tab=Tab::Transcript;
                let segment=&document.data.transcript.segments[0];
                let _=segment;
            }
        }
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

        if (68..=72).contains(&scene) {
            app.state.ktalk.address="https://neutral.ktalk.ru".into();
            app.state.ktalk.settings=Some(notetaker_llm::ktalk::Settings {base_url:app.state.ktalk.address.clone(),credential_id:"neutral".into(),api_key_env:None,extra:Default::default()});
            app.state.ktalk.people=vec![notetaker_core::person_from_name("Анна Петрова").unwrap()];
            if scene<=69 {
                app.show_settings=true;app.settings_panel.section=crate::settings::Section::Ktalk;
                if scene==68 {app.state.ktalk.address.clear();app.state.ktalk.settings=None;}
                else {app.state.ktalk.saved=true;app.state.ktalk.status="Подключено: Олег Сидоров".into();}
            } else {
                let path=app.state.selected.clone().unwrap();
                let (local,plan)=crate::ktalk_fixture::plan();
                let document=app.state.documents.get_mut(&path).unwrap();
                document.data.meeting.update_meta(|meta| {meta.started_at=plan.recording.created_date;meta.ended_at=Some(meta.started_at+chrono::Duration::minutes(5));}).unwrap();
                document.data.meeting.write_transcript(&local).unwrap();
                crate::ktalk::register_recording(&document.data.meeting).unwrap();
                document.data=crate::worker::DocumentData::read(document.data.meeting.clone()).unwrap();
                let task=crate::ktalk::Task {meeting:document.data.meeting.clone(),ended:plan.recording.created_date+chrono::Duration::minutes(5),progress:Default::default()};
                if scene==70 {
                    app.state.ktalk.preview_open=true;app.state.ktalk.preview=Some((task,crate::ktalk::Outcome::Preview(Box::new(plan))));
                } else {
                    let message=crate::ktalk::run(crate::ktalk::Job::Apply {task,plan:Box::new(plan),voices:false},&notetaker_llm::OsCredentialStore);
                    app.state.ktalk_result(message);
                    if scene==71 {app.state.ktalk.successes.clear();} else {app.state.settings.voice_library=true;app.state.settings_draft.voice_library=true;}
                }
            }
        }
        if scene==65 {
            app.state.recording.phase=Phase::Recording;app.state.recording.path=app.state.selected.clone();
            app.state.recording.started=Some(Instant::now()-Duration::from_secs(123));app.state.recording.levels=[0.15,0.0];
            app.state.call_prompt=Some(CallPrompt::Stop {pid:42,started_at:100,countdown:crate::detection::Countdown::new(Instant::now())});
        }
        if scene==67 {
            let row=app.state.rows.last_mut().unwrap();row.meta.title="План релиза мобильного приложения".into();row.meta.title_source=Some(notetaker_core::TitleSource::Generated);
            row.meeting.update_meta(|meta| {meta.title=row.meta.title.clone();meta.title_source=row.meta.title_source.clone();}).unwrap();
            if let Some(document)=app.state.documents.get_mut(row.meeting.path()) {document.data.meta=row.meta.clone();}
            app.state.calendar_events.clear();app.state.rows.rotate_right(1);
        }
        if (73..=77).contains(&scene) {
            app.home=true;
            let now=chrono::Utc::now();
            app.state.settings.calendars=vec![notetaker_llm::calendar::Subscription {id:"calendar-visible".into(),name:"Команда".into()}];
            app.state.settings_draft=app.state.settings.clone();
            let events:Vec<_>=(0..8).map(|i| {
                use chrono::TimeZone;
                let start=if i==0 {now-chrono::Duration::minutes(10)} else if i<4 {now+chrono::Duration::minutes(20*i)} else {
                    let day=now.with_timezone(&chrono::Local).date_naive().succ_opt().unwrap();
                    chrono::Local.from_local_datetime(&day.and_hms_opt(10+i as u32,0,0).unwrap()).earliest().unwrap().with_timezone(&chrono::Utc)
                };
                notetaker_core::calendar::CalendarEvent {link:notetaker_core::calendar::EventLink {uid:format!("visible-{i}"),title:["Планирование команды","Обзор прототипа","Проверка результатов","Обсуждение задач","Рабочая встреча","Итоги проекта","План следующего этапа","Обзор документации"][i as usize].into(),attendees:vec![],call_url:(i%2==0).then(||"https://zoom.us/neutral".into())},start,end:start+chrono::Duration::minutes(30),all_day:false,organizer:None}
            }).collect();
            let skipped=vec![notetaker_core::calendar::SkippedEvent {title:"Почасовой обзор".into(),reason:notetaker_core::calendar::SkipReason::Recurrence},notetaker_core::calendar::SkippedEvent {title:"Повтор по номеру недели".into(),reason:notetaker_core::calendar::SkipReason::Recurrence},notetaker_core::calendar::SkippedEvent {title:"Событие в неизвестной зоне с длинным названием для проверки усечения строки без изменения ширины карточки подписки".repeat(3),reason:notetaker_core::calendar::SkipReason::Timezone}];
            let error=(scene==75 || scene==77).then(||"Не удалось загрузить календарь. Проверьте сеть и сохранённую подписку.".into());
            app.state.calendar_reminder=Some((events[0].link.uid.clone(),events[0].start));
            app.state.dismiss_calendar_reminder();
            app.state.calendar_events=if scene==74 {vec![]} else {events.clone()};
            app.state.calendar_reports=vec![notetaker_llm::calendar::SubscriptionStatus {id:"calendar-visible".into(),events:app.state.calendar_events.clone(),skipped:if scene==76 {skipped} else {vec![]},updated_at:Some(if scene==75 || scene==77 {now-chrono::Duration::days(1)} else {now}),error}];
            app.state.calendar_busy=false;
            app.state.calendar_checked=Some(Instant::now());
            if scene==76 || scene==77 {app.show_settings=true;app.settings_panel.section=crate::settings::Section::Calendars;}
        }
        app
    }
    pub fn fixture_jump_settled(&self) -> bool { self.state.selected.as_ref().and_then(|path| self.state.documents.get(path)).is_none_or(|document| document.speaker_ui.scroll_to.is_none()) }
    pub fn pill(ui: &mut egui::Ui) { recording_pill(ui, Duration::from_secs(123), 0.3); }
}
'''
    if args.source_ref or path.stem in {"ui", "state", "quickstart", "people", "organization", "maintenance", "trackers", "theme", "calendar_ui", "speakers", "settings"}:
        path = harness / "src" / path.name
        path.write_text(text)
    modules.append(f'#[path = "{path}"] mod {path.stem};')
main = r'''
#![allow(dead_code)]
MODULES
use eframe::egui;
struct Preview { app: ui::NotetakerApp, frames: usize, output: std::path::PathBuf, screen: usize, started: Option<f64>, captured: bool, _commands: std::sync::mpsc::Receiver<worker::Command> }
impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Use the real UI, but never call its logic/worker polling.
        // A stationary Xvfb pointer must not open an unrelated hover submenu.
        ui.ctx().input_mut(|input| input.pointer = Default::default());
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("fixture-meeting-menu"), self.screen == 15));
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("preview-expand-settings"),self.screen==37 || self.screen==38));
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("calendar-status-preview"),self.screen==76 || self.screen==77));
        ui.ctx().data_mut(|data| {data.insert_temp(egui::Id::unique("fixture-speaker-menu"),self.screen==58);data.insert_temp(egui::Id::unique("fixture-rename-form"),self.screen==58);});
        if self.screen==66 && ui.ctx().content_rect().height()<600.0 {
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("fixture-recording-settings-offset"),240.0_f32));
        }
        self.app.ui(ui, frame);
        if self.screen==81 {egui::Window::new("Загрузка").show(ui.ctx(), |ui| {crate::theme::spinner(ui);});}
        if self.screen==82 {egui::Window::new("Подсказка звонка").show(ui.ctx(), |ui| {crate::detection::start_card(ui,"Chrome",None,Some("Дождитесь проверки звука"));});}
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
            theme::update_prompt(ui.ctx(),"Доступно обновление".into(),"Новая версия готова к установке.","Установить",true,true,Some(&["Новая версия готова к установке.".into()]));
        }
        if self.screen == 12 {
            egui::Area::new(egui::Id::unique("recording-pill-preview")).anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -100.0])
                .show(ui.ctx(), ui::NotetakerApp::pill);
        }
        self.frames += 1;
        if self.screen == 62 && ui.ctx().content_rect().height() < 600.0 {
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::unique("fixture-person-offset"), 96.0_f32));
        }
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
        let (tx, commands) = std::sync::mpsc::channel(); let (_, rx) = std::sync::mpsc::channel();
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
        Ok(Box::new(Preview { app: ui::NotetakerApp::fixture(state, screen), frames: 0, output: output.into(), screen, started:None,captured:false,_commands:commands }))
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
names = ["welcome", "name", "connections", "home", "note", "transcript", "general", "recording", "calendars", "ai", "search", "storage", "recording-pill", "connections-expanded", "delete-confirm", "meeting-menu", "toast", "empty-library", "narrow-settings", "recording-bar", "chat", "tasks", "people", "person", "series", "digest", "folders", "diagnostics", "tracker-preview", "new-recording", "rename", "regenerate", "capture-warning", "widgets", "update-card", "folder-delete", "merge-people", "folders-advanced", "ai-expanded", "performance-transcript", "performance-meetings", "performance-live", "chat-meeting-empty", "chat-library-empty", "chat-recipes", "chat-recording", "recording-silence", "recording-final-pass", "recording-ready", "recording-not-ready", "summary-skipped", "calendar-reminder-link", "calendar-reminder-no-link", "calendar-upcoming", "calendar-help", "direct-task-checkboxes", "direct-template-bar", "direct-template-picker", "direct-speaker-menu", "compact-transcript", "compact-summary", "compact-storage", "compact-person", "compact-series", "one-step-bar", "one-step-countdown", "one-step-settings", "one-step-auto-title", "ktalk-settings-empty", "ktalk-settings-connected", "ktalk-preview", "ktalk-transcript", "ktalk-success", "calendar-home-events", "calendar-home-empty", "calendar-home-error", "calendar-settings-status", "calendar-settings-error", "mac-sound-no-access", "mac-sound-silent", "mac-sound-ready", "spinner-retina", "hint-disabled", "calendar-recording-blocked", "model-byte-progress", "model-download-stalled", "calendar-partial-success", "welcome-download-progress", "profile-list-hints", "permission-home", "permission-recording", "permission-settings", "recording-ready-speech-only", "optional-speaker-download", "optional-search-failure"]
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
specs += [(81,"spinner-retina",theme,1280,800,2.0) for theme in ("light","dark") if not args.screens or "spinner-retina" in args.screens]
# Isolated fixture libraries let software rendering run concurrently without shared data writes.
with ThreadPoolExecutor(max_workers=3) as executor:
    list(executor.map(render,specs))

# A local, self-contained gallery. The inline viewer uses these same PNG files.
options = "".join(f'<option value="{name}">{name}</option>' for name in names)
images = "".join(f'<img data-key="{name}-{theme}-{size}-{zoom}" src="{output}/{name}-{theme}-{size}-{zoom}.png" alt="{name}, {theme}, {size}, {zoom}%" hidden>'
    for name in names for theme in ("light", "dark") for size, zoom in (("1280x800", 100), ("960x640", 100), ("960x640", 150)))
html = '<!doctype html><html><head><meta charset="utf-8"><style>:root{font-family:var(--font-sans, sans-serif)}[hidden]{display:none}img{width:100%;height:auto;border-radius:var(--radius, 8px)}nav{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px}select{font:inherit;color:var(--foreground);background:var(--card);border:1px solid var(--border);border-radius:6px;padding:6px}</style></head><body><nav><select id="screen">' + options + '</select><select id="theme"><option>light</option><option>dark</option></select><select id="size"><option>1280x800-100</option><option>960x640-100</option><option>960x640-150</option></select></nav>' + images + '<script>function show(){const key=screenSelect.value+"-"+themeSelect.value+"-"+sizeSelect.value;document.querySelectorAll("img").forEach(img=>img.hidden=img.dataset.key!==key)}const screenSelect=document.getElementById("screen"),themeSelect=document.getElementById("theme"),sizeSelect=document.getElementById("size");document.querySelectorAll("select").forEach(s=>s.onchange=show);show();</script></body></html>'
(output / "index.html").write_text(html)
