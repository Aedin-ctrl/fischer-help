//! "Aquarium boost" click/scroll sequence, tuned and verified standalone via
//! bin/aquarium_boost.rs before being wired into the casting loops.
//!
//! Sequence: switch off the rod (press '9'), click the aquarium menu
//! button, wait, click the item, scroll far to one side, then BOOST_PASSES
//! passes of {click, small scroll the other way, click, LOOP_DELAY}, then
//! scroll back, then CLOSE_CLICKS clicks on the close button (spaced
//! CLOSE_CLICK_DELAY apart) to close it, then switch back to the rod slot.
//!
//! The rod-slot switch matters more than it looks: hotbar slots are a
//! toggle, not a select -- pressing the slot you're ALREADY holding
//! unselects it (holding nothing) instead of re-confirming it. So closing
//! with just "press rod slot" is only safe if you know for certain you're
//! not already on it; switching to a different slot first (9) guarantees
//! that, since a different-slot press always ends up holding that slot
//! regardless of what you started on. The failure mode this guards
//! against: if the '9' press doesn't register, the closing '1' lands on
//! "already holding rod" and toggles it OFF instead of confirming it.

use std::thread;
use std::time::{Duration, Instant};

use rdev::{Button, Key};

use crate::{calibration, input};

/// Fallback points -- absolute screen pixels tuned on the original dev
/// machine, used until the matching circle is dragged into place via
/// control_panel's "Aquarium" section (point_overlay --which menu|item|close).
/// Public so point_overlay can use the same fallback for its own initial
/// on-screen placement before anything's been calibrated.
pub const DEFAULT_MENU_BUTTON: (f64, f64) = (829.0, 64.0);
pub const DEFAULT_AQUARIUM_ITEM: (f64, f64) = (646.0, 655.0);
pub const DEFAULT_CLOSE_BUTTON: (f64, f64) = (1152.0, 180.0);

/// Hotbar slot switched to (away from the rod) before clicking the UI --
/// clicking with the rod still equipped was interfering with the aquarium
/// clicks. Not a settings.toml field since it's always '9', same as
/// rod-reset's default.
const SWITCH_AWAY_KEY: Key = Key::Num9;
/// Held for 0.2s specifically to give the game time to register the
/// switch-away press -- a too-short tap was the actual cause of the
/// "randomly ends up holding nothing" issue (see module doc above).
const SWITCH_KEY_HOLD: Duration = Duration::from_millis(200);
/// Settle time after switching off/back onto the rod slot, before the next
/// action.
const SWITCH_SETTLE_DELAY: Duration = Duration::from_millis(400);

/// How long the initial menu-open press is held before releasing.
const MENU_OPEN_HOLD: Duration = Duration::from_millis(300);
const POST_OPEN_DELAY: Duration = Duration::from_millis(600);
const LOOP_DELAY: Duration = Duration::from_millis(500);
const CLOSE_CLICK_DELAY: Duration = Duration::from_millis(300);
const BOOST_PASSES: u32 = 10;
const CLOSE_CLICKS: u32 = 3;

const SCROLL_BIG_LEFT: i64 = 3000;
const SCROLL_SMALL_RIGHT: i64 = -113; // 3/4 of the original 150
/// Scrolls back to the far right at the end. The BOOST_PASSES small
/// right-scrolls already claw back some of SCROLL_BIG_LEFT's distance
/// (10 * 113 = 1130), so undoing it with the same magnitude undershoots --
/// this needs to cover the full original distance *plus* however far the
/// passes already walked back, so it's set well past worst case instead of
/// computed exactly.
const SCROLL_BACK_RIGHT: i64 = -6000;
const PRE_PASSES_BREAK: Duration = Duration::from_millis(500);
const POST_SCROLL_BACK_BREAK: Duration = Duration::from_millis(500);

/// Calibrated position if the matching point's been dragged into place,
/// else the hardcoded default.
fn menu_button() -> (f64, f64) {
    let cal = calibration::load();
    cal.and_then(|c| calibration::resolve_point(c.aquarium_menu_x_frac, c.aquarium_menu_y_frac)).unwrap_or(DEFAULT_MENU_BUTTON)
}

fn aquarium_item() -> (f64, f64) {
    let cal = calibration::load();
    cal.and_then(|c| calibration::resolve_point(c.aquarium_item_x_frac, c.aquarium_item_y_frac)).unwrap_or(DEFAULT_AQUARIUM_ITEM)
}

fn close_button() -> (f64, f64) {
    let cal = calibration::load();
    cal.and_then(|c| calibration::resolve_point(c.aquarium_close_x_frac, c.aquarium_close_y_frac)).unwrap_or(DEFAULT_CLOSE_BUTTON)
}

/// Sleeps in small increments so ESC is noticed promptly mid-wait.
fn checked_sleep(duration: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < duration {
        if input::is_stopped() {
            return false;
        }
        thread::sleep(Duration::from_millis(20).min(duration.saturating_sub(start.elapsed())));
    }
    true
}

pub fn run_aquarium_boost(rod_key: Key) {
    let menu = menu_button();
    let item = aquarium_item();
    let close = close_button();

    println!("aquarium boost: switching off rod (pressing '9')...");
    input::tap_key(SWITCH_AWAY_KEY, SWITCH_KEY_HOLD);
    if !checked_sleep(SWITCH_SETTLE_DELAY) {
        return;
    }

    println!("aquarium boost: opening menu at {menu:?}...");
    input::mouse_move(menu.0, menu.1);
    input::mouse_down(Button::Left);
    if !checked_sleep(MENU_OPEN_HOLD) {
        return;
    }
    input::mouse_up(Button::Left);
    if !checked_sleep(POST_OPEN_DELAY) {
        return;
    }

    println!("aquarium boost: clicking item and scrolling far left...");
    input::click(item.0, item.1);
    input::scroll(SCROLL_BIG_LEFT, 0);
    if !checked_sleep(PRE_PASSES_BREAK) {
        return;
    }

    for i in 1..=BOOST_PASSES {
        if input::is_stopped() {
            println!("\nstopped mid-boost.");
            return;
        }
        println!("  pass {i}/{BOOST_PASSES}");
        input::click(item.0, item.1);
        input::scroll(SCROLL_SMALL_RIGHT, 0);
        input::click(item.0, item.1);
        if !checked_sleep(LOOP_DELAY) {
            return;
        }
    }

    println!("aquarium boost: scrolling back...");
    input::click(item.0, item.1);
    input::scroll(SCROLL_BACK_RIGHT, 0);
    if !checked_sleep(POST_SCROLL_BACK_BREAK) {
        return;
    }

    println!("aquarium boost: closing menu at {close:?}...");
    for i in 1..=CLOSE_CLICKS {
        input::click(close.0, close.1);
        if i < CLOSE_CLICKS && !checked_sleep(CLOSE_CLICK_DELAY) {
            return;
        }
    }

    if !checked_sleep(SWITCH_SETTLE_DELAY) {
        return;
    }
    println!("aquarium boost: switching back to rod...");
    input::tap_key(rod_key, SWITCH_KEY_HOLD);
    println!("aquarium boost: done.");
}
