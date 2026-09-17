//! Read-only diagnostic: never presses any key. Every 300ms, captures the
//! control-bar region, classifies each of the 5 sample points (white /
//! bar-present / neither) and prints their raw HSV, and saves the frame to
//! bar_debug_capture.png for visual inspection. For debugging the "stuck
//! holding Space" daytime issue -- run this while it's happening (or while
//! casting normally) and see whether all 5 points read white even with no
//! real bar on screen.
//!
//! Ctrl+C to stop (no hotkey listener -- this never touches input at all).

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use image::RgbaImage;

use fish_bar_bot::config::HsvRange;
use fish_bar_bot::{capture, config};

const SAMPLE_POINTS_X: [i32; 5] = [994, 784, 721, 660, 445];
const SAMPLE_Y: i32 = 805;
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
    h /= 2.0;
    (h.round() as u8, s.round() as u8, v.round() as u8)
}

fn in_range(hsv: (u8, u8, u8), range: &HsvRange) -> bool {
    let (low, high) = *range;
    hsv.0 >= low.0 && hsv.0 <= high.0 && hsv.1 >= low.1 && hsv.1 <= high.1 && hsv.2 >= low.2 && hsv.2 <= high.2
}

/// Median HSV of the 3x3 neighborhood (median, not mean, so a stray outlier
/// pixel can't skew the reported value) plus whether it matches each range.
fn sample_point(frame: &RgbaImage, cx: i32, cy: i32) -> ((u8, u8, u8), bool, bool) {
    let mut hs = Vec::new();
    let mut ss = Vec::new();
    let mut vs = Vec::new();
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x as u32 >= frame.width() || y as u32 >= frame.height() {
                continue;
            }
            let p = frame.get_pixel(x as u32, y as u32);
            let (h, s, v) = rgb_to_hsv(p[0], p[1], p[2]);
            hs.push(h);
            ss.push(s);
            vs.push(v);
        }
    }
    hs.sort_unstable();
    ss.sort_unstable();
    vs.sort_unstable();
    let mid = hs.len() / 2;
    let hsv = (hs[mid], ss[mid], vs[mid]);
    (hsv, in_range(hsv, &WHITE_RANGE), BAR_RANGES.iter().any(|r| in_range(hsv, r)))
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
    let capture_path = asset_dir.join("bar_debug_capture.png");

    println!("Read-only -- never presses any key. Region: {:?}", config::REGION);
    println!("Saving each frame to {}. Ctrl+C to stop.", capture_path.display());
    println!("Switch to Roblox now -- starting in:");
    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    loop {
        match capture::grab_region_normalized(config::REGION) {
            Ok(frame) => {
                if let Err(e) = frame.save(&capture_path) {
                    println!("(couldn't save capture: {e})");
                }
                let mut white_count = 0;
                let mut bar_count = 0;
                let mut parts = Vec::new();
                for &x_abs in &SAMPLE_POINTS_X {
                    let cx = x_abs - config::REGION.left;
                    let cy = SAMPLE_Y - config::REGION.top;
                    let (hsv, is_white, is_bar) = sample_point(&frame, cx, cy);
                    if is_white {
                        white_count += 1;
                    }
                    if is_bar {
                        bar_count += 1;
                    }
                    parts.push(format!(
                        "x={x_abs} hsv={hsv:?}{}{}",
                        if is_white { " WHITE" } else { "" },
                        if is_bar && !is_white { " bar" } else { "" }
                    ));
                }
                println!("white={white_count}/5 bar={bar_count}/5   {}", parts.join("  |  "));
            }
            Err(e) => println!("capture failed: {e}"),
        }
        thread::sleep(Duration::from_millis(300));
    }
}
