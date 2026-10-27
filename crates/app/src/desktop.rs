//! Windows-only native integrations. Constructed and polled on eframe's Win32
//! event-loop thread. Native callbacks wake egui via Context::request_repaint.
use std::sync::mpsc::{self, Receiver};

use anyhow::Result;
use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use tray_icon::{
    Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

pub enum Action {
    Show,
    Toggle,
    Quit,
}
pub struct Desktop {
    tray: TrayIcon,
    recording: bool,
    manager: GlobalHotKeyManager,
    hotkey: Option<HotKey>,
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
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed {
                let _ = tx.send(Action::Toggle);
                context.request_repaint();
            }
        }));
        Ok(Self {
            tray,
            recording: false,
            manager,
            hotkey: None,
            events,
        })
    }
    pub fn set_hotkey(&mut self, text: &str) -> Result<()> {
        let hotkey: HotKey = text.parse()?;
        if self.hotkey == Some(hotkey) {
            return Ok(());
        }
        // Register first: a conflict preserves the working old shortcut.
        self.manager.register(hotkey)?;
        if let Some(old) = self.hotkey.replace(hotkey) {
            self.manager.unregister(old)?;
        }
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
        if let Some(hotkey) = self.hotkey.take() {
            let _ = self.manager.unregister(hotkey);
        }
    }
}
