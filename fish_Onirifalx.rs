//! Autonomous Bar Bot (macOS, terminal) — for the fixed-color minigame skin
//! (the fill bar is always the same light blue, the pin is always the same
//! near-black line, neither ever flashes/changes color). Instead of
//! fish_ctrl_02.rs's 5-point white-flash trick (which only existed because
//! that skin's bar had no cleanly-colored, trackable needle), this tracks
//! the bar and the pin as two separate color blobs and steers toward
//! whichever side the pin is on — see run_pin_tracking_control() below.
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
use fish_bar_bot::{aquarium, capture, config, control, input, vision};

/// Extra Enter presses tacked on after each normal cast sequence, on top of
/// run_trigger_sequence's own burst.
const EXTRA_ENTER_COUNT: u32 = 0;

// ----------------------------------------------------------------------
// BAR + PIN COLOR DETECTION
// ----------------------------------------------------------------------
//
// This skin doesn't flash any color to signal alignment -- the fill bar is
// one fixed light-blue color and the pin is one fixed near-black vertical
// line, both constant regardless of state. So each is tracked directly as
// its own color blob: the bar via vision::find_bar_x_bounds (the same HSV
// blob-matching machinery control::run_control_until_gone uses for the
// original minigame skin), and the pin via the dedicated full-height
// dark-column scan below -- which distinguishes it from the small
// decorative hook icon drawn in the middle of the bar. The hook is also
// near-black, but only partial-height, while the pin spans the whole bar
// top-to-bottom, so a structural "how much of the column's height is dark"
// check tells them apart regardless of where either one sits.
//
// BAR_HSV_RANGES's V-min was retuned from 130 to 110 against a real capture
// session: the static empty track (visible even before any bite) and the
// real bar share almost the same hue/saturation, differing mainly in
// brightness -- but real in-fight bar pixels were observed dipping to
// V=~109-126, below the old 130 cutoff, causing the detector to
// intermittently drop the bar mid-fight. 110 sits just below that
// observed range while staying above the empty track's observed max
// (~112) -- still a tight margin, may need further tuning.
const BAR_HSV_RANGES: &[HsvRange] = &[((90, 25, 110), (135, 150, 255))];
const BAR_MIN_BLOB_AREA_FRACTION: f64 = 0.01;
/// Bridges gaps left in the bar's color mask by the pin (and/or the hook
/// icon) sitting on top of it -- generous, matching the original skin's
/// proven BAR_MERGE_MAX_GAP_PX (config::BAR_MERGE_MAX_GAP_PX = 40), so both
/// occluders being briefly close together doesn't fragment bar detection.
const BAR_MERGE_MAX_GAP_PX: u32 = 40;
/// Unlike the old flash bar (which only ever rested at two extremes and
/// always measured 300px+ wide), this bar can apparently sit anywhere along
/// the track and may be narrower -- this floor just needs to sit above
/// stray noise, not above a specific rest-width.
const BAR_MIN_VALID_WIDTH_PX: f64 = 40.0;

/// Per-channel brightness at/below this counts as "black" for pin
/// detection -- generous margin above pure black (0) to tolerate
/// compression/anti-aliasing, well below the light-blue bar's own channel
/// values.
const PIN_MAX_CHANNEL_VALUE: u8 = 70;
/// Fraction of the frame's height a column of dark pixels must cover to
/// count as (part of) the pin rather than the shorter decorative hook icon.
const MIN_PIN_HEIGHT_FRACTION: f64 = 0.8;
/// A dark column-span wider than this isn't a thin pin line anymore --
/// reject it (almost certainly the hook icon or some other dark blob).
const PIN_MAX_WIDTH_PX: u32 = 12;

fn is_dark_pixel(p: image::Rgba<u8>) -> bool {
    p[0].max(p[1]).max(p[2]) <= PIN_MAX_CHANNEL_VALUE
}

/// Scans columns for ones that are dark for at least MIN_PIN_HEIGHT_FRACTION
/// of the frame's height, groups contiguous qualifying columns into spans,
/// and returns the widest still-thin-enough span as the pin's bounds.
fn find_pin_bounds(frame: &RgbaImage) -> Option<vision::Bounds> {
    let (width, height) = frame.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let min_dark_rows = (MIN_PIN_HEIGHT_FRACTION * height as f64).ceil() as u32;

    let mut dark_columns = vec![false; width as usize];
    for x in 0..width {
        let mut dark_count = 0u32;
        for y in 0..height {
            if is_dark_pixel(*frame.get_pixel(x, y)) {
                dark_count += 1;
            }
        }
        dark_columns[x as usize] = dark_count >= min_dark_rows;
    }

    let mut spans: Vec<(u32, u32)> = Vec::new();
    let mut run_start: Option<u32> = None;
    for x in 0..width {
        if dark_columns[x as usize] {
            run_start.get_or_insert(x);
        } else if let Some(start) = run_start.take() {
            spans.push((start, x));
        }
    }
    if let Some(start) = run_start {
        spans.push((start, width));
    }

    spans.retain(|&(l, r)| r - l <= PIN_MAX_WIDTH_PX);
    let (left, right) = spans.into_iter().max_by_key(|&(l, r)| r - l)?;
    Some(vision::Bounds { left: left as f64, right: right as f64, center: left as f64 + (right - left) as f64 / 2.0 })
}

/// Finds the fill bar's bounds via HSV-range color-blob matching, same
/// machinery control::run_control_until_gone uses for the original skin.
fn find_bar_bounds(frame: &RgbaImage) -> Option<vision::Bounds> {
    let hsv = vision::frame_to_hsv(frame);
    let mask = vision::union_mask(&hsv, BAR_HSV_RANGES);
    vision::find_bar_x_bounds(&mask, hsv.width, hsv.height, BAR_MIN_BLOB_AREA_FRACTION, BAR_MERGE_MAX_GAP_PX, BAR_MIN_VALID_WIDTH_PX)
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
const DAY_NIGHT_CHECK_INTERVAL: Duration = Duration::from_secs(13 * 60);
const AQUARIUM_BOOST_INTERVAL: Duration = Duration::from_secs(31 * 60);
const TOTEM_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

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

const DEFAULT_NO_BITE_TIMEOUT_SECS: u64 = 20;
/// Stop the whole program after this many no-bite timeouts in a row.
const NO_BITE_TIMEOUT_STOP_LIMIT: u32 = 3;

/// Which seventh of the bar the pin is currently in -- see the doc comment
/// above run_pin_tracking_control() for what each one does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone {
    /// First 3/7 of the bar -- do nothing at all, key stays up.
    Release,
    /// Center 1/7 -- slow tap-tracking, not a sustained hold.
    Middle,
    /// Last 3/7 -- hold the key down, don't let go until the pin leaves.
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
// PIN-TRACKING CONTROL LOOP — replaces the 5-point flash approach
// ----------------------------------------------------------------------
//
// Every tick: find the bar's x-bounds and the pin's x-position, split the
// bar's OWN span into sevenths, and act on exactly where the pin is RIGHT
// NOW -- no smoothing, no debounce, an instant switch the moment it
// crosses into a different zone (see Zone above):
//   - Release (first 3/7): do nothing at all -- key stays up, the bar
//     drifts left on its own.
//   - Hold (last 3/7): hold the key down, and don't let go until the pin
//     actually leaves this zone on a later frame -- a single bad
//     detection frame can't drop it, since the zone only changes in
//     response to a fresh reading, never a timer.
//   - Middle (center 1/7): slow tap-tracking -- a brief tap every
//     MIDDLE_TAP_INTERVAL rather than a sustained hold, since holding
//     continuously in such a narrow band would overshoot straight through
//     it into the hold zone.
//
// Confirmed from real play: when the bar first appears there's a brief
// grace period where both the bar and the pin sit locked at a fixed
// position (pin roughly centered) before control is handed over -- no
// special-casing needed, since the pin naturally lands in the middle 1/7
// during it.
//
// The small decorative icon in the middle of the bar (visible only while
// NOT holding -- it appears and disappears along with the key state) is
// why find_pin_bounds() requires a pin candidate to be dark for
// MIN_PIN_HEIGHT_FRACTION of the frame's height: the icon only occupies
// the middle vertically, never the full bar height, so it can't pass that
// check regardless of whether it's on screen this frame.
//
// "Fight over" detection: once bar and pin have both been seen together for
// CONFIRM_STREAK_REQUIRED consecutive ticks (ever_confirmed), losing them
// both for MISSING_GRACE means the fight legitimately ended. Before that
// first confirm, only the no_bite_timeout safety net applies -- the
// natural pre-bite wait (bar not on screen yet at all) is unbounded and
// must not be mistaken for "over."
//
// The streak requirement (not just a single detected frame) exists because
// a one-off false-positive match -- some unrelated on-screen pixel briefly
// satisfying both the bar's HSV range and the pin's darkness threshold at
// once -- would otherwise arm ever_confirmed immediately, and the very
// next tick (both gone again) would trip MISSING_GRACE and declare the
// fight over within a few hundred ms of a cast that never actually got a
// bite. Steering itself isn't gated by the streak -- only "trust this
// enough to arm the fight-over/no-bite-timeout logic" is.

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Fraction of the bar's own width, from the left edge, that counts as
/// Zone::Release.
const RELEASE_FRACTION: f64 = 3.0 / 7.0;
/// Fraction of the bar's own width, from the right edge, that counts as
/// Zone::Hold. Together with RELEASE_FRACTION this leaves the middle 1/7
/// as Zone::Middle.
const HOLD_FRACTION: f64 = 3.0 / 7.0;
/// How long to wait (measured from the end of the previous tap) before the
/// next tap while the pin sits in Zone::Middle -- faster than the initial
/// 200ms, since with MIDDLE_TAP_HOLD longer than this interval, taps end
/// up firing back-to-back anyway (each tap's own hold time already covers
/// most of the gap), giving something close to a trickle-hold rather than
/// isolated clicks.
const MIDDLE_TAP_INTERVAL: Duration = Duration::from_millis(100);
/// Length of each tap while tracking in Zone::Middle -- 0.1s longer than
/// the initial 50ms.
const MIDDLE_TAP_HOLD: Duration = Duration::from_millis(150);
/// How long bar AND pin must both stay undetected (after having been
/// confirmed together for CONFIRM_STREAK_REQUIRED ticks) before the fight
/// is considered over. Real capture data showed the detector legitimately
/// losing and reacquiring lock repeatedly within a single ongoing fight
/// (flickering in and out every few frames, not a sustained loss) -- kept
/// generous so those brief real gaps don't get mistaken for the fight
/// actually ending.
const MISSING_GRACE: Duration = Duration::from_millis(1200);
/// Consecutive ticks bar+pin must both be detected together before it's
/// trusted as a real bite rather than a single-frame false positive.
const CONFIRM_STREAK_REQUIRED: u32 = 4;

/// Measured directly against this skin's real bar position (top-left
/// 441,795 to bottom-right 1001,817) -- close to what calibration::region()
/// was already resolving (left=428 top=791 width=586 height=27), an
/// earlier measurement of (351,726)-(816,745) turned out to be mistaken.
/// Hardcoded here rather than trusting the shared calibration data, in
/// case it needs to diverge again; will need updating if the Roblox
/// window moves or resizes.
const FIXED_REGION: config::Region = config::Region { left: 441, top: 795, width: 1001 - 441, height: 817 - 795 };

fn run_pin_tracking_control(no_bite_timeout: Duration) -> ControlOutcome {
    println!("Pin-tracking control engaged (bar + pin color detection)...");

    let region = FIXED_REGION;
    println!(
        "  watching region: left={} top={} width={} height={}",
        region.left, region.top, region.width, region.height
    );
    // No zone yet, no key held -- matches the real key state, since
    // whatever ended the previous fight already released it.
    let mut current_zone: Option<Zone> = None;
    let mut last_middle_tap: Option<Instant> = None;
    let mut ever_confirmed = false;
    let mut confirm_streak = 0u32;
    let mut missing_since: Option<Instant> = None;
    let session_start = Instant::now();
    // TEMPORARY: dumps a full-width color profile once per cast, on the
    // very first captured frame, now that the region itself is fixed --
    // needed for one fresh calibration pass against the CORRECT patch of
    // screen (the color thresholds tuned earlier were reverse-engineered
    // from the wrong region and are likely meaningless here). Strip once
    // BAR_HSV_RANGES/PIN_MAX_CHANNEL_VALUE are retuned.
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
        }

        let bar = find_bar_bounds(&frame);
        let pin = find_pin_bounds(&frame);

        match (bar, pin) {
            (Some(bar), Some(pin)) => {
                missing_since = None;
                if confirm_streak == 0 {
                    // Permanent (non-overwritten) log of exactly what got
                    // detected at the start of a streak -- the \r status
                    // line below gets erased before a human can read it if
                    // the streak is short-lived, so this is the only
                    // reliable record of what triggered it.
                    println!(
                        "\n  [first detection] bar={:.0}-{:.0} (w={:.0}) pin={:.0}",
                        bar.left,
                        bar.right,
                        bar.right - bar.left,
                        pin.center
                    );
                }
                confirm_streak += 1;
                if confirm_streak == CONFIRM_STREAK_REQUIRED {
                    println!("  [confirmed] {CONFIRM_STREAK_REQUIRED} consecutive detections -- treating as a real bite.");
                }
                if confirm_streak >= CONFIRM_STREAK_REQUIRED {
                    ever_confirmed = true;
                }
                // Fresh, immediate decision every single frame -- exactly
                // where the pin is right now, split against the bar's own
                // width into sevenths.
                let bar_width = bar.right - bar.left;
                if bar_width > 0.0 {
                    let release_edge = bar.left + RELEASE_FRACTION * bar_width;
                    let hold_edge = bar.right - HOLD_FRACTION * bar_width;
                    let zone = if pin.center <= release_edge {
                        Zone::Release
                    } else if pin.center >= hold_edge {
                        Zone::Hold
                    } else {
                        Zone::Middle
                    };

                    if Some(zone) != current_zone {
                        // key_down/key_up each fire a brand-new synthetic
                        // key event -- only call them right on the
                        // transition into a new zone, not every tick,
                        // otherwise a sustained Hold spams repeated
                        // KeyPress events instead of one continuous hold.
                        match zone {
                            Zone::Release | Zone::Middle => input::key_up(config::HOLD_KEY),
                            Zone::Hold => input::key_down(config::HOLD_KEY),
                        }
                        current_zone = Some(zone);
                        if zone == Zone::Middle {
                            last_middle_tap = None; // tap immediately on entry
                        }
                    }

                    if zone == Zone::Middle {
                        let ready = last_middle_tap.map_or(true, |t| t.elapsed() >= MIDDLE_TAP_INTERVAL);
                        if ready {
                            input::tap_key(config::HOLD_KEY, MIDDLE_TAP_HOLD);
                            last_middle_tap = Some(Instant::now());
                        }
                    }

                    print_status(&format!(
                        "[track] bar={:.0}-{:.0} pin={:.0} zone={zone:?} streak={confirm_streak}",
                        bar.left, bar.right, pin.center
                    ));
                }
            }
            _ => {
                confirm_streak = 0;
                print_status(&format!(
                    "[track] bar={} pin={}",
                    if bar.is_some() { "ok" } else { "none" },
                    if pin.is_some() { "ok" } else { "none" }
                ));
                if ever_confirmed {
                    match missing_since {
                        None => missing_since = Some(Instant::now()),
                        Some(since) if since.elapsed() >= MISSING_GRACE => {
                            println!("\nBar/pin both gone -- fight over.");
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
    // Consecutive no-bite timeouts (never confirmed bar+pin together before
    // giving up) -- reset to 0 on any fight that actually engaged. Hitting
    // NO_BITE_TIMEOUT_STOP_LIMIT in a row means something's actually wrong
    // (wrong window focused, region miscalibrated, BAR_HSV_RANGES needs
    // retuning, etc.), not just bad luck, so the program stops instead of
    // casting forever into nothing.
    let mut consecutive_no_bite_timeouts: u32 = 0;
    // Each starts at session_start, so the first check of each kind fires
    // after its own full interval has elapsed, not immediately on cast 1.
    let mut last_day_night_check = session_start;
    let mut last_aquarium_boost = session_start;
    let mut last_totem_refresh = session_start;

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
// TESTS -- fuzz find_bar_bounds()/find_pin_bounds() against synthetic frames
// ----------------------------------------------------------------------
//
// Builds fake bar/track/pin/hook-icon frames at random sizes and geometry
// and checks that:
//   - find_bar_bounds recovers the true bar span, even with the pin and/or
//     hook icon both occluding it
//   - find_pin_bounds recovers the true pin position, NOT the hook icon,
//     regardless of where the hook happens to sit (including overlapping
//     the pin or the bar)
//   - the resulting hold-direction decision matches the true pin-vs-bar
//     relationship whenever it's outside the deadzone
#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    // Fixed reference colors, chosen so each cleanly falls on one side of
    // every threshold involved (BAR_HSV_RANGES, PIN_MAX_CHANNEL_VALUE) --
    // the point of the fuzz is to stress the geometry/position handling,
    // not to re-litigate color-threshold placement.
    const BAR_COLOR: (u8, u8, u8) = (150, 200, 240); // -> HSV ~(103, 96, 240), inside BAR_HSV_RANGES
    const BG_COLOR: (u8, u8, u8) = (40, 40, 90); // -> V=90 < bar's V-min (130); max channel 90 > PIN_MAX_CHANNEL_VALUE
    const DARK_COLOR: (u8, u8, u8) = (10, 10, 10); // pin and hook both use this -- real discriminator is height, not color

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

    #[test]
    fn bar_and_pin_detection_fuzz_2000() {
        const TRIALS: u32 = 2000;
        let (mut bar_ok, mut bar_total) = (0u32, 0u32);
        let (mut pin_ok, mut pin_total) = (0u32, 0u32);
        let (mut zone_ok, mut zone_total) = (0u32, 0u32);

        for i in 0..TRIALS {
            let width = rand::random_range(300u32..=800);
            let height = rand::random_range(20u32..=50);

            let bar_width_frac = rand::random_range(0.2f64..=0.8);
            let bar_left = (rand::random_range(0.0f64..=(1.0 - bar_width_frac)) * width as f64).round() as u32;
            let bar_right = (bar_left as f64 + bar_width_frac * width as f64).round() as u32;
            let bar_right = bar_right.min(width);

            let pin_width = rand::random_range(2u32..=6);
            let pin_x = rand::random_range(0u32..width.saturating_sub(pin_width).max(1));

            let hook_width = rand::random_range(6u32..=10);
            let hook_x = rand::random_range(0u32..width.saturating_sub(hook_width).max(1));
            let hook_height_frac = rand::random_range(0.2f64..=0.45);
            let hook_h = (hook_height_frac * height as f64).round() as u32;
            let hook_y0 = (height.saturating_sub(hook_h)) / 2;

            let mut frame = make_frame(width, height, BG_COLOR);
            paint_rect(&mut frame, bar_left, bar_right, 0, height, BAR_COLOR);
            paint_rect(&mut frame, hook_x, hook_x + hook_width, hook_y0, hook_y0 + hook_h, DARK_COLOR);
            // Pin painted last -- full height, on top of bar/hook alike.
            paint_rect(&mut frame, pin_x, pin_x + pin_width, 0, height, DARK_COLOR);

            let true_pin_center = pin_x as f64 + pin_width as f64 / 2.0;

            // When the pin (and/or the hook, if it also overlaps that same
            // edge) lands right at a bar edge, it can fully occlude a
            // sliver of the true bar color too narrow to count as a
            // "significant" blob (BAR_MIN_BLOB_AREA_FRACTION) -- worse, if
            // the hook sits vertically centered and spans that sliver's
            // full width, it splits the sliver's top/bottom remainder into
            // two disconnected pieces, each independently below the
            // significance floor even when their combined area wouldn't
            // be. That sliver is genuinely gone from the rendered pixels,
            // not a detection bug, so the edge tolerance has to account for
            // how wide such an erased sliver can legitimately be (up to
            // ~1% of width) plus both occluders' widths.
            let edge_tolerance = 0.01 * width as f64 + pin_width as f64 + hook_width as f64 + 3.0;
            let detected_bar = find_bar_bounds(&frame);
            let detected_pin = find_pin_bounds(&frame);

            bar_total += 1;
            match detected_bar {
                Some(detected)
                    if (detected.left - bar_left as f64).abs() <= edge_tolerance
                        && (detected.right - bar_right as f64).abs() <= edge_tolerance =>
                {
                    bar_ok += 1;
                }
                other => {
                    eprintln!(
                        "FAIL[bar #{i}]: true=({bar_left}-{bar_right}) detected={other:?} tolerance={edge_tolerance:.1} pin_x={pin_x} hook_x={hook_x} region={width}x{height}"
                    );
                }
            }

            pin_total += 1;
            match detected_pin {
                Some(detected) if (detected.center - true_pin_center).abs() <= 4.0 => {
                    pin_ok += 1;
                }
                other => {
                    eprintln!(
                        "FAIL[pin #{i}]: true_center={true_pin_center:.1} detected={other:?} hook_x={hook_x} bar=({bar_left}-{bar_right}) region={width}x{height}"
                    );
                }
            }

            // Zone decision: split the TRUE bar span into sevenths, same
            // as run_pin_tracking_control -- pin in the first 3/7 ->
            // Release, last 3/7 -> Hold, middle 1/7 -> Middle. Assert with
            // a little slack around each boundary for detection rounding.
            let bar_width_true = (bar_right - bar_left) as f64;
            let release_edge = bar_left as f64 + RELEASE_FRACTION * bar_width_true;
            let hold_edge = bar_right as f64 - HOLD_FRACTION * bar_width_true;
            let slack = 6.0;

            let expected_zone = if true_pin_center <= release_edge - slack {
                Some(Zone::Release)
            } else if true_pin_center >= hold_edge + slack {
                Some(Zone::Hold)
            } else if true_pin_center >= release_edge + slack && true_pin_center <= hold_edge - slack {
                Some(Zone::Middle)
            } else {
                None // too close to a boundary to have a deterministic expectation
            };

            if let Some(expected) = expected_zone {
                zone_total += 1;
                let actual = match (detected_bar, detected_pin) {
                    (Some(b), Some(p)) => {
                        let w = b.right - b.left;
                        let re = b.left + RELEASE_FRACTION * w;
                        let he = b.right - HOLD_FRACTION * w;
                        if p.center <= re {
                            Some(Zone::Release)
                        } else if p.center >= he {
                            Some(Zone::Hold)
                        } else {
                            Some(Zone::Middle)
                        }
                    }
                    _ => None,
                };

                match actual {
                    Some(a) if a == expected => zone_ok += 1,
                    _ => eprintln!(
                        "FAIL[zone #{i}]: expected={expected:?} actual={actual:?} true_pin={true_pin_center:.1} release_edge={release_edge:.1} hold_edge={hold_edge:.1}"
                    ),
                }
            }
        }

        println!(
            "bar/pin detection fuzz results over {TRIALS} trials:\n\
             bar bounds correct:      {bar_ok}/{bar_total}\n\
             pin center correct:      {pin_ok}/{pin_total}\n\
             zone decision correct (outside boundary slack): {zone_ok}/{zone_total}"
        );

        assert_eq!(bar_ok, bar_total, "some bar spans were not recovered correctly");
        assert_eq!(pin_ok, pin_total, "some pin positions were not recovered correctly (hook icon confusion?)");
        assert_eq!(zone_ok, zone_total, "some zone decisions were wrong");
    }
}
