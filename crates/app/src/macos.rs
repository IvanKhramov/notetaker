//! Route AppKit termination through the application's asynchronous shutdown,
//! and show the hidden main window when the Dock icon is clicked.
#![allow(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    sync::OnceLock,
};

use anyhow::{Context, Result, ensure};
use eframe::egui;
use objc2::{
    MainThreadMarker,
    runtime::{AnyObject, Bool, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};

thread_local! {
    static QUIT: Cell<bool> = const { Cell::new(false) };
    static REOPEN: Cell<bool> = const { Cell::new(false) };
    /// AppKit waits for replyToApplicationShouldTerminate: (logout, restart, Cmd+Q).
    static TERMINATE_PENDING: Cell<bool> = const { Cell::new(false) };
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

extern "C-unwind" fn should_terminate(
    _: &AnyObject,
    _: Sel,
    _: &NSApplication,
) -> NSApplicationTerminateReply {
    QUIT.set(true);
    TERMINATE_PENDING.set(true);
    repaint();
    // TerminateCancel would abort a logout or restart. TerminateLater keeps the
    // session waiting while state.exit() finalizes the recording; the run loop
    // stays in common modes, so the UI keeps drawing. finish_termination() replies.
    NSApplicationTerminateReply::TerminateLater
}

extern "C-unwind" fn should_handle_reopen(
    _: &AnyObject,
    _: Sel,
    _: &NSApplication,
    _visible_windows: Bool,
) -> Bool {
    REOPEN.set(true);
    repaint();
    Bool::YES
}

fn repaint() {
    CONTEXT.with_borrow(|ctx| {
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}

pub fn install_termination_handler(ctx: &egui::Context) -> Result<()> {
    let mtm = MainThreadMarker::new().context("AppKit требует главный поток")?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = app.delegate().context("Нет делегата AppKit")?;
    let object: &AnyObject = (*delegate).as_ref();
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        // winit 0.30 owns the delegate and requires its exact class. Add only
        // its missing optional methods, preserving all winit events.
        let class = std::ptr::from_ref(object.class()).cast_mut();
        // SAFETY: each callback matches its encoding on 64-bit macOS:
        // NSUInteger(id, SEL, id) and BOOL(id, SEL, id, BOOL). AppKit calls
        // them on the main thread with live objects.
        unsafe {
            objc2::ffi::class_addMethod(
                class,
                sel!(applicationShouldTerminate:),
                std::mem::transmute::<
                    extern "C-unwind" fn(
                        &AnyObject,
                        Sel,
                        &NSApplication,
                    ) -> NSApplicationTerminateReply,
                    Imp,
                >(should_terminate),
                c"Q@:@".as_ptr(),
            )
            .as_bool()
                && objc2::ffi::class_addMethod(
                    class,
                    sel!(applicationShouldHandleReopen:hasVisibleWindows:),
                    std::mem::transmute::<
                        extern "C-unwind" fn(&AnyObject, Sel, &NSApplication, Bool) -> Bool,
                        Imp,
                    >(should_handle_reopen),
                    c"B@:@B".as_ptr(),
                )
                .as_bool()
        }
    });
    ensure!(
        *installed,
        "Не удалось подключить безопасное завершение AppKit"
    );
    CONTEXT.with_borrow_mut(|context| *context = Some(ctx.clone()));
    // Refresh AppKit's cached optional delegate methods after adding the hook.
    app.setDelegate(None);
    app.setDelegate(Some(&delegate));
    Ok(())
}

pub fn quit_requested() -> bool {
    QUIT.replace(false)
}

pub fn reopen_requested() -> bool {
    REOPEN.replace(false)
}

/// Let AppKit end the process once shutdown is complete. Returns false when no
/// AppKit termination is waiting and the caller closes the window as usual.
pub fn finish_termination() -> bool {
    if !TERMINATE_PENDING.replace(false) {
        return false;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
    true
}

/// True when the app runs from a mounted disk image or a translocated copy
/// (quarantined app started outside Applications): it disappears on eject,
/// and updates or autostart act on another copy.
pub fn outside_applications() -> bool {
    static OUTSIDE: OnceLock<bool> = OnceLock::new();
    *OUTSIDE.get_or_init(|| {
        std::env::current_exe().is_ok_and(|path| {
            path.starts_with("/Volumes")
                || path
                    .components()
                    .any(|part| part.as_os_str() == "AppTranslocation")
        })
    })
}
