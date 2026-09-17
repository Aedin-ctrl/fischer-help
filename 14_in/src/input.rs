//! Keyboard/mouse simulation, the F8/ESC global hotkey listener, and the
//! cached frontmost-app check — port of the pynput-based bottom half of
//! fish_bar_new.py. rdev's macOS backend sends real hardware keycodes for
//! Key::Num9/Num1/Num5/Return (25/18/23/36) already, the same fix the
//! Python script needed explicit vk codes for — plain char-based key
//! events can silently fall back to a unicode-text insert on macOS, which
//! games reading raw keyCodes just ignore.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rdev::{Button, EventType, Key};

use crate::config::TARGET_APP_NAME;

pub static PAUSED: AtomicBool = AtomicBool::new(false);
pub static STOPPED: AtomicBool = AtomicBool::new(false);

pub fn start_hotkey_listener() {
    thread::spawn(|| {
        let callback = |event: rdev::Event| {
            if let EventType::KeyPress(key) = event.event_type {
                match key {
                    Key::F8 => {
                        let now_paused = !PAUSED.load(Ordering::SeqCst);
                        PAUSED.store(now_paused, Ordering::SeqCst);
                        println!("\n{}", if now_paused { "PAUSED" } else { "RESUMED" });
                    }
                    Key::Escape => {
                        STOPPED.store(true, Ordering::SeqCst);
                        println!("\nStopping...");
                    }
                    _ => {}
                }
            }
        };
        if let Err(e) = rdev::listen(callback) {
            eprintln!("hotkey listener failed to start: {e:?} (check Accessibility permission)");
        }
    });
}

pub fn is_stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

pub fn is_paused() -> bool {
    PAUSED.load(Ordering::SeqCst)
}

pub fn key_down(key: Key) {
    let _ = rdev::simulate(&EventType::KeyPress(key));
}

pub fn key_up(key: Key) {
    let _ = rdev::simulate(&EventType::KeyRelease(key));
}

pub fn tap_key(key: Key, hold: Duration) {
    key_down(key);
    thread::sleep(hold);
    key_up(key);
}

pub fn mouse_move(x: f64, y: f64) {
    let _ = rdev::simulate(&EventType::MouseMove { x, y });
}

pub fn mouse_down(button: Button) {
    let _ = rdev::simulate(&EventType::ButtonPress(button));
}

pub fn mouse_up(button: Button) {
    let _ = rdev::simulate(&EventType::ButtonRelease(button));
}

/// Moves to (x, y) and does a plain left click there.
pub fn click(x: f64, y: f64) {
    mouse_move(x, y);
    mouse_down(Button::Left);
    thread::sleep(Duration::from_millis(30));
    mouse_up(Button::Left);
}

/// Simulates a scroll-wheel tick. Positive `delta_x` scrolls right,
/// negative scrolls left (macOS CGEvent convention -- flip the sign if a
/// caller finds it backwards on their machine's scroll-direction setting).
pub fn scroll(delta_x: i64, delta_y: i64) {
    let _ = rdev::simulate(&EventType::Wheel { delta_x, delta_y });
}

/// Cached — the real frontmost-window query only actually runs at most
/// every FOCUS_CHECK_INTERVAL; focus doesn't change fast enough to need
/// checking every loop iteration, and that round trip was the source of
/// periodic stalls in the hot control loop (same reasoning as the Python
/// original's frontmost_app_ok()).
pub struct FocusCache {
    ok: bool,
    checked_at: Instant,
}

impl FocusCache {
    pub fn new() -> Self {
        FocusCache { ok: true, checked_at: Instant::now() - crate::config::FOCUS_CHECK_INTERVAL }
    }

    pub fn check(&mut self, interval: Duration) -> bool {
        if self.checked_at.elapsed() < interval {
            return self.ok;
        }
        self.ok = match active_win_pos_rs::get_active_window() {
            Ok(w) => w.app_name.to_lowercase().contains(&TARGET_APP_NAME.to_lowercase()),
            Err(_) => true, // if the check itself fails, don't block on it
        };
        self.checked_at = Instant::now();
        self.ok
    }
}

pub static FOCUS: Mutex<Option<FocusCache>> = Mutex::new(None);

pub fn frontmost_app_ok() -> bool {
    let mut guard = FOCUS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(FocusCache::new());
    }
    guard.as_mut().unwrap().check(crate::config::FOCUS_CHECK_INTERVAL)
}

/// Blocks (checking for stop) while paused or the target app isn't
/// frontmost. Returns false if we should abort entirely (ESC hit).
pub fn wait_while_paused_or_unfocused() -> bool {
    while !is_stopped() && (is_paused() || !frontmost_app_ok()) {
        thread::sleep(Duration::from_millis(50));
    }
    !is_stopped()
}
