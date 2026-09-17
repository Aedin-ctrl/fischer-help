//! Standalone test harness for the "aquarium boost" click/scroll sequence
//! (fish_bar_bot::aquarium::run_aquarium_boost), so it can be tried in
//! isolation. Now wired into fish_perf_3_simple_ext's every-50-casts hook
//! too -- keep this around for quick manual re-tests.
//!
//! ESC quits (mid-run too, via the shared hotkey listener).

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use rdev::Key;

use fish_bar_bot::{aquarium, input, settings};

fn main() {
    input::start_hotkey_listener();

    let rod_key = settings::key_for_slot(settings::load().rod_slot).unwrap_or(Key::Num1);

    print!("aquarium boost?: y/yes, enter/skip: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();

    match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => {
            for i in (1..=3).rev() {
                println!("{i}");
                thread::sleep(Duration::from_secs(1));
            }
            aquarium::run_aquarium_boost(rod_key);
        }
        _ => println!("skipped."),
    }
}
