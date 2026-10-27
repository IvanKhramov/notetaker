//! Windows-only native integrations. Constructed and polled on eframe's Win32
//! event-loop thread. Native callbacks wake egui via Context::request_repaint.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
};

use anyhow::Result;
use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use tray_icon::{
    Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

#[derive(Clone, Copy)]
pub enum Action {
    Show,
    Toggle,
    Recap,
    Bookmark,
    Quit,
}
pub struct Desktop {
    tray: TrayIcon,
    recording: bool,
    manager: GlobalHotKeyManager,
    hotkeys: Vec<HotKey>,
    routes: Arc<Mutex<HashMap<u32, Action>>>,
    events: Receiver<Action>,
}
impl Desktop {
    pub fn new(ctx: &egui::Context) -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Открыть notetaker", true, None);
        let toggle = MenuItem::new("Начать / остановить запись", true, None);
        let quit = MenuItem::new("Выход", true, None);
        menu.append_items(&[&show, &toggle, &quit])?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("notetaker")
            .with_icon(Icon::from_rgba(
                crate::icon::image(false).into_raw(),
                32,
                32,
            )?)
            .with_menu(Box::new(menu))
            .build()?;
        let manager = GlobalHotKeyManager::new()?;
        let (tx, events) = mpsc::channel();
        let sender = tx.clone();
        let context = ctx.clone();
        let show_id = show.id().clone();
        let toggle_id = toggle.id().clone();
        let quit_id = quit.id().clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == show_id {
                Action::Show
            } else if event.id == toggle_id {
                Action::Toggle
            } else if event.id == quit_id {
                Action::Quit
            } else {
                return;
            };
            let _ = sender.send(action);
            context.request_repaint();
        }));
        let sender = tx.clone();
        let context = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                let _ = sender.send(Action::Show);
                context.request_repaint();
            }
        }));
        let context = ctx.clone();
        let routes = Arc::new(Mutex::new(HashMap::new()));
        let callback_routes = Arc::clone(&routes);
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed {
                if let Some(action) = callback_routes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&event.id)
                {
                    let _ = tx.send(*action);
                }
                context.request_repaint();
            }
        }));
        Ok(Self {
            tray,
            recording: false,
            manager,
            hotkeys: Vec::new(),
            routes,
            events,
        })
    }
    pub fn set_hotkeys(&mut self, values: [&str; 3]) -> Result<()> {
        crate::config::validate_hotkeys(values)?;
        let next = values
            .iter()
            .map(|value| value.parse::<HotKey>())
            .collect::<Result<Vec<_>, _>>()?;
        let mut registered = Vec::new();
        for key in &next {
            if !self.hotkeys.contains(key) {
                if let Err(error) = self.manager.register(*key) {
                    for key in registered {
                        let _ = self.manager.unregister(key);
                    }
                    return Err(error.into());
                }
                registered.push(*key);
            }
        }
        for old in &self.hotkeys {
            if !next.contains(old) {
                self.manager.unregister(*old)?;
            }
        }
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        routes.clear();
        for (key, action) in next
            .iter()
            .zip([Action::Toggle, Action::Recap, Action::Bookmark])
        {
            routes.insert(key.id(), action);
        }
        self.hotkeys = next;
        Ok(())
    }
    pub fn poll(&self) -> Vec<Action> {
        self.events.try_iter().collect()
    }
    pub fn set_recording(&mut self, recording: bool) -> Result<()> {
        if self.recording != recording {
            self.tray.set_icon(Some(Icon::from_rgba(
                crate::icon::image(recording).into_raw(),
                32,
                32,
            )?))?;
            self.recording = recording;
        }
        Ok(())
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        for hotkey in self.hotkeys.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
    }
}
