//! Identical to region_overlay/platform.rs and fisch_ash_ser's own float-
//! above-fullscreen trick -- see that file's doc comment for the reasoning.

#[cfg(target_os = "macos")]
pub fn make_float_above_fullscreen(cc: &eframe::CreationContext<'_>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSView, NSWindowCollectionBehavior};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    } else {
        eprintln!("[overlay] make_float_above_fullscreen: not on the main thread -- activation policy NOT set");
    }

    const NS_SCREEN_SAVER_WINDOW_LEVEL: isize = 1000;

    let Ok(handle) = cc.window_handle() else {
        eprintln!("[overlay] make_float_above_fullscreen: cc.window_handle() failed -- window level NOT set, overlay will behave like a normal window");
        return;
    };
    let RawWindowHandle::AppKit(appkit_handle) = handle.as_raw() else {
        eprintln!("[overlay] make_float_above_fullscreen: window handle isn't AppKit (unexpected on macOS) -- window level NOT set");
        return;
    };

    // SAFETY: eframe/winit guarantee ns_view points at a live NSView for as
    // long as the window exists, and we're on the main thread inside the
    // creation callback.
    let ns_view: &NSView = unsafe { &*(appkit_handle.ns_view.as_ptr() as *mut NSView) };
    let Some(window) = ns_view.window() else {
        eprintln!("[overlay] make_float_above_fullscreen: ns_view.window() returned None -- window level NOT set");
        return;
    };

    window.setLevel(NS_SCREEN_SAVER_WINDOW_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    println!("[overlay] make_float_above_fullscreen: applied window level {NS_SCREEN_SAVER_WINDOW_LEVEL} + CanJoinAllSpaces/FullScreenAuxiliary successfully");
}

#[cfg(not(target_os = "macos"))]
pub fn make_float_above_fullscreen(_cc: &eframe::CreationContext<'_>) {}
