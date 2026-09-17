//! Read-only diagnostic for the day/night icon check. Doesn't need any
//! overlay window (which can't render over a fullscreen Roblox Space) --
//! just captures the exact region the real bot checks, saves it to
//! day_night_debug_capture.png for visual inspection, and prints the
//! match score against both sun.png and moon.png so it's obvious whether
//! the region is even pointing at the right spot, and how close either
//! score is to MOON_MATCH_THRESHOLD.
//!
//! Never touches input. Ctrl+C to stop.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use fish_bar_bot::{capture, config, control, vision};

fn main() -> anyhow::Result<()> {
    for i in (1..=5).rev() {
        println!("day_night_debug capturing in {i}s -- switch to Roblox now...");
        thread::sleep(Duration::from_secs(1));
    }

    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from("."));
    let asset_dir = if exe_dir.join("moon.png").exists() || exe_dir.join("sun.png").exists() {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };

    let sun = control::load_icon_template(&asset_dir.join("sun.png"), "sun");
    let moon = control::load_icon_template(&asset_dir.join("moon.png"), "moon");

    let region = control::day_night_check_region();
    println!("day/night check region: {region:?}");

    let frame = capture::grab_region_normalized(region)?;
    println!("captured frame: {}x{}", frame.width(), frame.height());

    let out_path = asset_dir.join("day_night_debug_capture.png");
    frame.save(&out_path)?;
    println!("saved capture to {}", out_path.display());

    for (label, template) in [("sun", &sun), ("moon", &moon)] {
        let Some(t) = template else {
            println!("{label}: template failed to load");
            continue;
        };
        if frame.width() < t.width() || frame.height() < t.height() {
            println!(
                "{label}: template ({}x{}) is bigger than the region ({}x{}) even before clamping",
                t.width(),
                t.height(),
                frame.width(),
                frame.height()
            );
        }
        let tw = t.width().min(frame.width());
        let th = t.height().min(frame.height());
        let fitted = image::imageops::crop_imm(t, 0, 0, tw, th).to_image();
        let score = vision::match_template_max(&frame, &fitted);
        println!(
            "{label}: template {}x{} (fitted to {}x{}) -> score {score:.3} (threshold {:.3}, {})",
            t.width(),
            t.height(),
            tw,
            th,
            config::MOON_MATCH_THRESHOLD,
            if score >= config::MOON_MATCH_THRESHOLD { "PASS" } else { "fail" }
        );
    }

    Ok(())
}
