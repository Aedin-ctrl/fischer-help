//! Throwaway debug script: every 1s, captures the day/night check region
//! and prints the sun/moon template match scores plus whether each one
//! crosses MOON_MATCH_THRESHOLD. Also dumps diagnostics (monitor info,
//! frame/template sizes) once at startup and saves the live captured
//! region to throwaway_capture.png every tick, so a mismatch (wrong
//! monitor, region not actually over the icon, etc.) is easy to spot.
//! Just for checking whether sun/moon detection is actually working --
//! not wired into the real bot loop.
//!
//! F8 = pause/resume, ESC = quit (same hotkeys as the real bot).

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use rdev::Key;
use xcap::Monitor;

use fish_bar_bot::config::Region;
use fish_bar_bot::{capture, config, control, input, vision};

fn day_night_check_region() -> Region {
    let (x1, y1) = config::MOON_CHECK_CORNER_A;
    let (x2, y2) = config::MOON_CHECK_CORNER_B;
    let (left, right) = (x1.min(x2), x1.max(x2));
    let (top, bottom) = (y1.min(y2), y1.max(y2));
    Region {
        left: left - config::MOON_CHECK_MARGIN,
        top: top - config::MOON_CHECK_MARGIN,
        width: (right - left + 2 * config::MOON_CHECK_MARGIN) as u32,
        height: (bottom - top + 2 * config::MOON_CHECK_MARGIN) as u32,
    }
}

fn main() -> anyhow::Result<()> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let asset_dir = if exe_dir.join("moon.png").exists() || exe_dir.join("sun.png").exists() {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };

    let sun = control::load_icon_template(&asset_dir.join("sun.png"), "sun");
    let moon = control::load_icon_template(&asset_dir.join("moon.png"), "moon");

    if sun.is_none() {
        println!("sun.png failed to load -- sun detection disabled.");
    }
    if moon.is_none() {
        println!("moon.png failed to load -- moon detection disabled.");
    }
    if let Some(t) = &sun {
        println!("sun.png cropped template size: {}x{}", t.width(), t.height());
    }
    if let Some(t) = &moon {
        println!("moon.png cropped template size: {}x{}", t.width(), t.height());
    }

    let region = day_night_check_region();
    println!("Check region (absolute): {region:?}");

    match Monitor::from_point(region.left, region.top) {
        Ok(m) => println!(
            "Resolved monitor: x={:?} y={:?} w={:?} h={:?} primary={:?}",
            m.x(), m.y(), m.width(), m.height(), m.is_primary()
        ),
        Err(e) => println!("Could not resolve a monitor at the region's top-left corner: {e}"),
    }

    let capture_path = asset_dir.join("throwaway_capture.png");
    println!("Saving each captured frame to {} for visual inspection.", capture_path.display());

    println!("Switch to Roblox now -- starting in:");
    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    println!("Watching every 1s (threshold={:.2}). F8 = pause/resume, ESC = quit.", config::MOON_MATCH_THRESHOLD);
    input::start_hotkey_listener();

    while !input::is_stopped() {
        if input::is_paused() {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        match capture::grab_region_normalized(region) {
            Ok(frame) => {
                if let Err(e) = frame.save(&capture_path) {
                    println!("(couldn't save capture: {e})");
                }

                let sun_score = sun.as_ref().map(|t| vision::match_template_max(&frame, t));
                let moon_score = moon.as_ref().map(|t| vision::match_template_max(&frame, t));

                let fmt = |score: Option<f64>, label: &str| -> String {
                    match score {
                        Some(s) => format!(
                            "{label}={s:.3}{}",
                            if s >= config::MOON_MATCH_THRESHOLD { " <-- DETECTED" } else { "" }
                        ),
                        None => format!("{label}=n/a"),
                    }
                };

                println!("frame={}x{}   {}   {}", frame.width(), frame.height(), fmt(sun_score, "sun"), fmt(moon_score, "moon"));
            }
            Err(e) => println!("capture failed: {e}"),
        }

        let mut slept = Duration::ZERO;
        while slept < Duration::from_secs(1) && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(Key::Space);
    println!("Stopped.");
    Ok(())
}
