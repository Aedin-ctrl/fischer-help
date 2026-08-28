//! Persistent, device-independent calibration data.
//!
//! The problem: config.rs's REGION and HSV ranges are tuned against one
//! specific machine's screen. Move to a different device/resolution and
//! the absolute pixel numbers are wrong, even though the Roblox UI is
//! laid out proportionally the same relative to the window.
//!
//! The fix: calibrate_rod (src/bin/calibrate_rod.rs) has you drag a
//! rectangle over the control bar once; that rectangle gets stored here
//! as a FRACTION of the window's bounds at calibration time, plus HSV
//! ranges sampled from inside it. On every future run, resolve_region()
//! re-multiplies those fractions against whatever window size is live
//! that day — no recalibration needed unless the game's own UI layout
//! changes.
//!
//! Saved to ~/Library/Application Support/fish_bar_bot/calibration.toml
//! rather than inside the crate, so it survives being packaged into a
//! .app bundle later (apps shouldn't write into their own bundle).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{HsvRange, Region};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Calibration {
    /// Window size calibration was captured against — kept for
    /// reference/debugging, not used in resolution (fractions already
    /// encode it).
    pub window_width: f64,
    pub window_height: f64,

    pub region_left_frac: f64,
    pub region_top_frac: f64,
    pub region_width_frac: f64,
    pub region_height_frac: f64,

    /// (h_lo, s_lo, v_lo, h_hi, s_hi, v_hi) tuples — flat because toml
    /// doesn't have a native tuple-of-tuples shape.
    pub bar_hsv_ranges: Vec<(u8, u8, u8, u8, u8, u8)>,

    /// Day/night icon-check region, same window-relative-fraction idea as
    /// region_*_frac above. Option because older calibration.toml files
    /// (and fresh ones before this square is ever dragged) won't have
    /// these -- missing Option fields deserialize to None automatically.
    pub day_night_left_frac: Option<f64>,
    pub day_night_top_frac: Option<f64>,
    pub day_night_width_frac: Option<f64>,
    pub day_night_height_frac: Option<f64>,

    /// Aquarium-boost click points (see aquarium.rs), same window-relative
    /// fraction idea, one x/y pair per point. Option for the same reason
    /// as the day/night fields above.
    pub aquarium_menu_x_frac: Option<f64>,
    pub aquarium_menu_y_frac: Option<f64>,
    pub aquarium_item_x_frac: Option<f64>,
    pub aquarium_item_y_frac: Option<f64>,
    pub aquarium_close_x_frac: Option<f64>,
    pub aquarium_close_y_frac: Option<f64>,
}

pub fn calibration_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/fish_bar_bot"))
}

pub fn calibration_path() -> Option<PathBuf> {
    Some(calibration_dir()?.join("calibration.toml"))
}

pub fn load() -> Option<Calibration> {
    let path = calibration_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cal: Calibration = toml::from_str(&text).ok()?;
    // A corrupted or stale-from-before-a-bugfix file (this exact thing
    // happened: window_left/top/width/height captured against the wrong
    // window produced fractions like -1.66 or 1.55, which is nonsense —
    // a region can't start before the window's left edge by more than a
    // sliver, or be 155% of the window's own width) is worse than no
    // file at all, since callers trust Some(cal) unconditionally
    // otherwise. Treat garbage the same as absent.
    cal.is_sane().then_some(cal)
}

pub fn save(cal: &Calibration) -> anyhow::Result<()> {
    let dir = calibration_dir().ok_or_else(|| anyhow::anyhow!("couldn't determine HOME directory"))?;
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(cal)?;
    std::fs::write(dir.join("calibration.toml"), text)?;
    Ok(())
}

impl Calibration {
    /// A region should sit mostly inside its window and be a modest
    /// fraction of it — a fishing bar is nowhere near the size of the
    /// whole game window. Generous bounds on purpose (this only needs to
    /// catch "captured against the wrong window entirely" garbage, not
    /// reject legitimate edge-adjacent placements).
    pub fn is_sane(&self) -> bool {
        let in_range = |v: f64, lo: f64, hi: f64| v.is_finite() && v >= lo && v <= hi;
        in_range(self.region_left_frac, -0.2, 1.0)
            && in_range(self.region_top_frac, -0.2, 1.0)
            && in_range(self.region_width_frac, 0.001, 1.0)
            && in_range(self.region_height_frac, 0.001, 1.0)
    }

    pub fn region_for_window(&self, window_left: f64, window_top: f64, window_width: f64, window_height: f64) -> Region {
        Region {
            left: (window_left + self.region_left_frac * window_width).round() as i32,
            top: (window_top + self.region_top_frac * window_height).round() as i32,
            width: (self.region_width_frac * window_width).round() as u32,
            height: (self.region_height_frac * window_height).round() as u32,
        }
    }

    /// Same idea as region_for_window(), for the day/night check region --
    /// None if any of the 4 fracs hasn't been calibrated yet.
    pub fn day_night_region_for_window(&self, window_left: f64, window_top: f64, window_width: f64, window_height: f64) -> Option<Region> {
        Some(Region {
            left: (window_left + self.day_night_left_frac? * window_width).round() as i32,
            top: (window_top + self.day_night_top_frac? * window_height).round() as i32,
            width: (self.day_night_width_frac? * window_width).round() as u32,
            height: (self.day_night_height_frac? * window_height).round() as u32,
        })
    }

    pub fn hsv_ranges(&self) -> Vec<HsvRange> {
        self.bar_hsv_ranges
            .iter()
            .map(|&(h_lo, s_lo, v_lo, h_hi, s_hi, v_hi)| ((h_lo, s_lo, v_lo), (h_hi, s_hi, v_hi)))
            .collect()
    }

}

/// The region to actually use this run: saved calibration (re-scaled to
/// the CURRENT window bounds) if one exists and the window can be found,
/// else config.rs's hardcoded default. Call this after the game window
/// is focused/visible — same timing requirement as the rest of this
/// bot's window-relative logic (frontmost_app_ok, etc.).
pub fn resolve_region() -> Region {
    if let Some(cal) = load() {
        if let Ok(w) = active_win_pos_rs::get_active_window() {
            return cal.region_for_window(w.position.x, w.position.y, w.position.width, w.position.height);
        }
    }
    crate::config::REGION
}

/// Same idea for HSV ranges: calibrated values if present, else the
/// hand-tuned config.rs defaults.
pub fn resolve_bar_hsv_ranges() -> Vec<HsvRange> {
    load().map(|c| c.hsv_ranges()).filter(|r| !r.is_empty()).unwrap_or_else(|| crate::config::CONTROLLED_BAR_HSV_RANGES.to_vec())
}

/// Calibrated day/night check region (re-scaled to the current window
/// bounds), or None if it hasn't been calibrated yet -- callers fall back
/// to their own hardcoded default in that case (see control.rs).
pub fn resolve_day_night_region() -> Option<Region> {
    let cal = load()?;
    let w = active_win_pos_rs::get_active_window().ok()?;
    cal.day_night_region_for_window(w.position.x, w.position.y, w.position.width, w.position.height)
}

/// Resolves a single calibrated point (e.g. one of aquarium.rs's click
/// points) against the current window bounds. None if either fraction is
/// missing -- callers fall back to their own hardcoded default point.
pub fn resolve_point(x_frac: Option<f64>, y_frac: Option<f64>) -> Option<(f64, f64)> {
    let (xf, yf) = (x_frac?, y_frac?);
    let w = active_win_pos_rs::get_active_window().ok()?;
    Some((w.position.x + xf * w.position.width, w.position.y + yf * w.position.height))
}

static CACHED_REGION: std::sync::OnceLock<Region> = std::sync::OnceLock::new();
static CACHED_BAR_HSV: std::sync::OnceLock<Vec<HsvRange>> = std::sync::OnceLock::new();

/// Same as resolve_region(), but memoized for the process's lifetime —
/// the window isn't expected to move/resize mid-session, and this gets
/// called every tick of the reactive control loop, so re-reading the
/// calibration file and re-querying the OS for window bounds that often
/// would be pure waste. Call resolve_region() directly instead if you
/// specifically need a fresh read (calibrate_rod does, since its whole
/// job is producing a NEW calibration).
pub fn region() -> Region {
    *CACHED_REGION.get_or_init(resolve_region)
}

pub fn bar_hsv_ranges() -> &'static [HsvRange] {
    CACHED_BAR_HSV.get_or_init(resolve_bar_hsv_ranges)
}
