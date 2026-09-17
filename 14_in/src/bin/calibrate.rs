//! Calibration tool: captures the EXACT pixels the bot sees (same region,
//! same Retina normalization) and reports real HSV numbers, instead of
//! guessing from a screenshot that might not match what mss/xcap actually
//! grabs.
//!
//!   cargo run --release --bin calibrate            # 5 fishing cycles
//!   cargo run --release --bin calibrate -- 8        # 8 cycles instead
//!   cargo run --release --bin calibrate -- moon     # day/night icon region, fixed duration
//!
//! Bar mode is fully autonomous: it casts (the same cast sequence the real
//! bot uses), blind-jitter-taps to keep the line engaged, watches for the
//! bar to appear and disappear, then recasts — repeating for N cycles and
//! stopping itself. You just switch to the game and wait; F8 pause / ESC
//! quit both work the whole time, same as the real bot.
//!
//! Cycle boundaries are detected using ONLY CONTROLLED_BAR_HSV_RANGES[0]
//! (the bright/white range, the one range in config.rs that's actually
//! confirmed rather than guessed), specifically so this doesn't inherit
//! the same background-bleed bug we're trying to diagnose. Every captured
//! frame's pixels still get folded into one of two histograms — "active"
//! (during a cycle) vs "idle" (between cycles) — so the report shows
//! which colors are actually bar/needle-specific vs just always-present
//! background.

use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use fish_bar_bot::{capture, config, control, input, vision};
use image::imageops::FilterType;
use image::RgbaImage;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const COUNTDOWN_SECONDS: u64 = 8;
const MAX_SESSION_SECONDS: u64 = 600; // safety cap so this can't hang forever
const MAX_SNAPSHOTS: usize = 60;
const MID_SNAPSHOT_INTERVAL: Duration = Duration::from_millis(400);
const MID_SNAPSHOTS_PER_CYCLE: u32 = 8;
const UPSCALE: u32 = 6;

type Histogram = HashMap<(u8, u8, u8), u64>;

fn bucket_key(h: u8, s: u8, v: u8) -> (u8, u8, u8) {
    // H in steps of 3 (0..=179 -> 60 buckets), S/V in steps of 16.
    (h / 3, s / 16, v / 16)
}

fn bucket_range(key: (u8, u8, u8)) -> (u8, u8, u8, u8, u8, u8) {
    let (hb, sb, vb) = key;
    let h_lo = hb * 3;
    let s_lo = sb * 16;
    let v_lo = vb * 16;
    (h_lo, h_lo + 2, s_lo, (s_lo + 15).min(255), v_lo, (v_lo + 15).min(255))
}

fn record(hist: &mut Histogram, pixels: &mut u64, hsv: &vision::HsvFrame) {
    for &(h, s, v) in &hsv.pixels {
        *hist.entry(bucket_key(h, s, v)).or_insert(0) += 1;
        *pixels += 1;
    }
}

fn save_snapshot(out_dir: &Path, prefix: &str, name: &str, frame: &RgbaImage) -> anyhow::Result<()> {
    let upscaled = image::imageops::resize(frame, frame.width() * UPSCALE, frame.height() * UPSCALE, FilterType::Nearest);
    let path = out_dir.join(format!("{prefix}_{name}.png"));
    upscaled.save(&path)?;
    println!("  saved {}", path.display());
    Ok(())
}

fn write_report(
    out_dir: &Path,
    prefix: &str,
    region: config::Region,
    active_hist: &Histogram,
    active_pixels: u64,
    idle_hist: &Histogram,
    idle_pixels: u64,
    frame_count: u32,
    cycles: u32,
) -> anyhow::Result<()> {
    let mut report = String::new();
    report.push_str(&format!(
        "region ({}, {}, {}x{})\ncycles completed: {cycles}\nframes captured: {frame_count}\n\
active pixels: {active_pixels}   idle pixels: {idle_pixels}\n\n",
        region.left, region.top, region.width, region.height
    ));

    let idle_pct_for = |key: &(u8, u8, u8)| -> f64 {
        if idle_pixels == 0 {
            return 0.0;
        }
        100.0 * (*idle_hist.get(key).unwrap_or(&0) as f64) / (idle_pixels as f64)
    };

    let mut active: Vec<(&(u8, u8, u8), &u64)> = active_hist.iter().collect();
    active.sort_by(|a, b| b.1.cmp(a.1));
    report.push_str("=== ACTIVE (bar visible) HSV clusters — H,S,V on OpenCV's 0-179/0-255 scale ===\n");
    report.push_str(&format!("{:<26} {:>8} {:>10} {:>10}\n", "HSV range", "active%", "idle%", "pixels"));
    for (key, count) in active.iter().take(35) {
        let (h_lo, h_hi, s_lo, s_hi, v_lo, v_hi) = bucket_range(**key);
        let active_pct = 100.0 * (**count as f64) / (active_pixels as f64);
        let idle_pct = idle_pct_for(key);
        report.push_str(&format!(
            "H{h_lo:>3}-{h_hi:<3} S{s_lo:>3}-{s_hi:<3} V{v_lo:>3}-{v_hi:<3}   {active_pct:>7.2}% {idle_pct:>9.2}% {:>10}\n",
            **count
        ));
    }

    let mut idle: Vec<(&(u8, u8, u8), &u64)> = idle_hist.iter().collect();
    idle.sort_by(|a, b| b.1.cmp(a.1));
    report.push_str("\n=== IDLE (bar not visible) HSV clusters — for reference, to exclude from ranges ===\n");
    report.push_str(&format!("{:<26} {:>8} {:>10}\n", "HSV range", "idle%", "pixels"));
    for (key, count) in idle.iter().take(15) {
        let (h_lo, h_hi, s_lo, s_hi, v_lo, v_hi) = bucket_range(**key);
        let idle_pct = 100.0 * (**count as f64) / (idle_pixels as f64);
        report.push_str(&format!(
            "H{h_lo:>3}-{h_hi:<3} S{s_lo:>3}-{s_hi:<3} V{v_lo:>3}-{v_hi:<3}   {idle_pct:>7.2}% {:>10}\n",
            **count
        ));
    }

    report.push_str(
        "\nClusters with high active% but also high idle% are background, not the bar/pin —\n\
prefer clusters that show up strongly in ACTIVE and are near-absent from IDLE.\n",
    );

    print!("\n{report}");
    let path = out_dir.join(format!("{prefix}_report.txt"));
    std::fs::write(&path, &report)?;
    println!("\nsaved {}", path.display());
    Ok(())
}

fn countdown(seconds: u64, message: &str) {
    println!("{message}");
    for i in (1..=seconds).rev() {
        println!("{i}...");
        thread::sleep(Duration::from_secs(1));
    }
    println!("Go!\n");
}

fn run_moon_mode() -> anyhow::Result<()> {
    const CAPTURE_SECONDS: u64 = 6;

    let (x1, y1) = config::MOON_CHECK_CORNER_A;
    let (x2, y2) = config::MOON_CHECK_CORNER_B;
    let (left, right) = (x1.min(x2), x1.max(x2));
    let (top, bottom) = (y1.min(y2), y1.max(y2));
    let region = config::Region {
        left: left - config::MOON_CHECK_MARGIN,
        top: top - config::MOON_CHECK_MARGIN,
        width: (right - left + 2 * config::MOON_CHECK_MARGIN) as u32,
        height: (bottom - top + 2 * config::MOON_CHECK_MARGIN) as u32,
    };

    countdown(COUNTDOWN_SECONDS, "Get the moon/sun icon on screen now.");

    let mut hist: Histogram = HashMap::new();
    let mut total_pixels = 0u64;
    let mut snapshots = Vec::new();
    let start = Instant::now();
    let mut frame_count = 0u32;

    while start.elapsed() < Duration::from_secs(CAPTURE_SECONDS) {
        let tick_start = Instant::now();
        if let Ok(frame) = capture::grab_region_normalized(region) {
            frame_count += 1;
            let hsv = vision::frame_to_hsv(&frame);
            record(&mut hist, &mut total_pixels, &hsv);
            if snapshots.len() < 3 {
                snapshots.push(frame);
            }
        }
        let elapsed = tick_start.elapsed();
        if elapsed < POLL_INTERVAL {
            thread::sleep(POLL_INTERVAL - elapsed);
        }
    }

    println!("\nCaptured {frame_count} frames, {total_pixels} pixels.\n");
    let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (i, snap) in snapshots.iter().enumerate() {
        save_snapshot(&out_dir, "calib_moon", &format!("{}", i + 1), snap)?;
    }
    write_report(&out_dir, "calib_moon", region, &hist, total_pixels, &HashMap::new(), 0, frame_count, 0)?;
    Ok(())
}

/// Bar visible/not-visible using ONLY the confirmed bright/white range —
/// deliberately not the full CONTROLLED_BAR_HSV_RANGES union, since that
/// union is exactly what's mis-detecting background as "the bar" right
/// now. This is just for cycle-boundary gating, not the real detector.
fn white_bar_visible(hsv: &vision::HsvFrame) -> bool {
    let white_range = [config::CONTROLLED_BAR_HSV_RANGES[0]];
    let mask = vision::union_mask(hsv, &white_range);
    vision::find_bar_x_bounds(
        &mask,
        hsv.width,
        hsv.height,
        config::BAR_MIN_BLOB_AREA_FRACTION,
        config::BAR_MERGE_MAX_GAP_PX,
        config::BAR_MIN_VALID_WIDTH_PX,
    )
    .is_some()
}

enum CycleOutcome {
    Completed,
    TimedOut,
    Stopped,
}

/// Runs after a cast. Blind-jitter-taps HOLD_KEY on the same keepalive
/// cadence the real bot uses before the bar is confirmed visible (no
/// color calibration needed for that part), watches for the bar to
/// appear then disappear, and records every frame's pixels into the
/// active/idle histograms along the way.
#[allow(clippy::too_many_arguments)]
fn observe_cycle(
    out_dir: &Path,
    cycle_num: u32,
    region: config::Region,
    active_hist: &mut Histogram,
    active_pixels: &mut u64,
    idle_hist: &mut Histogram,
    idle_pixels: &mut u64,
    frame_count: &mut u32,
    snapshots_saved: &mut usize,
) -> anyhow::Result<CycleOutcome> {
    let mut bar_seen = false;
    let mut missing_since: Option<Instant> = None;
    let mut last_tap_time = Instant::now() - config::KEEPALIVE_TAP_INTERVAL;
    let mut last_mid_snapshot: Option<Instant> = None;
    let mut mid_snapshot_count = 0u32;
    let cycle_start = Instant::now();

    loop {
        if input::is_stopped() {
            input::key_up(config::HOLD_KEY);
            return Ok(CycleOutcome::Stopped);
        }
        if input::is_paused() || !input::frontmost_app_ok() {
            input::key_up(config::HOLD_KEY);
            if !input::wait_while_paused_or_unfocused() {
                return Ok(CycleOutcome::Stopped);
            }
            continue;
        }
        if cycle_start.elapsed() >= config::CONTROL_TIMEOUT {
            input::key_up(config::HOLD_KEY);
            return Ok(CycleOutcome::TimedOut);
        }

        let tick_start = Instant::now();
        let Ok(frame) = capture::grab_region_normalized(region) else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };
        *frame_count += 1;
        let hsv = vision::frame_to_hsv(&frame);
        let visible = white_bar_visible(&hsv);

        let now = Instant::now();
        if now.duration_since(last_tap_time) >= config::KEEPALIVE_TAP_INTERVAL {
            input::tap_key(config::HOLD_KEY, config::TAP_SECONDS);
            last_tap_time = Instant::now();
        }

        if visible {
            if !bar_seen {
                bar_seen = true;
                missing_since = None;
                last_mid_snapshot = Some(Instant::now());
                println!("  bar appeared...");
                if *snapshots_saved < MAX_SNAPSHOTS {
                    save_snapshot(out_dir, "calib_bar", &format!("cycle{cycle_num}_start"), &frame)?;
                    *snapshots_saved += 1;
                }
            } else if mid_snapshot_count < MID_SNAPSHOTS_PER_CYCLE
                && last_mid_snapshot.is_some_and(|t| t.elapsed() >= MID_SNAPSHOT_INTERVAL)
                && *snapshots_saved < MAX_SNAPSHOTS
            {
                // Periodic snapshots WHILE the bar is up, not just at the
                // start/end — the start frame is always the same static
                // pre-movement arrows/pin display, never the actual moving
                // needle, so without these we'd never see what the needle
                // looks like in motion.
                mid_snapshot_count += 1;
                save_snapshot(out_dir, "calib_bar", &format!("cycle{cycle_num}_mid{mid_snapshot_count}"), &frame)?;
                *snapshots_saved += 1;
                last_mid_snapshot = Some(Instant::now());
            }
            record(active_hist, active_pixels, &hsv);
        } else {
            record(idle_hist, idle_pixels, &hsv);

            if bar_seen {
                match missing_since {
                    None => missing_since = Some(Instant::now()),
                    Some(since) if since.elapsed() >= config::MISSING_GRACE => {
                        if *snapshots_saved < MAX_SNAPSHOTS {
                            save_snapshot(out_dir, "calib_bar", &format!("cycle{cycle_num}_end"), &frame)?;
                            *snapshots_saved += 1;
                        }
                        input::key_up(config::HOLD_KEY);
                        return Ok(CycleOutcome::Completed);
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

fn run_bar_mode(target_cycles: u32) -> anyhow::Result<()> {
    let region = config::REGION;
    input::start_hotkey_listener();
    println!("F8 = pause/resume, ESC = quit — both work the whole time, same as the real bot.");

    countdown(
        COUNTDOWN_SECONDS,
        &format!("Switch to the game now — this will cast, fish, and record {target_cycles} cycles on its own."),
    );

    let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Ok(frame) = capture::grab_region_normalized(region) {
        save_snapshot(&out_dir, "calib_bar", "idle_background", &frame)?;
    }

    let mut active_hist: Histogram = HashMap::new();
    let mut idle_hist: Histogram = HashMap::new();
    let mut active_pixels = 0u64;
    let mut idle_pixels = 0u64;
    let mut frame_count = 0u32;
    let mut snapshots_saved = 0usize;
    let mut cycles_completed = 0u32;
    let mut attempts = 0u32;
    let max_attempts = target_cycles * 3 + 3;
    let session_start = Instant::now();

    while cycles_completed < target_cycles && attempts < max_attempts {
        if input::is_stopped() {
            println!("\nStopped by user (ESC).");
            break;
        }
        if session_start.elapsed() > Duration::from_secs(MAX_SESSION_SECONDS) {
            println!(
                "\nHit the {MAX_SESSION_SECONDS}s safety cap with only {cycles_completed}/{target_cycles} \
cycles seen — stopping early with whatever was captured."
            );
            break;
        }
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        attempts += 1;
        println!("\nCast {attempts} (cycle {}/{target_cycles})...", cycles_completed + 1);
        control::run_trigger_sequence(false, None);
        if input::is_stopped() {
            break;
        }

        match observe_cycle(
            &out_dir,
            cycles_completed + 1,
            region,
            &mut active_hist,
            &mut active_pixels,
            &mut idle_hist,
            &mut idle_pixels,
            &mut frame_count,
            &mut snapshots_saved,
        )? {
            CycleOutcome::Completed => {
                cycles_completed += 1;
                println!("Cycle {cycles_completed}/{target_cycles} complete.");
            }
            CycleOutcome::TimedOut => {
                println!("  bar never appeared within {}s — recasting.", config::CONTROL_TIMEOUT.as_secs());
            }
            CycleOutcome::Stopped => break,
        }

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(config::HOLD_KEY);

    println!(
        "\nDone — {cycles_completed}/{target_cycles} cycles from {attempts} casts, {frame_count} frames \
({active_pixels} active px, {idle_pixels} idle px).\n"
    );

    write_report(&out_dir, "calib_bar", region, &active_hist, active_pixels, &idle_hist, idle_pixels, frame_count, cycles_completed)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("moon") {
        return run_moon_mode();
    }

    let target_cycles: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5);
    run_bar_mode(target_cycles)
}
