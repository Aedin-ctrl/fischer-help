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

use fish_bar_bot::config::HsvRange;
use fish_bar_bot::{capture, config, control, input};

// ----------------------------------------------------------------------
// 5-POINT BAR SAMPLING
// ----------------------------------------------------------------------
//
// Five fixed x-positions along the control-bar track (absolute screen
// coordinates, same y), ordered far-right to far-left. The bar occupies
// ~55% of the track's width, and these five points were picked so that
// wherever the bar currently sits, at least two of them fall under it —
// so we never need to track the bar's actual edges, just sample these
// fixed spots.
const SAMPLE_POINTS_X: [i32; 5] = [994, 784, 721, 660, 445];
const SAMPLE_Y: i32 = 805;

// The bar's own color is a whole-bar state, not per-pixel: it reads
// bright/white when the needle is currently aligned with it ("engaged"),
// and dim/brown when it isn't. Reuse the same two ranges main.rs's
// tracker uses for "the bar" as a whole.
const WHITE_RANGE: HsvRange = config::CONTROLLED_BAR_HSV_RANGES[0];
const BAR_RANGES: &[HsvRange] = config::CONTROLLED_BAR_HSV_RANGES;

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

fn sample_bar(frame: &RgbaImage, region_left: i32, region_top: i32) -> BarSample {
    let mut white_count = 0;
    let mut bar_present_count = 0;
    for &x_abs in &SAMPLE_POINTS_X {
        let cx = x_abs - region_left;
        let cy = SAMPLE_Y - region_top;
        if point_matches(frame, cx, cy, std::slice::from_ref(&WHITE_RANGE)) {
            white_count += 1;
        }
        if point_matches(frame, cx, cy, BAR_RANGES) {
            bar_present_count += 1;
        }
    }
    BarSample { white_count, bar_present_count }
}

#[derive(Clone, Copy, PartialEq)]
enum DayNightPref {
    Day,
    Night,
}

/// Ask which time of day to skip. 'd'/'day' watches for the sun icon (day)
/// and skips it -- net effect: only night. 'n'/'night' watches for the moon
/// icon (night) instead -- net effect: only day. Anything else (including
/// just Enter) disables the check entirely.
fn prompt_day_night_preference() -> Option<DayNightPref> {
    print!("What do you want to skip? Type d/day to skip day, n/night to skip night, or press Enter to skip nothing: ");
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
// Throughout, if the bar isn't detected at all (bar_present_count == 0)
// for longer than BAR_GONE_TIMEOUT (2 full swings there-and-back), the
// fight is considered over and this function returns so main() can recast.

const TRANSITION_DURATION: Duration = Duration::from_millis(800);
const SETTLE_GRACE: Duration = Duration::from_secs(1);
const SETTLED_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// 2 full A->B->C->D->A cycles, each cycle being two 0.8s legs.
const BAR_GONE_TIMEOUT: Duration = Duration::from_millis(2 * 2 * 800);

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
        thread::sleep(Duration::from_millis(20).min(duration - start.elapsed()));
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

    let mut side = Side::Left;
    set_heading(side);
    let mut last_bar_seen = Instant::now();

    'outer: loop {
        if !stay_active() {
            return;
        }

        // -- SETTLED: poll slowly until the flash drops out --
        loop {
            if !stay_active() {
                return;
            }
            let Ok(frame) = capture::grab_region_normalized(config::REGION) else {
                thread::sleep(SETTLED_POLL_INTERVAL);
                continue;
            };
            let sample = sample_bar(&frame, config::REGION.left, config::REGION.top);

            if sample.bar_present_count > 0 {
                last_bar_seen = Instant::now();
            } else if last_bar_seen.elapsed() >= BAR_GONE_TIMEOUT {
                println!("\nBar not seen for {:.1}s (2 full swings) -- fight over.", BAR_GONE_TIMEOUT.as_secs_f64());
                input::key_up(config::HOLD_KEY);
                return;
            }

            print_status(&format!("[settled {side:?}] white={}/5 bar={}/5", sample.white_count, sample.bar_present_count));

            if sample.white_count < 2 {
                break; // lost the flash -- start hunting toward the other side
            }
            if !checked_sleep(SETTLED_POLL_INTERVAL) {
                return;
            }
        }

        // -- TRANSITIONING: swing leg by leg until a flash confirms one --
        let mut heading = side.other();
        set_heading(heading);

        loop {
            if !stay_active() {
                return;
            }
            let leg_start = Instant::now();
            let mut confirmed = false;

            while leg_start.elapsed() < TRANSITION_DURATION {
                if !stay_active() {
                    return;
                }
                let Ok(frame) = capture::grab_region_normalized(config::REGION) else {
                    continue;
                };
                let sample = sample_bar(&frame, config::REGION.left, config::REGION.top);

                if sample.bar_present_count > 0 {
                    last_bar_seen = Instant::now();
                } else if last_bar_seen.elapsed() >= BAR_GONE_TIMEOUT {
                    println!("\nBar not seen for {:.1}s (2 full swings) -- fight over.", BAR_GONE_TIMEOUT.as_secs_f64());
                    input::key_up(config::HOLD_KEY);
                    return;
                }

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
                side = heading;
                break;
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
    let day_night_preference = prompt_day_night_preference();

    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

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
    let watch_template = match day_night_preference {
        Some(DayNightPref::Day) => control::load_icon_template(&asset_dir.join("sun.png"), "sun"),
        Some(DayNightPref::Night) => control::load_icon_template(&asset_dir.join("moon.png"), "moon"),
        None => None,
    };

    let monitor = capture::find_primary_monitor()?;
    let mut session_count: u32 = 0;

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        if day_night_preference.is_some() && session_count >= config::SESSIONS_BETWEEN_MOON_CHECKS {
            session_count = 0;
            if control::is_icon_visible(watch_template.as_ref()) {
                control::run_moon_action(&monitor);
            }
            // Full delay before the cast either way.
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        control::run_trigger_sequence();
        if input::is_stopped() {
            break;
        }

        run_bang_bang_control();
        session_count += 1;

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
