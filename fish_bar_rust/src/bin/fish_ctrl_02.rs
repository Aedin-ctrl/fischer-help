//! Autonomous Bar Bot (macOS, terminal) — same as main.rs, but the
//! reactive control-bar/needle tracking loop (control::run_control_until_gone)
//! is replaced with a 5-point white-flash bang-bang controller — see
//! run_bang_bang_control() below.
//!
//! macOS: grant Screen Recording + Accessibility to your terminal app in
//! System Settings -> Privacy & Security, then restart the terminal.
//!
//! CONTROLS
//! --------
//! F8  = pause/resume the whole loop (safe to hit anytime)
//! ESC = quit immediately (always releases the mouse/key first)

use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use image::RgbaImage;
use rdev::Key;
use xcap::Monitor;

use fish_bar_bot::config::HsvRange;
use fish_bar_bot::{aquarium, calibration, capture, config, control, input};

/// Extra Enter presses tacked on after each normal cast sequence, on top of
/// run_trigger_sequence's own burst.
const EXTRA_ENTER_COUNT: u32 = 0;

// ----------------------------------------------------------------------
// 5-POINT BAR SAMPLING
// ----------------------------------------------------------------------
//
// Five fixed x-positions along the control-bar track (same y), ordered
// far-right to far-left. The bar occupies ~55% of the track's width, and
// these five points were picked so that wherever the bar currently sits,
// at least two of them fall under it -- so we never need to track the
// bar's actual edges, just sample these fixed spots.
//
// Stored as fractions of the region (not absolute pixels) so they still
// land on the bar regardless of device/resolution/calibration -- resolved
// against calibration::region() at runtime instead, same as fisch_new.rs.
// These fractions are just the original absolute values (994, 784, 721,
// 660, 445, y=805) divided against the old hardcoded config::REGION (426,
// 785, 589x35); no new calibration data needed since the ratios don't change.
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

/// Average RGB of the (up to 9) in-bounds pixels in a 3x3 neighborhood
/// around (cx, cy) -- like point_matches, but returns the raw averaged
/// color instead of a boolean HSV-range match. Used by bar_looks_gone()
/// below to sanity-check the bar's presence independent of the calibrated
/// HSV ranges.
fn point_avg_rgb(frame: &RgbaImage, cx: i32, cy: i32) -> (f64, f64, f64) {
    let (mut r_sum, mut g_sum, mut b_sum, mut n) = (0.0, 0.0, 0.0, 0u32);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as u32 >= frame.width() || y as u32 >= frame.height() {
                continue;
            }
            let p = frame.get_pixel(x as u32, y as u32);
            r_sum += p[0] as f64;
            g_sum += p[1] as f64;
            b_sum += p[2] as f64;
            n += 1;
        }
    }
    if n == 0 { (0.0, 0.0, 0.0) } else { (r_sum / n as f64, g_sum / n as f64, b_sum / n as f64) }
}

fn rgb_distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// Samples the raw average color (not an HSV-range match) at each of the 5
/// fixed bar points, same x/y fractions as sample_bar.
fn sample_point_colors(frame: &RgbaImage, region: config::Region) -> [(f64, f64, f64); 5] {
    let cy = (SAMPLE_Y_FRAC * region.height as f64).round() as i32;
    let mut colors = [(0.0, 0.0, 0.0); 5];
    for (i, &frac_x) in SAMPLE_POINTS_X_FRAC.iter().enumerate() {
        let cx = (frac_x * region.width as f64).round() as i32;
        colors[i] = point_avg_rgb(frame, cx, cy);
    }
    colors
}

/// While genuinely settled, the bar always covers at least 2 of the 5
/// points and leaves the rest on the background track -- so a normal
/// settled read is a MIX of (at least) two distinct colors. If instead all
/// 5 points read as roughly the same color, or all 5 read as mutually
/// distinct colors, that structured bar-vs-track pattern isn't there --
/// meaning the bar itself is gone (minigame closed) even if a stale/false
/// HSV-range match is still reporting a flash. Only checked once settled
/// and given time to stabilize -- see SETTLE_STABLE_DELAY.
const COLOR_SAME_THRESHOLD: f64 = 18.0;
const COLOR_DIFFERENT_THRESHOLD: f64 = 45.0;

fn bar_looks_gone(colors: &[(f64, f64, f64); 5]) -> bool {
    let mut all_same = true;
    let mut all_different = true;
    for i in 0..colors.len() {
        for j in (i + 1)..colors.len() {
            let d = rgb_distance(colors[i], colors[j]);
            if d > COLOR_SAME_THRESHOLD {
                all_same = false;
            }
            if d < COLOR_DIFFERENT_THRESHOLD {
                all_different = false;
            }
        }
    }
    all_same || all_different
}

#[derive(Clone, Copy, PartialEq)]
enum DayNightPref {
    Day,
    Night,
}

/// Ask which time of day to skip. 'd'/'day' watches for the sun icon (day)
/// and skips it -- net effect: only night. 'n'/'night' watches for the moon
/// icon (night) instead -- net effect: only day. Anything else (including
/// just Enter) skips nothing -- the day/night check is disabled entirely,
/// regardless of whatever control_panel's "skip day/night" setting says.
fn prompt_day_night_preference() -> Option<DayNightPref> {
    print!("What do you want to skip? Type d/day, n/night, or press Enter to skip nothing: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().to_lowercase().as_str() {
        "d" | "day" => {
            println!("okay, skipping day whenever it's detected");
            Some(DayNightPref::Day)
        }
        "n" | "night" => {
            println!("okay, skipping night whenever it's detected");
            Some(DayNightPref::Night)
        }
        _ => {
            println!("alright, skipping nothing — day/night check disabled");
            None
        }
    }
}

/// Hardcoded to 0 -- no Enter presses after casting, overriding
/// settings.cast_enter_count (control_panel's saved value, normally 12)
/// for this run only -- the saved settings.toml value is untouched.
const ENTER_COUNT_OVERRIDE: Option<u32> = Some(0);

/// Presses 9 then 1 (config::PRE_TRIGGER_KEYS) -- normally run before every
/// single cast (see control::run_trigger_sequence), but here it's only
/// wanted once at startup and again after anything that swaps hotbar slots
/// away from the rod (day/night action, aquarium boost, totem refresh), not
/// between every regular cast.
fn press_pre_trigger_keys() {
    for &(key, label) in config::PRE_TRIGGER_KEYS {
        if input::is_stopped() {
            return;
        }
        println!("  pressing '{label}'...");
        input::tap_key(key, config::PRE_TRIGGER_KEY_HOLD);
        thread::sleep(config::PRE_TRIGGER_KEY_DELAY);
    }
}

/// Ask how long to wait for a first bite before giving up and recasting
/// anyway (NO_BITE_TIMEOUT). Blank input keeps the standard amount (40s);
/// typing a number uses that many seconds instead -- e.g. 10 for much
/// faster recasting when needed.
fn prompt_no_bite_timeout_preference() -> Duration {
    print!("Max seconds to wait for a bite before recasting? Press Enter for the standard amount ({DEFAULT_NO_BITE_TIMEOUT_SECS}s), or type a number: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().parse::<u64>() {
        Ok(secs) => {
            println!("okay, recasting after {secs}s with no bite");
            Duration::from_secs(secs)
        }
        Err(_) => {
            println!("alright, using the standard {DEFAULT_NO_BITE_TIMEOUT_SECS}s");
            Duration::from_secs(DEFAULT_NO_BITE_TIMEOUT_SECS)
        }
    }
}

/// Ask which hotbar slot the sundial totem is in. Blank disables the
/// feature entirely; typing a number 1-9 enables it at that slot.
fn prompt_totem_preference() -> Option<Key> {
    print!("totems?: press Enter to skip, or type the totem's hotbar slot (1-9): ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().parse::<u32>().ok().and_then(fish_bar_bot::settings::key_for_slot) {
        Some(key) => {
            println!("okay, refreshing the totem every {TOTEM_INTERVAL} casts (starting cast {TOTEM_START_CAST})");
            Some(key)
        }
        None => {
            println!("alright, no totem refreshing");
            None
        }
    }
}

/// Every TOTEM_INTERVAL casts starting from TOTEM_START_CAST -- casts
/// 4, 14, 24, ... not a plain modulo, since that would also fire on cast 0.
const TOTEM_START_CAST: u32 = 4;
const TOTEM_INTERVAL: u32 = 10;
const TOTEM_KEY_HOLD: Duration = Duration::from_millis(120);
const TOTEM_SWITCH_DELAY: Duration = Duration::from_millis(300);
const TOTEM_CLICK_HOLD: Duration = Duration::from_millis(200);

/// Presses the totem slot, clicks screen center once, then switches back
/// to the rod slot -- same pattern as fisch_new.rs's totem refresh.
fn run_totem_action(monitor: &Monitor, totem_key: Key, rod_key: Key) {
    println!("\nRefreshing totem: pressing totem slot, clicking center, switching back to rod slot...");
    input::tap_key(totem_key, TOTEM_KEY_HOLD);
    thread::sleep(TOTEM_SWITCH_DELAY);

    let center_x = monitor.x().unwrap_or(0) + monitor.width().unwrap_or(0) as i32 / 2;
    let center_y = monitor.y().unwrap_or(0) + monitor.height().unwrap_or(0) as i32 / 2;
    input::mouse_move(center_x as f64, center_y as f64);
    input::mouse_down(rdev::Button::Left);
    thread::sleep(TOTEM_CLICK_HOLD);
    input::mouse_up(rdev::Button::Left);
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
/// How long to hold off the bar_looks_gone() color-uniformity check after
/// entering the settled loop, so a color mid-transition doesn't trip it.
const SETTLE_STABLE_DELAY: Duration = Duration::from_millis(300);
/// One full there-and-back swing cycle (2 unconfirmed legs) after having
/// caught the flash at least once before -- that means the fight legitimately
/// ended (fish caught/escaped/line broke), so give up and recast.
const LEGS_PER_GIVE_UP: u32 = 2;
/// Safety net for the case we've NEVER caught a flash yet this session --
/// covers the natural pre-bite wait (which can genuinely run 10+ seconds),
/// so this only fires if something is actually broken, not mid-wait. Now
/// prompted for at startup (see prompt_no_bite_timeout_preference()) instead
/// of fixed, so it can be shortened for faster recasting when needed.
const DEFAULT_NO_BITE_TIMEOUT_SECS: u64 = 20;
/// Stop the whole program after this many no-bite timeouts in a row.
const NO_BITE_TIMEOUT_STOP_LIMIT: u32 = 3;

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

/// Why run_bang_bang_control() returned -- lets main() tell a real fight
/// ending from a plain "nothing ever bit" timeout, so it can count
/// consecutive no-bite timeouts and stop the program after too many in a
/// row (see NO_BITE_TIMEOUT_STOP_LIMIT).
enum ControlOutcome {
    /// Confirmed at least one flash this fight, then it ended (caught,
    /// lost, or the bar visibly disappeared) -- a real bite happened.
    FightOver,
    /// Never confirmed a single flash before no_bite_timeout elapsed.
    NoBiteTimeout,
    /// ESC / stop requested.
    Stopped,
}

/// Runs the 5-point flash-based bang-bang controller until the bar is
/// gone for BAR_GONE_TIMEOUT (fight over) or the bot is stopped.
/// `no_bite_timeout` is the prompted-at-startup max wait for a first bite
/// before giving up and recasting anyway (see prompt_no_bite_timeout_preference()).
fn run_bang_bang_control(no_bite_timeout: Duration) -> ControlOutcome {
    println!("Bang-bang control engaged (5-point white-flash tracking)...");

    let region = calibration::region();
    let bar_ranges = calibration::bar_hsv_ranges();
    // Calibration's derived ranges are sorted by hue, not by "which one is
    // the bright/white one" -- unlike config.rs's hand-ordered defaults, so
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
            return ControlOutcome::Stopped;
        }

        // -- SETTLED: poll slowly until the flash drops out --
        let settled_since = Instant::now();
        loop {
            if !stay_active() {
                return ControlOutcome::Stopped;
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

            // Sanity check independent of the calibrated HSV ranges: once
            // settled and stable, the bar should still be visibly splitting
            // the 5 points into a mix of colors. If they've all converged
            // to the same color or all diverged to mutually distinct ones,
            // the bar itself is gone even though white_count is (falsely)
            // still reading a flash -- give up immediately and recast.
            if settled_since.elapsed() >= SETTLE_STABLE_DELAY && bar_looks_gone(&sample_point_colors(&frame, region)) {
                println!("\nAll 5 points read as one uniform color (or all mutually distinct) while settled -- bar is gone, fight over.");
                input::key_up(config::HOLD_KEY);
                return ControlOutcome::FightOver;
            }

            ever_confirmed = true;
            if !checked_sleep(SETTLED_POLL_INTERVAL) {
                return ControlOutcome::Stopped;
            }
        }

        // -- TRANSITIONING: swing leg by leg until a flash confirms one --
        let mut heading = side.other();
        set_heading(heading);
        let mut unconfirmed_legs = 0u32;

        loop {
            if !stay_active() {
                return ControlOutcome::Stopped;
            }
            let leg_start = Instant::now();
            let mut confirmed = false;

            while leg_start.elapsed() < TRANSITION_DURATION + TRANSITION_CONFIRM_BUFFER {
                if !stay_active() {
                    return ControlOutcome::Stopped;
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
                return ControlOutcome::FightOver;
            }
            if !ever_confirmed && session_start.elapsed() >= no_bite_timeout {
                println!("\nNo bite within {:.0}s -- recasting anyway.", no_bite_timeout.as_secs_f64());
                input::key_up(config::HOLD_KEY);
                return ControlOutcome::NoBiteTimeout;
            }

            // No flash this whole leg -- reverse and keep hunting.
            heading = heading.other();
            set_heading(heading);
        }

        // Confirmed catch: hold steady for one extra full second before
        // resuming the slow settled-poll loop.
        if !checked_sleep(SETTLE_GRACE) {
            return ControlOutcome::Stopped;
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

    let day_night_preference = prompt_day_night_preference();
    let no_bite_timeout = prompt_no_bite_timeout_preference();
    let totem_preference = prompt_totem_preference();

    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    println!("Autonomous bar bot running. F8 = pause/resume, ESC = quit.");
    input::start_hotkey_listener();

    // Only place the 9-then-1 pre-trigger sequence fires outside of the
    // post-action spots below -- once, here, at startup.
    press_pre_trigger_keys();

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
    let watch_template = match day_night_preference {
        Some(DayNightPref::Day) => control::load_icon_template(&asset_dir.join("sun.png"), "sun"),
        Some(DayNightPref::Night) => control::load_icon_template(&asset_dir.join("moon.png"), "moon"),
        None => None,
    };

    let monitor = capture::find_primary_monitor()?;
    // Total casts this run -- resets to 0 every relaunch (not persisted).
    // Both periodic checks below are measured against this same counter and
    // both run back-to-back, in order, whenever they're due -- previously
    // day/night was checked before casting off one counter and aquarium was
    // checked after casting off a separate counter, so even with matching
    // intervals they'd drift and land on different casts, and only one of
    // the two would ever actually fire on a given cast.
    let mut total_casts: u32 = 0;
    // General session timer -- read out alongside the cast count, same
    // idea as total_casts (resets to 0 every relaunch, not persisted).
    let session_start = Instant::now();
    // Consecutive no-bite timeouts (never even confirmed a flash before
    // giving up) -- reset to 0 on any fight that actually engaged. Hitting
    // NO_BITE_TIMEOUT_STOP_LIMIT in a row means something's actually wrong
    // (wrong window focused, region miscalibrated, etc.), not just bad luck,
    // so the program stops instead of casting forever into nothing.
    let mut consecutive_no_bite_timeouts: u32 = 0;

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        total_casts += 1;
        println!("\ncast {total_casts} | elapsed {}", format_elapsed(session_start.elapsed()));
        let settings = fish_bar_bot::settings::load();

        if day_night_preference.is_some() && total_casts % settings.skip_day_night_count.max(1) == 0 {
            if icon_visible_with_retries(watch_template.as_ref()) {
                let totem_key = fish_bar_bot::settings::key_for_slot(settings.skip_day_night_where).unwrap_or(Key::Num5);
                let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
                control::run_moon_action(&monitor, totem_key, rod_key);
                press_pre_trigger_keys();
            }
            // Full delay before the cast either way.
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        if settings.aquarium_boost_enabled && total_casts % settings.aquarium_boost_count.max(1) == 0 {
            println!("\n{total_casts} casts done ({} elapsed) -- running aquarium boost...", format_elapsed(session_start.elapsed()));
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            aquarium::run_aquarium_boost(rod_key);
            press_pre_trigger_keys();
        }

        if let Some(totem_key) = totem_preference
            && total_casts >= TOTEM_START_CAST
            && (total_casts - TOTEM_START_CAST) % TOTEM_INTERVAL == 0
        {
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            run_totem_action(&monitor, totem_key, rod_key);
            press_pre_trigger_keys();
        }

        // Pre-trigger (9, 1) is NOT pressed here between every cast anymore
        // -- only at startup and after the side-actions above (skip_pre_trigger
        // = true always).
        control::run_trigger_sequence(true, ENTER_COUNT_OVERRIDE);
        if input::is_stopped() {
            break;
        }

        // Extra Enter presses on top of run_trigger_sequence's own burst.
        for _ in 0..EXTRA_ENTER_COUNT {
            if input::is_stopped() {
                break;
            }
            input::tap_key(Key::Return, config::TRIGGER_ENTER_HOLD);
            thread::sleep(Duration::from_millis(settings.cast_enter_delay_ms));
        }

        match run_bang_bang_control(no_bite_timeout) {
            ControlOutcome::FightOver => {
                consecutive_no_bite_timeouts = 0;
            }
            ControlOutcome::NoBiteTimeout => {
                consecutive_no_bite_timeouts += 1;
                println!("no-bite timeout {consecutive_no_bite_timeouts}/{NO_BITE_TIMEOUT_STOP_LIMIT} in a row.");
                if consecutive_no_bite_timeouts >= NO_BITE_TIMEOUT_STOP_LIMIT {
                    println!("{NO_BITE_TIMEOUT_STOP_LIMIT} no-bite timeouts in a row -- stopping.");
                    break;
                }
            }
            ControlOutcome::Stopped => break,
        }

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(Key::Space);
    println!("Stopped. Total casts this run: {total_casts} ({} elapsed)", format_elapsed(session_start.elapsed()));
    Ok(())
}

// ----------------------------------------------------------------------
// TESTS -- fuzz bar_looks_gone() against synthetic frames
// ----------------------------------------------------------------------
//
// Builds fake control-bar/track/needle frames at random sizes and colors
// and checks bar_looks_gone()'s verdict against the ground truth we built
// the frame with, across three scenarios:
//   - bar genuinely settled left or right (should NOT be flagged gone)
//   - bar gone, uniform background behind it (SHOULD be flagged gone)
//   - bar gone, noisy/varied scenery behind it (SHOULD be flagged gone)
#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn make_frame(width: u32, height: u32, fill: (u8, u8, u8)) -> RgbaImage {
        RgbaImage::from_fn(width, height, |_, _| Rgba([fill.0, fill.1, fill.2, 255]))
    }

    fn rand_color() -> (u8, u8, u8) {
        (rand::random_range(0..=255u8), rand::random_range(0..=255u8), rand::random_range(0..=255u8))
    }

    fn color_dist_u8(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        rgb_distance((a.0 as f64, a.1 as f64, a.2 as f64), (b.0 as f64, b.1 as f64, b.2 as f64))
    }

    fn paint_point(frame: &mut RgbaImage, cx: i32, cy: i32, color: (u8, u8, u8)) {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && (x as u32) < frame.width() && (y as u32) < frame.height() {
                    frame.put_pixel(x as u32, y as u32, Rgba([color.0, color.1, color.2, 255]));
                }
            }
        }
    }

    /// Paints a settled control bar (occupying 55% of the track, resting
    /// fully left or fully right, with a little positional jitter to mimic
    /// capture noise) onto a frame already filled with the background/track
    /// color.
    fn paint_settled_bar(frame: &mut RgbaImage, region: config::Region, side: Side, bar_color: (u8, u8, u8)) {
        let cy = (SAMPLE_Y_FRAC * region.height as f64).round() as i32;
        let bar_width_frac = 0.55;
        let jitter = rand::random_range(0.0..0.02);
        let bar_left_frac = match side {
            Side::Left => jitter,
            Side::Right => 1.0 - bar_width_frac - jitter,
        };
        let left_px = (bar_left_frac * region.width as f64).round() as i32;
        let right_px = ((bar_left_frac + bar_width_frac) * region.width as f64).round() as i32;
        for x in left_px.max(0)..=right_px.min(region.width as i32 - 1) {
            paint_point(frame, x, cy, bar_color);
        }
    }

    #[test]
    fn bar_looks_gone_fuzz_2000() {
        const TRIALS: u32 = 2000;
        let (mut present_ok, mut present_total) = (0u32, 0u32);
        let (mut gone_uniform_ok, mut gone_uniform_total) = (0u32, 0u32);
        let (mut gone_noisy_ok, mut gone_noisy_total) = (0u32, 0u32);

        for i in 0..TRIALS {
            let width = rand::random_range(300u32..=800);
            let height = rand::random_range(20u32..=50);
            let region = config::Region { left: 0, top: 0, width, height };
            let cy = (SAMPLE_Y_FRAC * height as f64).round() as i32;

            match rand::random_range(0..3u8) {
                0 => {
                    // -- settled bar genuinely present, left or right rest --
                    let side = if rand::random::<bool>() { Side::Left } else { Side::Right };
                    let bg = rand_color();
                    let mut bar_color = rand_color();
                    while color_dist_u8(bg, bar_color) < 60.0 {
                        bar_color = rand_color();
                    }
                    let mut frame = make_frame(width, height, bg);
                    paint_settled_bar(&mut frame, region, side, bar_color);

                    let colors = sample_point_colors(&frame, region);
                    let flagged_gone = bar_looks_gone(&colors);
                    present_total += 1;
                    if !flagged_gone {
                        present_ok += 1;
                    } else {
                        eprintln!(
                            "FAIL[present #{i}]: side={side:?} bg={bg:?} bar={bar_color:?} region={width}x{height} colors={colors:?}"
                        );
                    }
                }
                1 => {
                    // -- bar gone: uniform background/scenery behind it --
                    let bg = rand_color();
                    let frame = make_frame(width, height, bg);
                    let colors = sample_point_colors(&frame, region);
                    let flagged_gone = bar_looks_gone(&colors);
                    gone_uniform_total += 1;
                    if flagged_gone {
                        gone_uniform_ok += 1;
                    } else {
                        eprintln!("FAIL[gone-uniform #{i}]: bg={bg:?} region={width}x{height} colors={colors:?}");
                    }
                }
                _ => {
                    // -- bar gone: varied/noisy scenery, each point a
                    // mutually well-separated random color --
                    let mut point_colors = [(0u8, 0u8, 0u8); 5];
                    'generate: loop {
                        for c in &mut point_colors {
                            *c = rand_color();
                        }
                        let all_separated = (0..5).all(|a| {
                            ((a + 1)..5).all(|b| color_dist_u8(point_colors[a], point_colors[b]) >= 60.0)
                        });
                        if all_separated {
                            break 'generate;
                        }
                    }
                    let mut frame = make_frame(width, height, (0, 0, 0));
                    for (idx, &frac_x) in SAMPLE_POINTS_X_FRAC.iter().enumerate() {
                        let cx = (frac_x * width as f64).round() as i32;
                        paint_point(&mut frame, cx, cy, point_colors[idx]);
                    }
                    let colors = sample_point_colors(&frame, region);
                    let flagged_gone = bar_looks_gone(&colors);
                    gone_noisy_total += 1;
                    if flagged_gone {
                        gone_noisy_ok += 1;
                    } else {
                        eprintln!("FAIL[gone-noisy #{i}]: point_colors={point_colors:?} region={width}x{height} colors={colors:?}");
                    }
                }
            }
        }

        println!(
            "bar_looks_gone fuzz results over {TRIALS} trials:\n\
             settled bar present, correctly NOT flagged gone: {present_ok}/{present_total}\n\
             bar gone (uniform bg), correctly flagged gone:   {gone_uniform_ok}/{gone_uniform_total}\n\
             bar gone (noisy bg), correctly flagged gone:     {gone_noisy_ok}/{gone_noisy_total}"
        );

        assert_eq!(present_ok, present_total, "some genuinely-settled bar frames were falsely flagged as gone");
        assert_eq!(gone_uniform_ok, gone_uniform_total, "some uniform-background 'bar gone' frames were missed");
        assert_eq!(gone_noisy_ok, gone_noisy_total, "some noisy-background 'bar gone' frames were missed");
    }
}
