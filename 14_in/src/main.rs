//! Autonomous Bar Bot (macOS, terminal) — Rust port of fish_bar_new.py.
//! Behavior, timings, and config values are kept 1:1 with the Python
//! original; see that file's module docstring for the full loop spec.
//!
//! macOS: grant Screen Recording + Accessibility to your terminal app in
//! System Settings -> Privacy & Security, then restart the terminal.
//!
//! CONTROLS
//! --------
//! F8  = pause/resume the whole loop (safe to hit anytime)
//! ESC = quit immediately (always releases the mouse/key first)

use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use rdev::Key;

use fish_bar_bot::{capture, config, control, input};

#[derive(Clone, Copy, PartialEq)]
enum DayNightPref {
    Day,
    Night,
}

/// Ask which time of day to skip. 'd'/'day' watches for the sun icon (day)
/// and skips it -- net effect: only night. 'n'/'night' watches for the moon
/// icon (night) instead -- net effect: only day. Anything else (including
/// just Enter) disables the check entirely.
fn prompt_day_night_preference() -> Option<DayNightPref> {
    print!("What do you want to skip? Type d/day to skip day, n/night to skip night, or press Enter to skip nothing: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().to_lowercase().as_str() {
        "d" | "day" => {
            println!("okay, skipping day whenever it's detected");
            Some(DayNightPref::Day)
        }
        "n" | "night" => {
            println!("okay, skipping night whenever it's detected");
            Some(DayNightPref::Night)
        }
        _ => {
            println!("alright, skipping nothing — day/night check disabled");
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    let day_night_preference = prompt_day_night_preference();

    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    println!("Autonomous bar bot running. F8 = pause/resume, ESC = quit.");
    input::start_hotkey_listener();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    // Fall back to the source-tree location so `cargo run` (whose exe lives
    // under target/debug) still finds moon.png/sun.png next to main.rs's
    // project root during development.
    let asset_dir = if exe_dir.join("moon.png").exists() || exe_dir.join("sun.png").exists() {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };

    // Only load the one icon template we actually need to watch for -- if
    // skipping nothing, never load either, so the periodic check below
    // never has anything to trigger on.
    let watch_template = match day_night_preference {
        Some(DayNightPref::Day) => control::load_icon_template(&asset_dir.join("sun.png"), "sun"),
        Some(DayNightPref::Night) => control::load_icon_template(&asset_dir.join("moon.png"), "moon"),
        None => None,
    };

    let monitor = capture::find_primary_monitor()?;
    let mut session_count: u32 = 0;

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        if day_night_preference.is_some() && session_count >= config::SESSIONS_BETWEEN_MOON_CHECKS {
            session_count = 0;
            if control::is_icon_visible(watch_template.as_ref()) {
                let settings = fish_bar_bot::settings::load();
                let totem_key = fish_bar_bot::settings::key_for_slot(settings.skip_day_night_where).unwrap_or(Key::Num5);
                let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
                control::run_moon_action(&monitor, totem_key, rod_key);
            }
            // Full delay before the cast either way.
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        control::run_trigger_sequence(false, None);
        if input::is_stopped() {
            break;
        }

        control::run_control_until_gone();
        session_count += 1;

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(Key::Space);
    println!("Stopped.");
    Ok(())
}
