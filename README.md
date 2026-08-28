# Fish_Bar_Bot

Automated fishing-bar bot for Roblox, written in Rust (macOS only — uses
screen capture + Accessibility/Screen Recording permissions).

## Setup on a fresh machine

1. Install Rust: https://rustup.rs
2. From this folder, build and package the app:
   ```
   ./other\ stuff/package.sh
   ```
   This builds all binaries in release mode and assembles `~/Desktop/FishBot.app`,
   code-signing each helper with a fixed identifier so macOS's Screen Recording
   permission grant survives future rebuilds (see comments at the top of the
   script for why that step exists).
3. Launch `FishBot.app` and grant Screen Recording / Accessibility permissions
   when macOS prompts for them.

Alternatively, to just build and run a single binary without packaging:
```
cd fish_bar_rust
cargo build --release
cargo run --release --bin fisch_new
```

## Layout

- `fish_bar_rust/` — the bot itself (capture, vision, control loop, calibration
  tools, the various `fisch_*`/`fish_*` bin variants)
- `other stuff/` — everything else needed to build and run it: `package.sh`,
  and `mco_spr` (a separate helper Cargo workspace member)
- `*.py` — earlier Python prototypes, kept for reference; the Rust version
  under `fish_bar_rust/` is the active one
