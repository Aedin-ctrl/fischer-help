//! Autonomous Bar Bot (macOS, terminal) — same as main.rs, plus rod-reset/
//! re-seat/totems/skip-day-night orchestration driven by settings.toml.
//! Tracking now uses control::run_predictive_control_until_gone() (see
//! that function's doc comment for the physics model) instead of this
//! file's original 5-point white-flash bang-bang controller. The
//! bang-bang code (run_bang_bang_control() and everything it uses) is
//! kept below, unused, as a fallback/reference rather than deleted —
//! hence the file-level dead_code allow.
//!
//! macOS: grant Screen Recording + Accessibility to your terminal app in
//! System Settings -> Privacy & Security, then restart the terminal.
//!
//! CONTROLS
//! --------
//! F8  = pause/resume the whole loop (safe to hit anytime)
//! ESC = quit immediately (always releases the mouse/key first)

#![allow(dead_code)]

use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use image::RgbaImage;
use rdev::{Button, Key};
use xcap::Monitor;

use fish_bar_bot::config::HsvRange;
use fish_bar_bot::{calibration, capture, config, control, input};

// ----------------------------------------------------------------------
// 5-POINT BAR SAMPLING
// ----------------------------------------------------------------------
//
// Five fixed x-positions along the control-bar track (same y), ordered
// far-right to far-left. The bar occupies ~55% of the track's width, and
// these five points were picked so that wherever the bar currently sits,
// at least two of them fall under it — so we never need to track the
// bar's actual edges, just sample these fixed spots.
//
// Stored as fractions of the region (not absolute pixels) so they still
// land on the bar regardless of device/resolution — resolved against
// calibration::region() at runtime instead. These fractions are just the
// original absolute values (994, 784, 721, 660, 445, y=805) divided
// against the old hardcoded config::REGION (426, 785, 589x35); no new
// calibration data needed since the ratios don't change.
const SAMPLE_POINTS_X_FRAC: [f64; 5] = [0.964346, 0.607810, 0.500849, 0.397284, 0.032258];
const SAMPLE_Y_FRAC: f64 = 0.571429;

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f64, g as f64, b as f64);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;

    let v = max;
    let s = if max == 0.0 { 0.0 } else { 255.0 * delta / max };

    let mut h = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    h /= 2.0; // fit OpenCV's 0..=179 8-bit hue range

    (h.round() as u8, s.round() as u8, v.round() as u8)
}

fn in_range(hsv: (u8, u8, u8), range: &HsvRange) -> bool {
    let (low, high) = *range;
    hsv.0 >= low.0 && hsv.0 <= high.0 && hsv.1 >= low.1 && hsv.1 <= high.1 && hsv.2 >= low.2 && hsv.2 <= high.2
}

/// Samples a 3x3 pixel neighborhood around (cx, cy) and reports it a match
/// if a majority of the (up to 9) in-bounds pixels fall in any of `ranges`
/// — smooths out single-pixel anti-aliasing/compression noise at each
/// sample point.
fn point_matches(frame: &RgbaImage, cx: i32, cy: i32, ranges: &[HsvRange]) -> bool {
    let mut total = 0u32;
    let mut hits = 0u32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as u32 >= frame.width() || y as u32 >= frame.height() {
                continue;
            }
            total += 1;
            let p = frame.get_pixel(x as u32, y as u32);
            let hsv = rgb_to_hsv(p[0], p[1], p[2]);
            if ranges.iter().any(|r| in_range(hsv, r)) {
                hits += 1;
            }
        }
    }
    total > 0 && hits * 2 >= total
}

struct BarSample {
    /// How many of the 5 points read white ("flash" / engaged with the needle).
    white_count: u32,
    /// How many of the 5 points read as either bar color (white or dim/brown)
    /// at all — used only to tell whether the bar is on screen anymore.
    bar_present_count: u32,
}

fn sample_bar(frame: &RgbaImage, region: config::Region, white_range: HsvRange, bar_ranges: &[HsvRange]) -> BarSample {
    let mut white_count = 0;
    let mut bar_present_count = 0;
    let cy = (SAMPLE_Y_FRAC * region.height as f64).round() as i32;
    for &frac_x in &SAMPLE_POINTS_X_FRAC {
        let cx = (frac_x * region.width as f64).round() as i32;
        if point_matches(frame, cx, cy, std::slice::from_ref(&white_range)) {
            white_count += 1;
        }
        if point_matches(frame, cx, cy, bar_ranges) {
            bar_present_count += 1;
        }
    }
    BarSample { white_count, bar_present_count }
}

const RESEAT_SPACE_HOLD: Duration = Duration::from_millis(120);
const RESEAT_POST_SPACE_DELAY: Duration = Duration::from_millis(800);
const RESEAT_E_HOLD: Duration = Duration::from_millis(500);

/// Taps space, waits, then holds 'e' briefly. Independent of rod-reset --
/// doesn't touch the rod slot at all.
fn run_reseat_action() {
    println!("\nRe-seating: space, then holding 'e'...");
    if input::is_stopped() {
        return;
    }
    input::tap_key(Key::Space, RESEAT_SPACE_HOLD);
    thread::sleep(RESEAT_POST_SPACE_DELAY);

    if input::is_stopped() {
        return;
    }
    input::tap_key(Key::KeyE, RESEAT_E_HOLD);
}

/// Presses the configured rod-reset slot, then the rod slot -- the same
/// lead-in the normal cast sequence does itself. Run this standalone,
/// then call control::run_trigger_sequence(true) so the cast doesn't
/// press them again on top of this.
fn run_rod_reset_action(reset_key: Key, rod_key: Key) {
    println!("\nRod-reset: pressing reset slot, then rod slot...");
    for key in [reset_key, rod_key] {
        if input::is_stopped() {
            return;
        }
        input::tap_key(key, config::PRE_TRIGGER_KEY_HOLD);
        thread::sleep(config::PRE_TRIGGER_KEY_DELAY);
    }
}

const TOTEM_KEY_HOLD: Duration = Duration::from_millis(120);
const TOTEM_SWITCH_DELAY: Duration = Duration::from_millis(300);
const TOTEM_CLICK_HOLD: Duration = Duration::from_millis(200);

/// Presses the totem slot, clicks screen center (places/refreshes it),
/// then switches back to the rod slot. No detection -- just the sequence.
fn run_totem_action(monitor: &Monitor, totem_key: Key, rod_key: Key) {
    println!("\nRefreshing totem: pressing totem slot, clicking center, switching back to rod slot...");
    input::tap_key(totem_key, TOTEM_KEY_HOLD);
    thread::sleep(TOTEM_SWITCH_DELAY);

    let center_x = monitor.x().unwrap_or(0) + monitor.width().unwrap_or(0) as i32 / 2;
    let center_y = monitor.y().unwrap_or(0) + monitor.height().unwrap_or(0) as i32 / 2;
    input::mouse_move(center_x as f64, center_y as f64);
    input::mouse_down(Button::Left);
    thread::sleep(TOTEM_CLICK_HOLD);
    input::mouse_up(Button::Left);
    thread::sleep(TOTEM_SWITCH_DELAY);

    input::tap_key(rod_key, TOTEM_KEY_HOLD);
}

/// A single instantaneous frame can catch the icon mid-animation-dip and
/// miss it (real captures showed the match score occasionally dropping
/// below threshold for one frame even while the icon was genuinely on
/// screen) -- retry for up to ICON_CHECK_RETRY_WINDOW before giving up.
const ICON_CHECK_RETRY_WINDOW: Duration = Duration::from_millis(500);
const ICON_CHECK_RETRY_INTERVAL: Duration = Duration::from_millis(100);

fn icon_visible_with_retries(template: Option<&RgbaImage>) -> bool {
    let start = Instant::now();
    loop {
        if control::is_icon_visible(template) {
            return true;
        }
        if input::is_stopped() || start.elapsed() >= ICON_CHECK_RETRY_WINDOW {
            return false;
        }
        thread::sleep(ICON_CHECK_RETRY_INTERVAL);
    }
}

// ----------------------------------------------------------------------
// BANG-BANG CONTROL LOOP — replaces control::run_control_until_gone()
// ----------------------------------------------------------------------
//
// The control bar has two rest positions (fully left / fully right) and
// takes TRANSITION_DURATION to swing fully from one to the other. Instead
// of tracking the needle's real position, we just watch for the bar
// flashing white (== needle currently aligned with it, see sample_bar):
//
//   Settled (left or right): poll slowly. As soon as the flash stops
//   (white_count < 2), start swinging toward the other side.
//
//   Transitioning: poll as fast as possible for a flash. A single flash
//   at any point confirms this swing -- let it finish (no input change
//   needed, we're already holding the right key state), settle on the
//   far side, then hold there untouched for SETTLE_GRACE before going
//   back to slow polling. If the full TRANSITION_DURATION elapses with
//   no flash at all, reverse direction and keep hunting.
//
// Before the fish ever bites, the bar hasn't appeared on screen yet at
// all -- that natural wait is unbounded (can easily run 10+ real seconds)
// and must NOT be mistaken for "fight over." So the give-up rule only
// arms once we've confirmed a flash at least once this session: from then
// on, if a full unconfirmed swing cycle (there and back, 2 legs) passes
// with no re-confirm, the fight is considered over and this function
// returns so main() can recast. Before that first confirm, we just keep
// swinging, bounded only by NO_BITE_TIMEOUT as a generous safety net.

const TRANSITION_DURATION: Duration = Duration::from_millis(800);
/// Extra slack tacked onto the end of each leg's flash-watching window.
/// When the needle sits right at the far edge of the track, the bar only
/// overlaps it as it reaches the wall -- i.e. right at the tail end of the
/// nominal 800ms, and capture/physics latency can push the real overlap a
/// touch past that. Without this buffer, a flash landing just after the
/// 800ms cutoff gets missed and we reverse right as we were about to catch it.
const TRANSITION_CONFIRM_BUFFER: Duration = Duration::from_millis(150);
const SETTLE_GRACE: Duration = Duration::from_millis(150);
const SETTLED_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// One full there-and-back swing cycle (2 unconfirmed legs) after having
/// caught the flash at least once before -- that means the fight legitimately
/// ended (fish caught/escaped/line broke), so give up and recast.
const LEGS_PER_GIVE_UP: u32 = 2;
/// Safety net for the case we've NEVER caught a flash yet this session --
/// covers the natural pre-bite wait (which can genuinely run 10+ seconds),
/// so this only fires if something is actually broken, not mid-wait.
const NO_BITE_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

fn print_status(line: &str) {
    print!("{line}    \r");
    let _ = io::stdout().flush();
}

/// Holds the key state matching "heading toward" a given side: pressed and
/// held while accelerating right, released while drifting/settling left.
fn set_heading(side: Side) {
    match side {
        Side::Right => input::key_down(config::HOLD_KEY),
        Side::Left => input::key_up(config::HOLD_KEY),
    }
}

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

/// Bails out to the caller (return false) on ESC, or pauses/waits out any
/// focus loss (releasing the key first) and returns true once clear to
/// keep going.
fn stay_active() -> bool {
    if input::is_stopped() {
        return false;
    }
    if input::is_paused() || !input::frontmost_app_ok() {
        input::key_up(config::HOLD_KEY);
        return input::wait_while_paused_or_unfocused();
    }
    true
}

/// Runs the 5-point flash-based bang-bang controller until the bar is
/// gone for BAR_GONE_TIMEOUT (fight over) or the bot is stopped.
fn run_bang_bang_control() {
    println!("Bang-bang control engaged (5-point white-flash tracking)...");

    let region = calibration::region();
    let bar_ranges = calibration::bar_hsv_ranges();
    // Calibration's derived ranges are sorted by hue, not by "which one is
    // the bright/white one" — unlike config.rs's hand-ordered defaults, so
    // bar_ranges[0] isn't reliably the white/flash range. Keep that one
    // tied to the known-correct hardcoded value; only bar presence
    // (order-independent) comes from calibration.
    let white_range = config::CONTROLLED_BAR_HSV_RANGES[0];

    let mut side = Side::Left;
    set_heading(side);
    let mut ever_confirmed = false;
    let session_start = Instant::now();

    'outer: loop {
        if !stay_active() {
            return;
        }

        // -- SETTLED: poll slowly until the flash drops out --
        loop {
            if !stay_active() {
                return;
            }
            let Ok(frame) = capture::grab_region_normalized(region) else {
                thread::sleep(SETTLED_POLL_INTERVAL);
                continue;
            };
            let sample = sample_bar(&frame, region, white_range, bar_ranges);

            print_status(&format!("[settled {side:?}] white={}/5 bar={}/5", sample.white_count, sample.bar_present_count));

            if sample.white_count < 2 {
                break; // lost the flash -- start hunting toward the other side
            }
            ever_confirmed = true;
            if !checked_sleep(SETTLED_POLL_INTERVAL) {
                return;
            }
        }

        // -- TRANSITIONING: swing leg by leg until a flash confirms one --
        let mut heading = side.other();
        set_heading(heading);
        let mut unconfirmed_legs = 0u32;

        loop {
            if !stay_active() {
                return;
            }
            let leg_start = Instant::now();
            let mut confirmed = false;

            while leg_start.elapsed() < TRANSITION_DURATION + TRANSITION_CONFIRM_BUFFER {
                if !stay_active() {
                    return;
                }
                let Ok(frame) = capture::grab_region_normalized(region) else {
                    continue;
                };
                let sample = sample_bar(&frame, region, white_range, bar_ranges);

                print_status(&format!(
                    "[transit -> {heading:?}] white={}/5 bar={}/5 {}",
                    sample.white_count,
                    sample.bar_present_count,
                    if confirmed { "CONFIRMED" } else { "" }
                ));

                if !confirmed && sample.white_count >= 2 {
                    confirmed = true; // single flash is enough -- let this leg finish naturally
                }
            }

            if confirmed {
                ever_confirmed = true;
                side = heading;
                break;
            }

            unconfirmed_legs += 1;

            if ever_confirmed && unconfirmed_legs >= LEGS_PER_GIVE_UP {
                println!("\nNo flash for a full swing cycle after catching it before -- fight over.");
                input::key_up(config::HOLD_KEY);
                return;
            }
            if !ever_confirmed && session_start.elapsed() >= NO_BITE_TIMEOUT {
                println!("\nNo bite within {:.0}s -- recasting anyway.", NO_BITE_TIMEOUT.as_secs_f64());
                input::key_up(config::HOLD_KEY);
                return;
            }

            // No flash this whole leg -- reverse and keep hunting.
            heading = heading.other();
            set_heading(heading);
        }

        // Confirmed catch: hold steady for one extra full second before
        // resuming the slow settled-poll loop.
        if !checked_sleep(SETTLE_GRACE) {
            return;
        }
        continue 'outer;
    }
}

fn main() -> anyhow::Result<()> {
    // Safety net: if anything panics anywhere in the loop below, release the
    // held key before unwinding -- otherwise a crash mid-hold leaves Space
    // stuck down in-game forever (this is what caused the "stuck jumping"
    // bug: a panic unwound straight past the key_up() at the bottom of main).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        input::key_up(Key::Space);
        default_hook(info);
    }));

    let settings = fish_bar_bot::settings::load();
    let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
    let rod_reset_where_key = fish_bar_bot::settings::key_for_slot(settings.rod_reset_where).unwrap_or(Key::Num9);
    let totems_where_key = fish_bar_bot::settings::key_for_slot(settings.totems_where).unwrap_or(Key::Num4);
    let skip_day_night_where_key = fish_bar_bot::settings::key_for_slot(settings.skip_day_night_where).unwrap_or(Key::Num5);
    println!(
        "settings: rod={} rod_reset={} (where={} every {}) skip={:?} (where={} every {}) totems={} (where={} every {}) reseat={} aquarium_boost={} (every {})",
        settings.rod_slot,
        settings.rod_reset_enabled,
        settings.rod_reset_where,
        settings.rod_reset_count,
        settings.skip_day_night,
        settings.skip_day_night_where,
        settings.skip_day_night_count,
        settings.use_totems_enabled,
        settings.totems_where,
        settings.totems_count,
        settings.reseat_enabled,
        settings.aquarium_boost_enabled,
        settings.aquarium_boost_count,
    );

    // No countdown here anymore -- control_panel shows its own 5s visible
    // countdown before spawning this process at all, and this process's
    // own println-based countdown was never actually visible in GUI mode
    // (stdout has nowhere to go), so it was just a silent extra delay.
    println!("Autonomous bar bot running. F8 = pause/resume, ESC = quit.");
    input::start_hotkey_listener();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    // Fall back to the source-tree location so `cargo run` (whose exe lives
    // under target/debug) still finds moon.png/sun.png next to main.rs's
    // project root during development.
    let asset_dir = if exe_dir.join("moon.png").exists() || exe_dir.join("sun.png").exists() {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };

    // Only load the one icon template we actually need to watch for -- if
    // skipping nothing, never load either, so the periodic check below
    // never has anything to trigger on.
    let watch_template = match settings.skip_day_night {
        Some(fish_bar_bot::settings::DayNight::Day) => control::load_icon_template(&asset_dir.join("sun.png"), "sun"),
        Some(fish_bar_bot::settings::DayNight::Night) => control::load_icon_template(&asset_dir.join("moon.png"), "moon"),
        None => None,
    };

    let monitor = capture::find_primary_monitor()?;
    let mut session_count: u32 = 0;
    let mut totem_count: u32 = 0;
    let mut rod_reset_count: u32 = 0;
    let mut aquarium_count: u32 = 0;
    let rod_reset_every = settings.rod_reset_count.max(1);
    let skip_day_night_every = settings.skip_day_night_count.max(1);
    let totems_every = settings.totems_count.max(1);
    let aquarium_every = settings.aquarium_boost_count.max(1);

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        if settings.skip_day_night.is_some() && session_count >= skip_day_night_every {
            session_count = 0;
            if icon_visible_with_retries(watch_template.as_ref()) {
                control::run_moon_action(&monitor, skip_day_night_where_key, rod_key);
            }
            // Full delay before the cast either way.
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        if settings.use_totems_enabled && totem_count >= totems_every {
            totem_count = 0;
            run_totem_action(&monitor, totems_where_key, rod_key);
        }

        // Either action already covers the normal cast's own rod-slot
        // lead-in -- re-seat replaces the need for it, rod-reset presses
        // it directly -- so skip the cast's built-in press whenever
        // either one ran.
        let mut skip_cast_pre_trigger = false;

        if settings.reseat_enabled {
            run_reseat_action();
            if input::is_stopped() {
                break;
            }
            skip_cast_pre_trigger = true;
        }

        if settings.rod_reset_enabled && rod_reset_count >= rod_reset_every {
            rod_reset_count = 0;
            run_rod_reset_action(rod_reset_where_key, rod_key);
            if input::is_stopped() {
                break;
            }
            skip_cast_pre_trigger = true;
        }

        control::run_trigger_sequence(skip_cast_pre_trigger, None);
        if input::is_stopped() {
            break;
        }

        // Physics-aware predictive controller (see control.rs) instead of
        // the old 5-point bang-bang. The bang-bang code below is kept in
        // place, unused, as a fallback/reference — not deleted — in case
        // the new one needs more real-world tuning.
        control::run_predictive_control_until_gone();
        session_count += 1;
        totem_count += 1;
        rod_reset_count += 1;

        aquarium_count += 1;
        if settings.aquarium_boost_enabled && aquarium_count >= aquarium_every {
            aquarium_count = 0;
            fish_bar_bot::aquarium::run_aquarium_boost(rod_key);
        }

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(Key::Space);
    println!("Stopped.");
    Ok(())
}
