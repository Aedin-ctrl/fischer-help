//! Cast sequence, day/night icon check + action, and the reactive control
//! loop — port of run_trigger_sequence(), run_moon_action(),
//! is_icon_visible(), and run_control_until_gone() from fish_bar_new.py.

use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use image::RgbaImage;
use rdev::{Button, Key};
use xcap::Monitor;

use crate::calibration;
use crate::capture::grab_region_normalized;
use crate::config::*;
use crate::input::{self, frontmost_app_ok, is_stopped, wait_while_paused_or_unfocused};
use crate::vision::{self, Bounds};

/// Runs the normal cast: 9, 1, hold-click, then a burst of Enters. Pass
/// `skip_pre_trigger = true` when the caller already pressed 9 and 1 itself
/// this cycle (e.g. fisch_new's rod-reset option) so they aren't pressed twice.
///
/// Hold duration / enter count / enter delay come from settings.toml
/// (control_panel's Cast section) rather than the old fixed
/// TRIGGER_HOLD/TRIGGER_ENTER_COUNT/TRIGGER_ENTER_INTERVAL constants —
/// loaded fresh here rather than threaded in as parameters, since this
/// only runs once per cast, not in a hot loop. `enter_count_override`
/// overrides settings.cast_enter_count for just this call (e.g. an
/// interactive per-run prompt) without touching the saved settings.toml
/// value -- pass None to use the normal settings value.
pub fn run_trigger_sequence(skip_pre_trigger: bool, enter_count_override: Option<u32>) {
    println!("Casting...");
    let settings = crate::settings::load();
    let enter_count = enter_count_override.unwrap_or(settings.cast_enter_count);

    if !skip_pre_trigger {
        for &(key, label) in PRE_TRIGGER_KEYS {
            if is_stopped() {
                return;
            }
            println!("  pressing '{label}'...");
            input::tap_key(key, PRE_TRIGGER_KEY_HOLD);
            thread::sleep(PRE_TRIGGER_KEY_DELAY);
        }
    }

    input::mouse_down(Button::Left);
    thread::sleep(Duration::from_millis(settings.cast_hold_ms));
    input::mouse_up(Button::Left);

    thread::sleep(POST_HOLD_ENTER_DELAY);

    for _ in 0..enter_count {
        if is_stopped() {
            return;
        }
        input::tap_key(Key::Return, TRIGGER_ENTER_HOLD);
        thread::sleep(Duration::from_millis(settings.cast_enter_delay_ms));
    }
}

/// The hardcoded fallback day/night check region -- used until this
/// square is calibrated via control_panel's "Day/night region" overlay
/// (region_overlay --day-night). Public so that overlay can show the same
/// rectangle as its own starting position before anything's been dragged.
pub fn default_day_night_region() -> Region {
    let (x1, y1) = MOON_CHECK_CORNER_A;
    let (x2, y2) = MOON_CHECK_CORNER_B;
    let (left, right) = (x1.min(x2), x1.max(x2));
    let (top, bottom) = (y1.min(y2), y1.max(y2));
    Region {
        left: left - MOON_CHECK_MARGIN,
        top: top - MOON_CHECK_MARGIN,
        width: (right - left + 2 * MOON_CHECK_MARGIN) as u32,
        height: (bottom - top + 2 * MOON_CHECK_MARGIN) as u32,
    }
}

pub fn day_night_check_region() -> Region {
    calibration::resolve_day_night_region().unwrap_or_else(default_day_night_region)
}

/// Loads an icon PNG (moon or sun) and crops it down to just its opaque
/// pixels, so template matching compares against the icon shape itself
/// rather than empty transparent margin.
pub fn load_icon_template(path: &std::path::Path, label: &str) -> Option<RgbaImage> {
    let img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            println!("[{label}] couldn't load {}: {e} — {label}-check disabled", path.display());
            return None;
        }
    };

    let (w, h) = img.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    let mut found = false;
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 10 {
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if !found {
        return Some(img);
    }
    Some(image::imageops::crop_imm(&img, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1).to_image())
}

/// True if the given icon template (moon or sun) matches what's currently
/// in the shared day/night check region.
pub fn is_icon_visible(template: Option<&RgbaImage>) -> bool {
    let Some(template) = template else { return false };
    let Ok(frame) = grab_region_normalized(day_night_check_region()) else { return false };

    // The calibrated region is user-dragged via the overlay and can end up
    // a pixel or two smaller than the icon's own saved size (e.g. a 40px
    // tall region against a 41px tall moon.png) -- rather than hard-failing
    // every single check forever, clamp the template down to whatever's
    // actually available instead of refusing to match at all.
    let tw = template.width().min(frame.width());
    let th = template.height().min(frame.height());
    if tw == 0 || th == 0 {
        return false;
    }
    let fitted = image::imageops::crop_imm(template, 0, 0, tw, th).to_image();

    vision::match_template_max(&frame, &fitted) >= MOON_MATCH_THRESHOLD
}

/// The day/night icon was spotted on the every-Nth-session check — press
/// the configured totem slot, then click-and-hold near screen center
/// MOON_ACTION_CLICK_COUNT times, each at a randomly jittered position,
/// then switch back to the rod slot.
pub fn run_moon_action(monitor: &Monitor, totem_key: Key, rod_key: Key) {
    println!("\nDay/night icon detected — pressing totem slot, then clicking...");
    thread::sleep(MOON_ACTION_PRE_PRESS_DELAY);
    input::tap_key(totem_key, MOON_ACTION_TOTEM_HOLD);

    let center_x = monitor.x().unwrap_or(0) + monitor.width().unwrap_or(0) as i32 / 2;
    let center_y = monitor.y().unwrap_or(0) + monitor.height().unwrap_or(0) as i32 / 2;

    for i in 0..MOON_ACTION_CLICK_COUNT {
        if is_stopped() {
            return;
        }
        let x = center_x + rand::random_range(-MOON_ACTION_CLICK_JITTER_PX..=MOON_ACTION_CLICK_JITTER_PX);
        let y = center_y + rand::random_range(-MOON_ACTION_CLICK_JITTER_PX..=MOON_ACTION_CLICK_JITTER_PX);
        println!("  click {}/{} at ({x}, {y})...", i + 1, MOON_ACTION_CLICK_COUNT);
        input::mouse_move(x as f64, y as f64);
        input::mouse_down(Button::Left);
        thread::sleep(MOON_ACTION_CLICK_HOLD);
        input::mouse_up(Button::Left);
        thread::sleep(MOON_ACTION_CLICK_BREAK);
    }

    if is_stopped() {
        return;
    }
    println!("  switching back to rod slot...");
    input::tap_key(rod_key, MOON_ACTION_KEY_HOLD);
}

fn print_track(line: &str) {
    print!("{line}    \r");
    let _ = io::stdout().flush();
}

/// Runs immediately after casting. Jitter-clicks to hold position from the
/// very first frame onward, whether or not the whole bar has appeared yet.
/// Once the bar/pin are actually seen, switches to reacting to the pin's
/// real position. See run_control_until_gone() in fish_bar_new.py for the
/// full behavioral spec this mirrors exactly.
pub fn run_control_until_gone() {
    println!("Jitter-clicking while waiting for the whole bar to appear...");

    let mut bar_seen = false;
    let mut missing_since: Option<Instant> = None;
    let mut cached_bar: Option<Bounds> = None;
    let mut cached_bar_time = Instant::now();
    let mut last_needle_center: Option<f64> = None;
    let mut last_needle_time = Instant::now();
    let mut last_tap_time = Instant::now();
    let mut last_color_debug_time = Instant::now() - COLOR_DEBUG_PRINT_INTERVAL;
    let mut last_decision_time = Instant::now() - DECISION_INTERVAL;
    let start_time = Instant::now();

    let mut frame_count: u32 = 0;
    let mut fps_timer = Instant::now();
    let mut grab_time_total = Duration::ZERO;
    let mut detect_time_total = Duration::ZERO;
    let mut last_loop_hz: u32 = 0;
    let mut last_avg_grab_ms = 0.0;
    let mut last_avg_detect_ms = 0.0;
    let mut last_tick_start = Instant::now() - DECISION_INTERVAL;
    let region = calibration::region();
    let bar_hsv_ranges = calibration::bar_hsv_ranges();

    loop {
        if is_stopped() {
            break;
        }
        if start_time.elapsed() >= CONTROL_TIMEOUT {
            println!(
                "\n{:.0}s of holding elapsed without the bar clearing — forcing a restart.",
                CONTROL_TIMEOUT.as_secs_f64()
            );
            break;
        }

        if input::is_paused() || !frontmost_app_ok() {
            input::key_up(HOLD_KEY);
            if !wait_while_paused_or_unfocused() {
                break;
            }
            continue;
        }

        // Pace the whole loop to a fixed cadence rather than free-running as
        // fast as capture allows, so a late tick is unambiguously a real
        // capture stall, not just timing noise.
        let since_last_tick = last_tick_start.elapsed();
        if since_last_tick < DECISION_INTERVAL {
            thread::sleep(DECISION_INTERVAL - since_last_tick);
        }
        last_tick_start = Instant::now();

        let now = Instant::now();
        let t0 = now;
        let Ok(frame) = grab_region_normalized(region) else {
            continue;
        };
        let t1 = Instant::now();
        let needle_hint = (now.duration_since(last_needle_time) < NEEDLE_HINT_MAX_AGE)
            .then_some(last_needle_center)
            .flatten();
        let result = vision::detect_both(
            &frame,
            bar_hsv_ranges,
            NEEDLE_HSV_RANGES,
            BAR_MIN_BLOB_AREA_FRACTION,
            BAR_MERGE_MAX_GAP_PX,
            BAR_MIN_VALID_WIDTH_PX,
            NEEDLE_MAX_WIDTH_PX,
            needle_hint,
            NEEDLE_MAX_JUMP_PX,
        );
        let t2 = Instant::now();
        grab_time_total += t1 - t0;
        detect_time_total += t2 - t1;

        if let Some(needle) = result.needle {
            last_needle_center = Some(needle.center);
            last_needle_time = now;
        }

        if let Some(bar) = result.bar {
            cached_bar = Some(bar);
            cached_bar_time = now;
        }

        if result.bar.is_none() && result.needle.is_none() {
            if bar_seen {
                match missing_since {
                    None => missing_since = Some(now),
                    Some(since) if now.duration_since(since) >= MISSING_GRACE => {
                        println!("Bar and needle both gone — check complete.");
                        break;
                    }
                    _ => {}
                }
            }

            let clicked = if now.duration_since(last_tap_time) >= KEEPALIVE_TAP_INTERVAL {
                input::tap_key(HOLD_KEY, TAP_SECONDS);
                last_tap_time = Instant::now();
                true
            } else {
                input::key_up(HOLD_KEY);
                false
            };
            print_track(&format!("[track] bar=none  needle=none  {}", if clicked { "CLICK" } else { "idle " }));
            continue;
        }

        missing_since = None; // at least one of bar/needle visible

        let effective_bar = result.bar.or_else(|| {
            if now.duration_since(cached_bar_time) <= BAR_CACHE_MAX_AGE {
                cached_bar
            } else {
                None
            }
        });

        if effective_bar.is_none() || result.needle.is_none() {
            if now.duration_since(last_color_debug_time) >= COLOR_DEBUG_PRINT_INTERVAL {
                if result.bar.is_none() {
                    if let Some((lo, hi)) = vision::sample_unrecognized_color(&result.hsv, NEEDLE_HSV_RANGES) {
                        println!("\n[color debug] control bar not recognized — unknown blob HSV range: low={lo:?} high={hi:?}");
                    }
                } else if let Some((lo, hi)) = vision::sample_unrecognized_color(&result.hsv, bar_hsv_ranges) {
                    println!("\n[needle debug] needle not recognized — unknown blob HSV range: low={lo:?} high={hi:?}");
                }
                last_color_debug_time = now;
            }

            let clicked = if now.duration_since(last_tap_time) >= KEEPALIVE_TAP_INTERVAL {
                input::tap_key(HOLD_KEY, TAP_SECONDS);
                last_tap_time = Instant::now();
                true
            } else {
                input::key_up(HOLD_KEY);
                false
            };
            let bar_str = effective_bar.map_or("none".to_string(), |b| format!("{:.0}-{:.0}", b.left, b.right));
            let needle_str = result.needle.map_or("none".to_string(), |n| format!("{:.0}-{:.0}", n.left, n.right));
            print_track(&format!(
                "[track] bar={bar_str}  needle={needle_str}  {}",
                if clicked { "CLICK" } else { "idle " }
            ));
            continue;
        }

        if !bar_seen {
            println!("Whole bar appeared — engaging reactive control.");
        }
        bar_seen = true;

        if now.duration_since(last_decision_time) >= DECISION_INTERVAL {
            last_decision_time = now;
            let bar = effective_bar.unwrap();
            let needle = result.needle.unwrap();
            let bar_target = bar.left + PIN_TARGET_FRACTION * (bar.right - bar.left);
            let error = needle.center - bar_target;

            if error.abs() > region.width as f64 {
                println!(
                    "\n[error debug] impossible error={error:.0}px -- bar={} bar_left={:.0} bar_right={:.0} bar_target={bar_target:.0} needle_center={:.0} needle_bounds={:.0}-{:.0}",
                    if result.bar.is_none() { "cached" } else { "fresh" },
                    bar.left, bar.right, needle.center, needle.left, needle.right
                );
            }

            let clicked = if error >= 0.0 || now.duration_since(last_tap_time) >= KEEPALIVE_TAP_INTERVAL {
                input::tap_key(HOLD_KEY, TAP_SECONDS);
                last_tap_time = Instant::now();
                true
            } else {
                false
            };

            print_track(&format!(
                "[track] bar={:.0}-{:.0} (target={bar_target:.0})  needle={:.0}-{:.0} (center={:.0})  error={error:+.0}px  {}  [{last_loop_hz}Hz grab={last_avg_grab_ms:.0}ms detect={last_avg_detect_ms:.0}ms]",
                bar.left, bar.right, needle.left, needle.right, needle.center,
                if clicked { "CLICK" } else { "idle " }
            ));
        }

        frame_count += 1;
        let now = Instant::now();
        if now.duration_since(fps_timer) >= Duration::from_secs(1) {
            last_loop_hz = frame_count;
            last_avg_grab_ms = if frame_count > 0 { grab_time_total.as_secs_f64() / frame_count as f64 * 1000.0 } else { 0.0 };
            last_avg_detect_ms = if frame_count > 0 { detect_time_total.as_secs_f64() / frame_count as f64 * 1000.0 } else { 0.0 };
            frame_count = 0;
            fps_timer = now;
            grab_time_total = Duration::ZERO;
            detect_time_total = Duration::ZERO;
        }
    }

    // Guaranteed release no matter which path above broke out — the bar
    // being gone must never leave the key held.
    input::key_up(HOLD_KEY);
}
