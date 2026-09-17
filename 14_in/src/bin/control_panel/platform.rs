//! Adapted from mco_spr's platform.rs: makes this window float above a
//! fullscreen Roblox window instead of switching Spaces when it
//! activates — accessory activation policy + a high window level +
//! canJoinAllSpaces/fullScreenAuxiliary collection behavior.
//!
//! Unlike mco_spr, this one uses NSScreenSaverWindowLevel (1000), not
//! NSFloatingWindowLevel (3). mco_spr was dialed down to floating level
//! specifically because Finder's drag-and-drop session was getting
//! excluded at the more aggressive level — but this app doesn't need
//! drag-and-drop, and does need to reliably render above an actual
//! fullscreen game, which floating level wasn't strong enough for (this
//! is what "app doesn't seem to open, especially on Roblox" turned out
//! to be: it opened fine, just invisible underneath the game).

#[cfg(target_os = "macos")]
pub fn make_float_above_fullscreen(cc: &eframe::CreationContext<'_>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSView, NSWindowCollectionBehavior};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }

    const NS_SCREEN_SAVER_WINDOW_LEVEL: isize = 1000;

    let Ok(handle) = cc.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit_handle) = handle.as_raw() else { return };

    // SAFETY: eframe/winit guarantee ns_view points at a live NSView for as
    // long as the window exists, and we're on the main thread inside the
    // creation callback.
    let ns_view: &NSView = unsafe { &*(appkit_handle.ns_view.as_ptr() as *mut NSView) };
    let Some(window) = ns_view.window() else { return };

    window.setLevel(NS_SCREEN_SAVER_WINDOW_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
}

#[cfg(not(target_os = "macos"))]
pub fn make_float_above_fullscreen(_cc: &eframe::CreationContext<'_>) {}
