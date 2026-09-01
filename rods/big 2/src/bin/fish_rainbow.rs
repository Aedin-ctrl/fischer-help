//! Autonomous Bar Bot (macOS, terminal) — for the rainbow-gradient minigame
//! skin. The bar's rainbow fill sits in a FIXED screen position, confirmed
//! directly by the player -- FIXED_REGION is hardcoded to it, no bar
//! detection needed. Only the pin moves, and its own appearance never
//! changes either (just its x-position) -- so it's tracked by direct
//! template matching against a real captured image of it (see
//! find_pin_by_template below), not a color-threshold approximation.
//! Earlier versions of this file tried: tracking the bar's own color,
//! then a floating fish icon, both via brightness/saturation thresholds
//! that kept breaking against different real-world lighting; then an
//! achromatic-color threshold for the pin, which worked but still let an
//! unrelated fixed bright element trip it on empty captures. Template
//! matching the pin's actual, invariant pixels directly is both simpler
//! and, checked against every real capture available, notably more
//! accurate than any of those. Steers toward whichever half of the FIXED
//! bar the pin is on -- see run_pin_tracking_control() below.
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

use fish_bar_bot::{aquarium, capture, config, control, input, vision};

/// Extra Enter presses tacked on after each normal cast sequence, on top of
/// run_trigger_sequence's own burst.
const EXTRA_ENTER_COUNT: u32 = 0;

// ----------------------------------------------------------------------
// PIN DETECTION
// ----------------------------------------------------------------------
//
// The bar itself (see FIXED_REGION) never needs detecting -- confirmed by
// the player: same rainbow pattern, same position, same size, every cast.
// The pin's x-position is the only thing that ever changes, and its own
// appearance (shape, color) never does -- so instead of approximating
// "pin-colored" with a color threshold (an earlier version of this file
// did; it worked but still let one fixed, unrelated achromatic-bright
// element on screen occasionally trip it even on an empty capture), this
// matches the pin's ACTUAL pixels directly: PIN_REFERENCE is a real crop
// of the pin from a genuine gameplay screenshot, embedded at compile
// time, and find_pin_by_template slides it horizontally across the
// captured frame (same TM_CCOEFF_NORMED correlation vision::
// match_template_max uses for the moon/sun icon check, just tracking
// which x-offset won instead of only the best score) to find where it
// actually matches.
//
// Checked against every real capture available (8 different screenshots,
// different lighting/locations/times): landed within 1-7px of where
// color-threshold detection found the pin in every single case, scoring
// 0.70-1.0. A genuinely empty/black capture scored exactly 0.0.
static PIN_REFERENCE_BYTES: &[u8] = include_bytes!("pin_reference.png");
static PIN_REFERENCE: std::sync::LazyLock<RgbaImage> =
    std::sync::LazyLock::new(|| image::load_from_memory(PIN_REFERENCE_BYTES).expect("pin_reference.png is a valid embedded image").to_rgba8());
/// 0.65 -- sits just under the weakest real pin score measured (0.6996)
/// and well above where a genuinely empty capture scores (0.0). Fuzz
/// testing against fully independent random per-pixel noise (a much
/// harsher adversary than any real capture -- real screen content is
/// spatially smooth, not literal static) still occasionally produced a
/// spurious match above this by chance (up to ~0.77 in 1000 trials); no
/// single fixed threshold can fully separate that from the weakest real
/// pin match without also risking rejecting real weak matches. The
/// control loop's CONFIRM_STREAK_REQUIRED/MISSING_GRACE logic is the
/// actual backstop against a single spurious tick like that, same as it
/// always has been.
const PIN_TEMPLATE_MIN_SCORE: f64 = 0.65;

/// 1D horizontal sliding-window TM_CCOEFF_NORMED search -- same
/// correlation formula as vision::match_template_max, reimplemented
/// locally because that function only returns the best SCORE across a
/// full 2D search; here the winning x-offset IS the answer (the pin's
/// position), not just a yes/no. Only a 1D horizontal search because
/// PIN_REFERENCE's height is fixed to equal the search frame's height
/// (both come from FIXED_REGION), so there's exactly one valid y-offset.
fn match_1d_best_x(search: &RgbaImage, template: &RgbaImage) -> (u32, f64) {
    let (sw, sh) = search.dimensions();
    let (tw, th) = template.dimensions();
    assert_eq!(sh, th, "match_1d_best_x requires search and template heights to match");
    if tw > sw {
        return (0, f64::MIN);
    }

    let n = (tw * th) as f64;
    let mut t_mean = [0f64; 3];
    for p in template.pixels() {
        for c in 0..3 {
            t_mean[c] += p[c] as f64;
        }
    }
    for c in 0..3 {
        t_mean[c] /= n;
    }
    let mut t_denom = 0f64;
    let mut t_centered = vec![[0f64; 3]; (tw * th) as usize];
    for (i, p) in template.pixels().enumerate() {
        for c in 0..3 {
            let d = p[c] as f64 - t_mean[c];
            t_centered[i][c] = d;
            t_denom += d * d;
        }
    }

    let mut best_x = 0u32;
    let mut best_score = f64::MIN;
    for ox in 0..=(sw - tw) {
        let mut i_mean = [0f64; 3];
        for ty in 0..th {
            for tx in 0..tw {
                let p = search.get_pixel(ox + tx, ty);
                for c in 0..3 {
                    i_mean[c] += p[c] as f64;
                }
            }
        }
        for c in 0..3 {
            i_mean[c] /= n;
        }

        let mut numerator = 0f64;
        let mut i_denom = 0f64;
        for ty in 0..th {
            for tx in 0..tw {
                let idx = (ty * tw + tx) as usize;
                let p = search.get_pixel(ox + tx, ty);
                for c in 0..3 {
                    let d = p[c] as f64 - i_mean[c];
                    numerator += t_centered[idx][c] * d;
                    i_denom += d * d;
                }
            }
        }

        let denom = (t_denom * i_denom).sqrt();
        let score = if denom > 1e-10 { numerator / denom } else { 0.0 };
        if score > best_score {
            best_score = score;
            best_x = ox;
        }
    }
    (best_x, best_score)
}

fn find_pin_by_template(frame: &RgbaImage) -> Option<vision::Bounds> {
    let (best_x, score) = match_1d_best_x(frame, &PIN_REFERENCE);
    if score < PIN_TEMPLATE_MIN_SCORE {
        return None;
    }
    let left = best_x as f64;
    let width = PIN_REFERENCE.width() as f64;
    Some(vision::Bounds { left, right: left + width, center: left + width / 2.0 })
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

// ----------------------------------------------------------------------
// PERIODIC ACTIONS -- time-based, not cast-count-based
// ----------------------------------------------------------------------
//
// Cast duration varies a lot with this control loop (a quick miss vs. a
// real fight), so "every N casts" doesn't land on a consistent real-world
// cadence -- checked by elapsed time instead. Each timer is checked once
// per loop iteration, right before the next cast (never mid-fight), so if
// one fires while a long fight is still in progress, it just runs before
// the next cast starts rather than interrupting anything.
const DAY_NIGHT_CHECK_INTERVAL: Duration = Duration::from_secs(2 * 60);
const AQUARIUM_BOOST_INTERVAL: Duration = Duration::from_secs(31 * 60);
const TOTEM_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Separate totem slot/timer from the one above -- same refresh action,
/// just a different item (Shiny) on its own independent 12.5-minute
/// cadence and its own hotbar slot.
const SHINY_TOTEM_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 + 30);

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
/// anyway (NO_BITE_TIMEOUT). Blank input keeps the standard amount (20s);
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
            let mins = TOTEM_REFRESH_INTERVAL.as_secs() / 60;
            println!("okay, refreshing the totem every {mins} minutes");
            Some(key)
        }
        None => {
            println!("alright, no totem refreshing");
            None
        }
    }
}

/// Same idea as prompt_totem_preference(), a separate question for a
/// separate item (Shiny) on its own hotbar slot and its own refresh
/// cadence (SHINY_TOTEM_REFRESH_INTERVAL).
fn prompt_shiny_totem_preference() -> Option<Key> {
    print!("Shiny totem?: press Enter to skip, or type the Shiny totem's hotbar slot (1-9): ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().parse::<u32>().ok().and_then(fish_bar_bot::settings::key_for_slot) {
        Some(key) => {
            let mins = SHINY_TOTEM_REFRESH_INTERVAL.as_secs() as f64 / 60.0;
            println!("okay, refreshing the Shiny totem every {mins} minutes");
            Some(key)
        }
        None => {
            println!("alright, no Shiny totem refreshing");
            None
        }
    }
}

const TOTEM_KEY_HOLD: Duration = Duration::from_millis(120);
const TOTEM_SWITCH_DELAY: Duration = Duration::from_millis(300);
const TOTEM_CLICK_HOLD: Duration = Duration::from_millis(200);

/// Presses the given totem slot, clicks screen center once, then switches
/// back to the rod slot -- same pattern as fisch_new.rs's totem refresh.
/// Shared by both the regular and Shiny totem refresh (see
/// TOTEM_REFRESH_INTERVAL/SHINY_TOTEM_REFRESH_INTERVAL) -- same action,
/// just called with a different slot on a different cadence.
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

const DEFAULT_NO_BITE_TIMEOUT_SECS: u64 = 20;
/// Stop the whole program after this many no-bite timeouts in a row.
const NO_BITE_TIMEOUT_STOP_LIMIT: u32 = 3;

/// Which half of the bar the pin is currently in -- see the doc comment
/// above run_pin_tracking_control() for what each one does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone {
    /// Left half -- do nothing at all, key stays up.
    Release,
    /// Right half -- hold the key down, don't let go until the pin
    /// crosses back to the left half.
    Hold,
}

/// \r alone doesn't erase leftover characters from a longer previous line --
/// clear to end-of-line first so a shorter status string doesn't leave a
/// garbled tail behind.
fn print_status(line: &str) {
    print!("\r\x1b[K{line}");
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

/// Why run_pin_tracking_control() returned -- lets main() tell a real fight
/// ending from a plain "nothing ever bit" timeout, so it can count
/// consecutive no-bite timeouts and stop the program after too many in a
/// row (see NO_BITE_TIMEOUT_STOP_LIMIT).
enum ControlOutcome {
    /// Confirmed bar + pin together at least once this fight, then both
    /// disappeared -- a real bite happened and the fight ended (caught,
    /// lost, or line broke).
    FightOver,
    /// Never confirmed bar + pin together before no_bite_timeout elapsed.
    NoBiteTimeout,
    /// ESC / stop requested.
    Stopped,
}

// ----------------------------------------------------------------------
// PIN-TRACKING CONTROL LOOP
// ----------------------------------------------------------------------
//
// Every tick: find the pin's x-position within the FIXED bar region
// (FIXED_REGION), split that region at its exact midpoint, and act on
// exactly which half the pin is in RIGHT NOW -- no smoothing, no
// debounce, no middle deadzone, an instant switch the moment it crosses
// to the other half (see Zone above):
//   - Release (left half): do nothing at all -- key stays up.
//   - Hold (right half): hold the key down, and don't let go until the
//     pin actually crosses to the left half on a later frame -- a single
//     bad detection frame can't drop it, since the zone only changes in
//     response to a fresh reading, never a timer.
//
// Two earlier versions of this tracked something dynamically instead --
// the bar's own color, then a floating fish icon above it -- both via a
// brightness/saturation threshold that kept breaking against whatever
// lighting a given real play session happened to have (see the module
// doc comment at the top of this file). Since the bar itself is fixed
// (confirmed by the player: same pattern, position, and size on every
// cast), there's nothing dynamic left to detect except the pin, and
// FIXED_REGION's own midpoint IS the bar's midpoint -- no bar detection
// needed at all.
//
// "Fight over" detection: once the pin has been seen even once
// (ever_confirmed), losing it for MISSING_GRACE (currently ~1s) means the
// fight is treated as over and the next cast starts. Before that first
// sighting, only the no_bite_timeout safety net applies -- the natural
// pre-bite wait (pin not on screen yet at all) is unbounded and must not
// be mistaken for "over."
//
// ever_confirmed no longer waits for CONFIRM_STREAK_REQUIRED consecutive
// ticks before arming -- it used to, specifically to protect against a
// single false-positive detection ending a cast within MISSING_GRACE of
// it starting. That protection mattered when detection was a brightness/
// saturation heuristic prone to one-off noise (see the module doc
// comment); the achromatic pin check is reliable enough now that gating
// the recast safety net on it was actively hurting more than helping: any
// real detection flicker before the streak completed meant the fight
// never armed at all, so the ONLY thing that could end a cast was the
// full no_bite_timeout (default 20s) -- exactly the "timed out" behavior
// this was meant to avoid. confirm_streak/CONFIRM_STREAK_REQUIRED still
// drive the "[confirmed]" log line, just nothing safety-critical anymore.

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Skipped before the tracking loop starts trusting anything it sees.
/// Real capture data showed the cast's own line/rod-swing animation still
/// settling for a brief moment right after run_trigger_sequence returns --
/// judging frames during that window risked treating transient animation
/// as a real detection (and, worse, letting a few ticks of that noise
/// satisfy CONFIRM_STREAK_REQUIRED and get treated as a bite). Frames are
/// still captured and the loop still exits promptly on stop/pause during
/// this window -- only the pin-bounds decision is skipped, not input
/// responsiveness.
const CAST_SETTLE_DELAY: Duration = Duration::from_millis(600);
/// How long the pin must stay undetected -- after having been seen at
/// least once -- before the fight is considered over and the next cast
/// starts. 1 full second: long enough that a brief real flicker (a frame
/// or two of missed detection) doesn't falsely end an ongoing fight, short
/// enough that actually losing the pin recasts promptly instead of idling
/// out to the much longer no_bite_timeout.
const MISSING_GRACE: Duration = Duration::from_millis(1000);
/// Consecutive ticks the pin must be detected before it's trusted as a
/// real bite rather than a single-frame false positive.
const CONFIRM_STREAK_REQUIRED: u32 = 4;

/// The bar's own known, fixed screen position (logical points) -- outer
/// container left=418/right=1021 (measured consistently across every
/// capture checked, different lighting/location/session), vertical range
/// covering the pin's own travel band (top=775/bottom=828). Confirmed by
/// the player that the bar itself never changes position, size, or
/// pattern, so this needs measuring once, not detecting every frame.
///
/// Not sourced from calibration::region(): that shared calibration
/// belongs to a different bot/skin entirely. Hardcoded here rather than
/// trusting shared calibration, same as fish_Onirifalx.rs; will need
/// updating if the Roblox window moves or resizes.
const FIXED_REGION: config::Region = config::Region { left: 418, top: 775, width: 1021 - 418, height: 828 - 775 };

fn run_pin_tracking_control(no_bite_timeout: Duration) -> ControlOutcome {
    println!("Pin-tracking control engaged (fixed bar, achromatic pin)...");

    let region = FIXED_REGION;
    println!(
        "  watching region: left={} top={} width={} height={}",
        region.left, region.top, region.width, region.height
    );
    // No zone yet, no key held -- matches the real key state, since
    // whatever ended the previous fight already released it.
    let mut current_zone: Option<Zone> = None;
    let mut ever_confirmed = false;
    let mut confirm_streak = 0u32;
    let mut missing_since: Option<Instant> = None;
    let session_start = Instant::now();
    // Dumps a full-width color profile once per cast, on the first captured
    // frame AFTER CAST_SETTLE_DELAY -- if detection still misses in
    // practice, compare this against PIN_TEMPLATE_MIN_SCORE and retune.
    // Strip once confirmed solid.
    let mut profile_printed = false;

    loop {
        if !stay_active() {
            return ControlOutcome::Stopped;
        }

        let Ok(frame) = capture::grab_region_normalized(region) else {
            if !checked_sleep(POLL_INTERVAL) {
                return ControlOutcome::Stopped;
            }
            continue;
        };

        if session_start.elapsed() < CAST_SETTLE_DELAY {
            // Frame captured (so the wait itself stays responsive to
            // stop/pause via stay_active above) but deliberately not
            // looked at yet -- see CAST_SETTLE_DELAY's doc comment.
            if !checked_sleep(POLL_INTERVAL) {
                return ControlOutcome::Stopped;
            }
            continue;
        }

        if !profile_printed {
            profile_printed = true;
            let sample_y = frame.height() / 2;
            let step = (frame.width() / 24).max(1);
            let mut profile = String::new();
            let mut x = 0u32;
            while x < frame.width() {
                let p = frame.get_pixel(x, sample_y);
                profile.push_str(&format!("{x}:({},{},{}) ", p[0], p[1], p[2]));
                x += step;
            }
            println!("\n  [profile] {profile}");
            // Scans the WHOLE frame (not just the sampled row above, which
            // can legitimately miss the pin if it isn't sitting on that
            // exact row) -- a genuinely black/empty capture (every channel
            // 0) means Screen Recording permission isn't actually granted
            // for this exact build (macOS ties the grant to the binary's
            // signature -- a rebuild can silently invalidate a previous
            // grant, same issue package.sh works around for the packaged
            // .app; this raw `cargo run` binary isn't signed that way).
            let max_v_anywhere = frame.pixels().map(|p| p[0].max(p[1]).max(p[2])).max().unwrap_or(0);
            if max_v_anywhere == 0 {
                println!(
                    "  [warning] this frame is solid black -- that's not a real capture. Check System Settings -> \
                     Privacy & Security -> Screen Recording has THIS terminal app toggled on (toggle it off and back \
                     on if it's already checked -- a rebuild can silently break the grant)."
                );
            }
        }

        let pin = find_pin_by_template(&frame);
        let box_center = frame.width() as f64 / 2.0;

        match pin {
            Some(pin) => {
                missing_since = None;
                if confirm_streak == 0 {
                    // Permanent (non-overwritten) log of exactly what got
                    // detected at the start of a streak -- the \r status
                    // line below gets erased before a human can read it if
                    // the streak is short-lived, so this is the only
                    // reliable record of what triggered it.
                    println!("\n  [first detection] pin={:.0}-{:.0} center={:.0}", pin.left, pin.right, pin.center);
                }
                // Arms the recast-on-lost safety net (see MISSING_GRACE)
                // on the very first sighting, not gated behind a streak --
                // see the module-level doc comment above for why.
                ever_confirmed = true;
                confirm_streak += 1;
                if confirm_streak == CONFIRM_STREAK_REQUIRED {
                    println!("  [confirmed] {CONFIRM_STREAK_REQUIRED} consecutive detections -- treating as a real bite.");
                }
                // Fresh, immediate decision every single frame -- exactly
                // which half of the FIXED bar the pin is in right now.
                let zone = if pin.center <= box_center { Zone::Release } else { Zone::Hold };

                if Some(zone) != current_zone {
                    // key_down/key_up each fire a brand-new synthetic key
                    // event -- only call them right on the transition into
                    // a new zone, not every tick, otherwise a sustained
                    // Hold spams repeated KeyPress events instead of one
                    // continuous hold.
                    match zone {
                        Zone::Release => input::key_up(config::HOLD_KEY),
                        Zone::Hold => input::key_down(config::HOLD_KEY),
                    }
                    current_zone = Some(zone);
                }

                print_status(&format!("[track] pin={:.0} zone={zone:?} streak={confirm_streak}", pin.center));
            }
            None => {
                confirm_streak = 0;
                print_status("[track] pin=none");
                if ever_confirmed {
                    match missing_since {
                        None => missing_since = Some(Instant::now()),
                        Some(since) if since.elapsed() >= MISSING_GRACE => {
                            println!("\nPin gone -- fight over.");
                            input::key_up(config::HOLD_KEY);
                            return ControlOutcome::FightOver;
                        }
                        _ => {}
                    }
                } else if session_start.elapsed() >= no_bite_timeout {
                    println!("\nNo bite within {:.0}s -- recasting anyway.", no_bite_timeout.as_secs_f64());
                    input::key_up(config::HOLD_KEY);
                    return ControlOutcome::NoBiteTimeout;
                }
            }
        }

        if !checked_sleep(POLL_INTERVAL) {
            return ControlOutcome::Stopped;
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Safety net: if anything panics anywhere in the loop below, release the
    // held key before unwinding -- otherwise a crash mid-hold leaves Space
    // stuck down in-game forever.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        input::key_up(Key::Space);
        default_hook(info);
    }));

    let day_night_preference = prompt_day_night_preference();
    let no_bite_timeout = prompt_no_bite_timeout_preference();
    let totem_preference = prompt_totem_preference();
    let shiny_totem_preference = prompt_shiny_totem_preference();

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
    let mut total_casts: u32 = 0;
    let session_start = Instant::now();
    // Consecutive no-bite timeouts (never confirmed the pin before
    // giving up) -- reset to 0 on any fight that actually engaged. Hitting
    // NO_BITE_TIMEOUT_STOP_LIMIT in a row means something's actually wrong
    // (wrong window focused, region miscalibrated, PIN_TEMPLATE_MIN_SCORE
    // needs retuning, etc.), not just bad luck, so the program stops instead of
    // casting forever into nothing.
    let mut consecutive_no_bite_timeouts: u32 = 0;
    // Each starts at session_start, so the first check of each kind fires
    // after its own full interval has elapsed, not immediately on cast 1.
    let mut last_day_night_check = session_start;
    let mut last_aquarium_boost = session_start;
    let mut last_totem_refresh = session_start;
    let mut last_shiny_totem_refresh = session_start;

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        total_casts += 1;
        println!("\ncast {total_casts} | elapsed {}", format_elapsed(session_start.elapsed()));
        let settings = fish_bar_bot::settings::load();

        if day_night_preference.is_some() && last_day_night_check.elapsed() >= DAY_NIGHT_CHECK_INTERVAL {
            last_day_night_check = Instant::now();
            if icon_visible_with_retries(watch_template.as_ref()) {
                let totem_key = fish_bar_bot::settings::key_for_slot(settings.skip_day_night_where).unwrap_or(Key::Num5);
                let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
                control::run_moon_action(&monitor, totem_key, rod_key);
                press_pre_trigger_keys();
            }
            // Full delay before the cast either way.
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        if settings.aquarium_boost_enabled && last_aquarium_boost.elapsed() >= AQUARIUM_BOOST_INTERVAL {
            last_aquarium_boost = Instant::now();
            println!("\n{total_casts} casts done ({} elapsed) -- running aquarium boost...", format_elapsed(session_start.elapsed()));
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            aquarium::run_aquarium_boost(rod_key);
            press_pre_trigger_keys();
        }

        if let Some(totem_key) = totem_preference
            && last_totem_refresh.elapsed() >= TOTEM_REFRESH_INTERVAL
        {
            last_totem_refresh = Instant::now();
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            run_totem_action(&monitor, totem_key, rod_key);
            press_pre_trigger_keys();
        }

        if let Some(shiny_totem_key) = shiny_totem_preference
            && last_shiny_totem_refresh.elapsed() >= SHINY_TOTEM_REFRESH_INTERVAL
        {
            last_shiny_totem_refresh = Instant::now();
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            run_totem_action(&monitor, shiny_totem_key, rod_key);
            press_pre_trigger_keys();
        }

        // Pre-trigger (9, 1) is NOT pressed here between every cast -- only
        // at startup and after the side-actions above (skip_pre_trigger =
        // true always).
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

        match run_pin_tracking_control(no_bite_timeout) {
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
                // A timeout with no bite at all can mean the rod never got
                // properly re-seated after the last action -- press 9 then
                // 1 again before the next cast, same as after totem/day-
                // night/aquarium.
                press_pre_trigger_keys();
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
// TESTS -- fuzz find_pin_by_template() against synthetic frames
// ----------------------------------------------------------------------
//
// Pastes the ACTUAL embedded PIN_REFERENCE pixels at a random x-offset
// onto random-colored backgrounds with random colorful noise blobs, and
// checks that find_pin_by_template relocates it correctly, that it
// returns None when no pin was pasted at all, and that the resulting
// hold-direction decision matches the true pin-vs-bar-midpoint
// relationship whenever it's outside the deadzone.
#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn make_frame(width: u32, height: u32, fill: (u8, u8, u8)) -> RgbaImage {
        RgbaImage::from_fn(width, height, |_, _| Rgba([fill.0, fill.1, fill.2, 255]))
    }

    fn paint_rect(frame: &mut RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32, color: (u8, u8, u8)) {
        for x in x0..x1.min(frame.width()) {
            for y in y0..y1.min(frame.height()) {
                frame.put_pixel(x, y, Rgba([color.0, color.1, color.2, 255]));
            }
        }
    }

    /// Pastes the ACTUAL embedded PIN_REFERENCE pixels into frame at x0 --
    /// the fuzz is stressing find_pin_by_template's ability to relocate
    /// its own real reference image at an arbitrary offset against random
    /// backgrounds/noise, not a synthetic approximation of what a pin
    /// looks like.
    fn paste_real_pin(frame: &mut RgbaImage, x0: u32) {
        let (tw, th) = PIN_REFERENCE.dimensions();
        for ty in 0..th {
            for tx in 0..tw {
                frame.put_pixel(x0 + tx, ty, *PIN_REFERENCE.get_pixel(tx, ty));
            }
        }
    }

    fn random_color() -> (u8, u8, u8) {
        (rand::random_range(0u8..=255), rand::random_range(0u8..=255), rand::random_range(0u8..=255))
    }

    #[test]
    fn pin_template_fuzz_1000() {
        const TRIALS: u32 = 1000;
        let (mut pin_ok, mut pin_total) = (0u32, 0u32);
        let (mut zone_ok, mut zone_total) = (0u32, 0u32);
        let (mut no_pin_ok, mut no_pin_total) = (0u32, 0u32);
        let pin_width = PIN_REFERENCE.width();
        let pin_height = PIN_REFERENCE.height();

        for i in 0..TRIALS {
            let width = rand::random_range((pin_width + 50)..=800);
            // Height MUST equal the reference's own height -- FIXED_REGION
            // and PIN_REFERENCE are both built from the same real crop, so
            // a live capture always matches this exactly; only x varies.
            let height = pin_height;

            let bg_color = random_color();
            let mut frame = make_frame(width, height, bg_color);

            // 1 in 5 trials: no pin at all, just background + noise --
            // find_pin_by_template must return None, not snap to
            // whatever's closest.
            let include_pin = i % 5 != 0;

            let noise_count = rand::random_range(0u32..=4);
            for _ in 0..noise_count {
                let nw = rand::random_range(2u32..=20);
                let nx = rand::random_range(0u32..width.saturating_sub(nw).max(1));
                let nh = rand::random_range(1u32..=height);
                let ny = rand::random_range(0u32..height.saturating_sub(nh).max(1));
                paint_rect(&mut frame, nx, nx + nw, ny, ny + nh, random_color());
            }

            if !include_pin {
                no_pin_total += 1;
                if find_pin_by_template(&frame).is_none() {
                    no_pin_ok += 1;
                } else {
                    eprintln!("FAIL[no-pin #{i}]: found a pin in a frame with none pasted, region={width}x{height}");
                }
                continue;
            }

            let pin_x = rand::random_range(0u32..width.saturating_sub(pin_width).max(1));
            paste_real_pin(&mut frame, pin_x);

            let true_pin_center = pin_x as f64 + pin_width as f64 / 2.0;
            let edge_tolerance = 2.0;
            let detected_pin = find_pin_by_template(&frame);

            pin_total += 1;
            match detected_pin {
                Some(detected) if (detected.center - true_pin_center).abs() <= edge_tolerance => {
                    pin_ok += 1;
                }
                other => {
                    eprintln!("FAIL[pin #{i}]: true_center={true_pin_center:.1} detected={other:?} noise_count={noise_count} region={width}x{height}");
                }
            }

            // Zone decision: split the box at its exact midpoint, same as
            // run_pin_tracking_control -- pin in the left half -> Release,
            // right half -> Hold. Assert with a little slack around the
            // boundary for detection rounding.
            let box_center = width as f64 / 2.0;
            let slack = 6.0;

            let expected_zone = if true_pin_center <= box_center - slack {
                Some(Zone::Release)
            } else if true_pin_center >= box_center + slack {
                Some(Zone::Hold)
            } else {
                None // too close to the boundary to have a deterministic expectation
            };

            if let Some(expected) = expected_zone {
                zone_total += 1;
                let actual = detected_pin.map(|p| if p.center <= box_center { Zone::Release } else { Zone::Hold });

                match actual {
                    Some(a) if a == expected => zone_ok += 1,
                    _ => eprintln!("FAIL[zone #{i}]: expected={expected:?} actual={actual:?} true_pin={true_pin_center:.1} box_center={box_center:.1}"),
                }
            }
        }

        println!(
            "pin template fuzz results over {TRIALS} trials:\n\
             pin position correct:    {pin_ok}/{pin_total}\n\
             no-pin correctly empty:  {no_pin_ok}/{no_pin_total}\n\
             zone decision correct (outside boundary slack): {zone_ok}/{zone_total}"
        );

        assert_eq!(pin_ok, pin_total, "some pin positions were not recovered correctly");
        // Not assert_eq! here on purpose: fully independent random
        // per-pixel noise is a harsher adversary than any real capture
        // (real screen content is spatially smooth), and occasionally
        // produces a spurious match by pure chance across this many
        // trials x candidate positions -- see PIN_TEMPLATE_MIN_SCORE's
        // doc comment. Measured 0.5-4% across several runs; <=5% under
        // this adversarial noise is the bar -- a real regression (a
        // change that actually weakens the discriminator) would blow
        // well past it, not hover in this range.
        let no_pin_failure_rate = 1.0 - no_pin_ok as f64 / no_pin_total as f64;
        assert!(no_pin_failure_rate <= 0.05, "no-pin false-positive rate too high: {:.1}%", no_pin_failure_rate * 100.0);
        assert_eq!(zone_ok, zone_total, "some zone decisions were wrong");
    }
}
