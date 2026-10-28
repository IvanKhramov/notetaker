//! Route AppKit termination through the application's asynchronous shutdown.
#![allow(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    sync::OnceLock,
};

use anyhow::{Context, Result, ensure};
use eframe::egui;
use objc2::{
    MainThreadMarker,
    runtime::{AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};

thread_local! {
    static QUIT: Cell<bool> = const { Cell::new(false) };
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

extern "C-unwind" fn should_terminate(
    _: &AnyObject,
    _: Sel,
    _: &NSApplication,
) -> NSApplicationTerminateReply {
    QUIT.set(true);
    CONTEXT.with_borrow(|ctx| {
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
    // Never let NSApplication terminate the process. eframe closes its event
    // loop after state.exit() has finalized recording and can_exit() is true.
    NSApplicationTerminateReply::TerminateCancel
}

pub fn install_termination_handler(ctx: &egui::Context) -> Result<()> {
    let mtm = MainThreadMarker::new().context("AppKit требует главный поток")?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = app.delegate().context("Нет делегата AppKit")?;
    let object: &AnyObject = (*delegate).as_ref();
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        // winit 0.30 owns the delegate and requires its exact class. Add only
        // its missing optional termination method, preserving all winit events.
        // SAFETY: the callback and Q@:@ encoding match NSUInteger(id, SEL, id)
        // on 64-bit macOS. AppKit calls it on the main thread with live objects.
        unsafe {
            objc2::ffi::class_addMethod(
                std::ptr::from_ref(object.class()).cast_mut(),
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
