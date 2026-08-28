//! Platform-specific window tricks. Only macOS is implemented for real —
//! everywhere else this is a no-op so the crate still builds.

#[cfg(target_os = "macos")]
pub fn make_float_above_fullscreen(cc: &eframe::CreationContext<'_>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSView, NSWindowCollectionBehavior};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // The window-level/collection-behavior trick alone isn't enough: a
    // normal foreground app always switches you to its own Space when its
    // window activates, which is exactly the "opens on the desktop and
    // switches my view there" symptom. Floating overlays that actually
    // stay put over a fullscreen app (capture bars, recording overlays,
    // etc.) are accessory-policy apps — no Dock icon, no Space-switching
    // on activation. Set that first.
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }

    // NSFloatingWindowLevel — the standard level for utility panels that
    // sit above normal app windows. The earlier attempt used the much
    // more extreme NSScreenSaverWindowLevel (1000), which likely also
    // explains drag-and-drop not registering: AppKit tends to exclude
    // windows at that level from normal Finder drag sessions.
    const NS_FLOATING_WINDOW_LEVEL: isize = 3;

    let Ok(handle) = cc.window_handle() else {
        eprintln!("mco_spr: no window handle available; skipping float-above-fullscreen setup");
        return;
    };

    let RawWindowHandle::AppKit(appkit_handle) = handle.as_raw() else {
        eprintln!("mco_spr: not an AppKit window handle; skipping float-above-fullscreen setup");
        return;
    };

    // SAFETY: eframe/winit guarantee ns_view points at a live NSView for as
    // long as the window exists, and we're on the main thread inside the
    // creation callback.
    let ns_view: &NSView = unsafe { &*(appkit_handle.ns_view.as_ptr() as *mut NSView) };

    let Some(window) = ns_view.window() else {
        eprintln!("mco_spr: NSView has no backing NSWindow yet; skipping float-above-fullscreen setup");
        return;
    };

    window.setLevel(NS_FLOATING_WINDOW_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
}

#[cfg(not(target_os = "macos"))]
pub fn make_float_above_fullscreen(_cc: &eframe::CreationContext<'_>) {
    // Not implemented on this platform yet.
}
