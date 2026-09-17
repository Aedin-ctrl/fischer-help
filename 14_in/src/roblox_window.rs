//! Finds the Roblox window by name, regardless of what's currently
//! focused.
//!
//! active_win_pos_rs (used elsewhere in this bot for the frontmost-app
//! focus check) only exposes the single CURRENTLY-FOCUSED window, with
//! no way to look up a specific app. That's fine for "is Roblox focused
//! right now" during the bot's own loop, but it's the wrong tool for
//! calibrate_rod/region_overlay: those get launched by clicking a button
//! in control_panel, which makes control_panel itself the frontmost
//! window at that exact instant — so "grab whatever's frontmost" grabs
//! control_panel's bounds, not Roblox's, and the resulting overlay ends
//! up positioned somewhere wrong, tiny, or off-screen. That was the "the
//! region never appears" bug.
//!
//! xcap (already a dependency, used for screen capture) can enumerate
//! ALL windows, so this looks Roblox up by title directly, independent
//! of focus — no need for the user to alt-tab first, and no race
//! condition with whatever window happened to be active.

#[derive(Clone, Copy)]
pub struct WindowBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn find_roblox_window() -> Option<WindowBounds> {
    let windows = xcap::Window::all().ok()?;
    windows
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(false))
        .find(|w| w.title().unwrap_or_default().to_lowercase().contains("roblox"))
        .map(|w| WindowBounds {
            x: w.x().unwrap_or(0) as f64,
            y: w.y().unwrap_or(0) as f64,
            width: w.width().unwrap_or(0) as f64,
            height: w.height().unwrap_or(0) as f64,
        })
}
