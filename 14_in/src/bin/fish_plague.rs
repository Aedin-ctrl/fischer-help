//! Autonomous Bar Bot (macOS, terminal) — the simplest variant: no bar
//! tracking, no casting sequence, no calibration. Just holds the left
//! mouse button down for 0.3s and releases it for 0.3s, forever, with a
//! periodic aquarium boost. For minigames/rods that don't need anything
//! smarter than a steady click-hold rhythm.
//!
//! macOS: grant Accessibility to your terminal app in System Settings ->
//! Privacy & Security, then restart the terminal. (Screen Recording isn't
//! needed here -- this bot never captures the screen.)
//!
//! CONTROLS
//! --------
//! F8  = pause/resume the whole loop (safe to hit anytime)
//! ESC = quit immediately (always releases the mouse first)

use std::thread;
use std::time::{Duration, Instant};

use rdev::{Button, Key};

use fish_bar_bot::{aquarium, input};

const HOLD_DURATION: Duration = Duration::from_millis(200);
const RELEASE_DURATION: Duration = Duration::from_millis(200);
const AQUARIUM_INTERVAL: Duration = Duration::from_secs(31 * 60);
/// Gap between pausing the click loop and actually starting the aquarium
/// boost, so the game has a moment to settle out of the click rhythm
/// first instead of the boost sequence starting right off a click.
const PRE_AQUARIUM_PAUSE: Duration = Duration::from_secs(4);

/// Sleeps in small increments so ESC/pause are noticed promptly. Returns
/// false if the wait was cut short by a stop request.
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

/// Bails out to the caller (return false) on ESC, or releases the mouse
/// and pauses/waits out any focus loss, returning true once clear to keep
/// going.
fn stay_active() -> bool {
    if input::is_stopped() {
        return false;
    }
    if input::is_paused() || !input::frontmost_app_ok() {
        input::mouse_up(Button::Left);
        return input::wait_while_paused_or_unfocused();
    }
    true
}

/// Formats a Duration as H:MM:SS (or M:SS under an hour) for the session
/// elapsed-time readout.
fn format_elapsed(d: Duration) -> String {
    let total_secs = d.as_secs();
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn main() {
    // Safety net: if anything panics anywhere in the loop below, release
    // the held mouse button before unwinding -- otherwise a crash mid-hold
    // leaves it stuck down in-game forever.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        input::mouse_up(Button::Left);
        default_hook(info);
    }));

    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    println!("Plague bot running (click-hold, {}ms/{}ms). F8 = pause/resume, ESC = quit.", HOLD_DURATION.as_millis(), RELEASE_DURATION.as_millis());
    input::start_hotkey_listener();

    let session_start = Instant::now();
    let mut last_aquarium = Instant::now();

    'outer: loop {
        if !stay_active() {
            break;
        }

        if last_aquarium.elapsed() >= AQUARIUM_INTERVAL {
            input::mouse_up(Button::Left);
            println!("\n{} elapsed -- pausing clicks, aquarium boost in {}s...", format_elapsed(session_start.elapsed()), PRE_AQUARIUM_PAUSE.as_secs());
            if !checked_sleep(PRE_AQUARIUM_PAUSE) {
                break;
            }
            let settings = fish_bar_bot::settings::load();
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            aquarium::run_aquarium_boost(rod_key);
            last_aquarium = Instant::now();
            continue 'outer;
        }

        input::mouse_down(Button::Left);
        if !checked_sleep(HOLD_DURATION) {
            break;
        }
        input::mouse_up(Button::Left);
        if !checked_sleep(RELEASE_DURATION) {
            break;
        }
    }

    input::mouse_up(Button::Left);
    println!("\nStopped. Total time running: {}", format_elapsed(session_start.elapsed()));
}
