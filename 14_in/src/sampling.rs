//! Live color/region sampling used by both calibrate_rod (produces a new
//! calibration) and the control panel's "Recalibrate Colors" action
//! (re-samples against the CURRENT region without re-placing it). Casts
//! using the bot's real trigger sequence and records active-vs-idle HSV
//! histograms scoped to a given region.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{self, HsvRange, Region};
use crate::{capture, control, input, vision};

const SAMPLE_CYCLES: u32 = 5;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub type Histogram = HashMap<(u8, u8, u8), u64>;

fn bucket_key(h: u8, s: u8, v: u8) -> (u8, u8, u8) {
    (h / 3, s / 16, v / 16)
}

fn record(hist: &mut Histogram, pixels: &mut u64, hsv: &vision::HsvFrame) {
    for &(h, s, v) in &hsv.pixels {
        *hist.entry(bucket_key(h, s, v)).or_insert(0) += 1;
        *pixels += 1;
    }
}

/// Casts (reusing the bot's real trigger sequence), waits for the bar to
/// appear/disappear SAMPLE_CYCLES times, and records every frame's pixels
/// into active/idle histograms scoped to `region`.
pub fn sample_region(region: Region) -> anyhow::Result<(Histogram, u64, Histogram, u64)> {
    input::start_hotkey_listener();
    println!("F8 = pause/resume, ESC = quit, during sampling too.\n");

    let mut active_hist = Histogram::new();
    let mut idle_hist = Histogram::new();
    let mut active_pixels = 0u64;
    let mut idle_pixels = 0u64;
    let mut cycles_completed = 0u32;
    let mut attempts = 0u32;
    let max_attempts = SAMPLE_CYCLES * 3 + 3;

    while cycles_completed < SAMPLE_CYCLES && attempts < max_attempts {
        if input::is_stopped() {
            println!("stopped by user.");
            break;
        }
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        attempts += 1;
        println!("cast {attempts} (cycle {}/{SAMPLE_CYCLES})...", cycles_completed + 1);
        control::run_trigger_sequence(false, None);
        if input::is_stopped() {
            break;
        }

        match observe_one_cycle(region, &mut active_hist, &mut active_pixels, &mut idle_hist, &mut idle_pixels)? {
            true => {
                cycles_completed += 1;
                println!("  cycle {cycles_completed}/{SAMPLE_CYCLES} complete.");
            }
            false => println!("  bar never appeared — recasting."),
        }

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(config::HOLD_KEY);
    Ok((active_hist, active_pixels, idle_hist, idle_pixels))
}

/// Returns true if a full appear-then-disappear cycle was observed.
fn observe_one_cycle(region: Region, active_hist: &mut Histogram, active_pixels: &mut u64, idle_hist: &mut Histogram, idle_pixels: &mut u64) -> anyhow::Result<bool> {
    let mut bar_seen = false;
    let mut missing_since: Option<Instant> = None;
    let mut last_tap_time = Instant::now() - config::KEEPALIVE_TAP_INTERVAL;
    let cycle_start = Instant::now();

    loop {
        if input::is_stopped() {
            input::key_up(config::HOLD_KEY);
            return Ok(false);
        }
        if input::is_paused() || !input::frontmost_app_ok() {
            input::key_up(config::HOLD_KEY);
            if !input::wait_while_paused_or_unfocused() {
                return Ok(false);
            }
            continue;
        }
        if cycle_start.elapsed() >= config::CONTROL_TIMEOUT {
            input::key_up(config::HOLD_KEY);
            return Ok(false);
        }

        let tick_start = Instant::now();
        let Ok(frame) = capture::grab_region_normalized(region) else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };
        let hsv = vision::frame_to_hsv(&frame);
        let white_range = [config::CONTROLLED_BAR_HSV_RANGES[0]];
        let mask = vision::union_mask(&hsv, &white_range);
        let visible = vision::find_bar_x_bounds(
            &mask,
            hsv.width,
            hsv.height,
            config::BAR_MIN_BLOB_AREA_FRACTION,
            config::BAR_MERGE_MAX_GAP_PX,
            config::BAR_MIN_VALID_WIDTH_PX,
        )
        .is_some();

        let now = Instant::now();
        if now.duration_since(last_tap_time) >= config::KEEPALIVE_TAP_INTERVAL {
            input::tap_key(config::HOLD_KEY, config::TAP_SECONDS);
            last_tap_time = Instant::now();
        }

        if visible {
            if !bar_seen {
                bar_seen = true;
                missing_since = None;
                println!("  bar appeared...");
            }
            record(active_hist, active_pixels, &hsv);
        } else {
            record(idle_hist, idle_pixels, &hsv);
            if bar_seen {
                match missing_since {
                    None => missing_since = Some(Instant::now()),
                    Some(since) if since.elapsed() >= config::MISSING_GRACE => {
                        input::key_up(config::HOLD_KEY);
                        return Ok(true);
                    }
                    _ => {}
                }
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < POLL_INTERVAL {
            thread::sleep(POLL_INTERVAL - elapsed);
        }
    }
}

/// Picks buckets that show up strongly in ACTIVE frames and are
/// near-absent in IDLE frames, then merges hue-adjacent buckets into
/// contiguous ranges with a margin — same spirit as config.rs's hand-
/// tuned ranges, just computed instead of eyeballed.
pub fn derive_hsv_ranges(active: &Histogram, active_pixels: u64, idle: &Histogram, idle_pixels: u64) -> Vec<HsvRange> {
    if active_pixels == 0 {
        return Vec::new();
    }

    let mut good: Vec<(u8, u8, u8)> = active
        .iter()
        .filter_map(|(&key, &count)| {
            let active_pct = count as f64 / active_pixels as f64;
            if active_pct < 0.002 {
                return None;
            }
            let idle_pct = idle.get(&key).copied().unwrap_or(0) as f64 / idle_pixels.max(1) as f64;
            (idle_pct < active_pct * 0.25).then_some(key)
        })
        .collect();

    if good.is_empty() {
        return Vec::new();
    }
    good.sort_unstable();

    let mut clusters: Vec<Vec<(u8, u8, u8)>> = Vec::new();
    for b in good {
        match clusters.last_mut() {
            Some(cluster) if (b.0 as i32 - cluster.last().unwrap().0 as i32) <= 2 => cluster.push(b),
            _ => clusters.push(vec![b]),
        }
    }

    clusters
        .into_iter()
        .map(|cluster| {
            let h_lo = cluster.iter().map(|b| b.0).min().unwrap() as i32 * 3;
            let h_hi = cluster.iter().map(|b| b.0).max().unwrap() as i32 * 3 + 2;
            let s_lo = cluster.iter().map(|b| b.1).min().unwrap() as i32 * 16;
            let s_hi = cluster.iter().map(|b| b.1).max().unwrap() as i32 * 16 + 15;
            let v_lo = cluster.iter().map(|b| b.2).min().unwrap() as i32 * 16;
            let v_hi = cluster.iter().map(|b| b.2).max().unwrap() as i32 * 16 + 15;
            (
                ((h_lo - 3).clamp(0, 179) as u8, (s_lo - 16).clamp(0, 255) as u8, (v_lo - 16).clamp(0, 255) as u8),
                ((h_hi + 3).clamp(0, 179) as u8, (s_hi + 16).clamp(0, 255) as u8, (v_hi + 16).clamp(0, 255) as u8),
            )
        })
        .collect()
}
