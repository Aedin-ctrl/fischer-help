//! Persisted bot behavior settings — what control_panel's UI edits, and
//! what fisch_new reads at startup instead of its old interactive stdin
//! prompts. Saved to ~/Library/Application Support/fish_bar_bot/settings.toml
//! (same directory as calibration.toml, different file: this is behavior/
//! feature config, calibration.toml is vision/position tuning).
//!
//! "Slot" fields are Roblox hotbar numbers (1-9) — unlike REGION/HSV
//! calibration, these don't vary by device/resolution, so they're plain
//! user-entered numbers, not anything calibrate_rod needs to figure out.

use std::path::PathBuf;

use rdev::Key;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayNight {
    Day,
    Night,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    /// Hotbar slot the rod itself sits in. Was hardcoded Key::Num1
    /// everywhere "switch to the rod" happened.
    pub rod_slot: u32,

    pub rod_reset_enabled: bool,
    /// Hotbar slot to switch to (then back to rod_slot) as the reset.
    /// Was hardcoded Key::Num9.
    pub rod_reset_where: u32,
    /// Do this every N casts. 1 = every cast.
    pub rod_reset_count: u32,

    /// None = don't skip either. Some(Day) = watch for day, skip it
    /// (net effect: only night). Some(Night) = watch for night, skip it.
    pub skip_day_night: Option<DayNight>,
    /// Hotbar slot of the totem used to change time of day. Was
    /// hardcoded config::MOON_ACTION_KEY (Num5).
    pub skip_day_night_where: u32,
    /// Check every N casts. Was config::SESSIONS_BETWEEN_MOON_CHECKS.
    pub skip_day_night_count: u32,

    pub use_totems_enabled: bool,
    /// Hotbar slot of the (non-day/night) totem. Was hardcoded Key::Num4.
    pub totems_where: u32,
    /// Refresh every N casts. Was SESSIONS_BETWEEN_TOTEM_USE (15).
    pub totems_count: u32,

    pub reseat_enabled: bool,

    /// Runs aquarium.rs's click/scroll boost sequence periodically.
    #[serde(default)]
    pub aquarium_boost_enabled: bool,
    /// Every N casts. Was fish_perf_3_simple_ext's hardcoded
    /// CASTS_BETWEEN_AQUARIUM_BOOSTS.
    #[serde(default = "default_aquarium_boost_count")]
    pub aquarium_boost_count: u32,

    /// How long the cast's hold-click lasts. Was config::TRIGGER_HOLD
    /// (1000ms).
    #[serde(default = "default_cast_hold_ms")]
    pub cast_hold_ms: u64,
    /// How many times Enter gets pressed after the hold-click. Was
    /// config::TRIGGER_ENTER_COUNT (12).
    #[serde(default = "default_cast_enter_count")]
    pub cast_enter_count: u32,
    /// Delay between each Enter press. Was config::TRIGGER_ENTER_INTERVAL
    /// (100ms).
    #[serde(default = "default_cast_enter_delay_ms")]
    pub cast_enter_delay_ms: u64,
}

// PRE-EXISTING BUG FIX (found while verifying this session's changes, not
// caused by them): these 3 fields were added to Settings at some point
// without #[serde(default)], but real settings.toml files written before
// that point don't have these keys -- toml::from_str failed to parse them
// (missing required field), and load()'s .ok().unwrap_or_default() then
// silently discarded EVERY OTHER field too, resetting the whole struct.
// Defaults here match the values already in `impl Default for Settings`
// below, so old files now parse with those same values instead of
// silently losing rod_slot/skip_day_night/etc.
fn default_cast_hold_ms() -> u64 {
    1000
}

fn default_cast_enter_count() -> u32 {
    12
}

fn default_cast_enter_delay_ms() -> u64 {
    100
}

fn default_aquarium_boost_count() -> u32 {
    35
}

impl Default for Settings {
    fn default() -> Self {
        // Matches the old hardcoded fisch_new.rs/config.rs values exactly,
        // so a fresh install behaves the same as before this existed.
        Self {
            rod_slot: 1,
            rod_reset_enabled: false,
            rod_reset_where: 9,
            rod_reset_count: 1,
            skip_day_night: None,
            skip_day_night_where: 5,
            skip_day_night_count: 5,
            use_totems_enabled: false,
            totems_where: 4,
            totems_count: 15,
            reseat_enabled: false,
            aquarium_boost_enabled: false,
            aquarium_boost_count: 35,
            cast_hold_ms: 1000,
            cast_enter_count: 12,
            cast_enter_delay_ms: 100,
        }
    }
}

pub fn settings_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/fish_bar_bot"))
}

pub fn settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join("settings.toml"))
}

pub fn load() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> anyhow::Result<()> {
    let dir = settings_dir().ok_or_else(|| anyhow::anyhow!("couldn't determine HOME directory"))?;
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(settings)?;
    std::fs::write(dir.join("settings.toml"), text)?;
    Ok(())
}

/// Roblox hotbar slots are 1-9 — maps straight onto rdev's numeric keys.
pub fn key_for_slot(slot: u32) -> Option<Key> {
    match slot {
        1 => Some(Key::Num1),
        2 => Some(Key::Num2),
        3 => Some(Key::Num3),
        4 => Some(Key::Num4),
        5 => Some(Key::Num5),
        6 => Some(Key::Num6),
        7 => Some(Key::Num7),
        8 => Some(Key::Num8),
        9 => Some(Key::Num9),
        _ => None,
    }
}
