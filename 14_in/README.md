# 17_in — Fish Bar Bot

Rust fishing-bar bot for Roblox (macOS only — uses screen capture +
Accessibility/Screen Recording permissions).

## Setup on a fresh machine

1. Install Rust: https://rustup.rs
2. From this folder:
   ```
   cargo build --release
   ```
3. Run one of the bot binaries (see list below):
   ```
   cargo run --release --bin fish_ctrl_02
   ```
4. Grant Screen Recording / Accessibility permissions to your terminal app
   in System Settings -> Privacy & Security when macOS prompts for them.

## Bot binaries (src/bin)

Main app (default): `fish_bar_bot` (built from `src/main.rs`)

Variants:
- `fish_ctrl_02`
- `fish_Onirifalx`
- `fisch_ash_ser`
- `fisch_new`
- `fish_plague`
- `fish_fab`
- `fish_rainbow`
- `fish_perf_3_simple` / `fish_perf_3_simple_ext`
- `aquarium_boost`

Tools:
- `control_panel`, `calibrate_rod`, `calibrate` — calibration helpers
- `point_overlay`, `region_overlay`, `fisch_ash_ser_overlay` — debug overlays
- `bar_debug`, `day_night_debug`, `find_coords`, `release_space`, `throwaway` — one-off debug utilities

## Notes

`moon.png` and `sun.png` must stay next to `Cargo.toml` (or next to the
built binary) — they're loaded at runtime for day/night detection.
