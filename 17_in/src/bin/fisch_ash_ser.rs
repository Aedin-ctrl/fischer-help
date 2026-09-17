//! Autonomous Bar Bot (macOS, terminal) -- same overall shape as
//! fish_Onirifalx.rs (stdin prompts, F8/Esc hotkeys, \r status line), but a
//! from-scratch detection method for this skin's mostly-transparent bar/pin,
//! built by pixel-measuring real screenshots rather than porting
//! fish_Onirifalx.rs's bar+pin HSV-blob approach (which assumed opaque fill
//! colors that don't hold up here).
//!
//! Two solid, non-transparent elements sit above/inside the transparent bar
//! and track it faithfully:
//!
//!   - a small gold BALL floating above the bar, whose x matches the real
//!     pin's x almost exactly (measured: ball and pin land within 1px of
//!     each other in every sampled frame) -- used as the pin's proxy.
//!   - a bright white/lavender SPARKLE at the center of the colored "<"/">"
//!     highlight box inside the bar -- that box is NOT a wall the ball is
//!     confined to (the ball was observed sitting well outside it, still
//!     tracking correctly), it's just a moving marker of where "center"
//!     currently is. The sparkle is that marker's x.
//!
//! Decision, every tick, instantly: ball_x < sparkle_x -> release (key up);
//! ball_x > sparkle_x -> hold (key down). No zones, no smoothing -- this
//! skin's mechanic is a plain instant bang-bang split, not the 7ths/
//! tap-tracking scheme fish_Onirifalx.rs uses for the other skin.
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
use std::time::{Duration, Instant};

use image::RgbaImage;
use rdev::Key;
use xcap::Monitor;

use fish_bar_bot::{aquarium, capture, config, control, input};

// ----------------------------------------------------------------------
// BALL + SPARKLE COLOR DETECTION
// ----------------------------------------------------------------------
//
// Both regions share the same left/width as the bar itself, so a ball-region
// x and a bar-region x are directly comparable without any extra alignment
// math -- they're pixel offsets from the same left edge at the same scale.

/// Whole playable x-range, measured directly on-screen by hand: 440-1000.
/// Matches fish_Onirifalx.rs's own FIXED_REGION almost exactly, so this
/// skin's bar sits in the same physical spot, just re-rendered.
const BAR_REGION: config::Region = config::Region { left: 440, top: 795, width: 1000 - 440, height: 817 - 795 };

/// Capture region for the gold ball -- same left/width as BAR_REGION (the
/// ball can appear anywhere across that span), positioned in the band
/// measured above the bar (ball center consistently landed at y=746-750
/// across 4 independently-measured screenshots). Extra top/bottom margin
/// since the ball has a soft glow halo larger than its solid core.
const BALL_REGION: config::Region = config::Region { left: BAR_REGION.left, top: 738, width: BAR_REGION.width, height: 24 };

/// Single capture spanning both BALL_REGION and BAR_REGION, grabbed once per
/// tick instead of two separate grab_region_normalized() calls -- the two
/// bands then come from the exact same instant, eliminating any possible
/// skew between "where the ball was" and "where the sparkle was" that two
/// sequential (and separately monitor-resolved) captures could otherwise
/// introduce right at the moment they matter most: when ball_x and
/// sparkle_x are close together, near a hold/release crossing.
const COMBINED_REGION: config::Region =
    config::Region { left: BAR_REGION.left, top: BALL_REGION.top, width: BAR_REGION.width, height: (BAR_REGION.top - BALL_REGION.top) as u32 + BAR_REGION.height };
/// Row range of the ball band within COMBINED_REGION.
const BALL_BAND_TOP: u32 = 0;
const BALL_BAND_HEIGHT: u32 = BALL_REGION.height;
/// Row range of the bar/sparkle band within COMBINED_REGION.
const SPARKLE_BAND_TOP: u32 = (BAR_REGION.top - BALL_REGION.top) as u32;
const SPARKLE_BAND_HEIGHT: u32 = BAR_REGION.height;

/// Ball core color measured directly off a real screenshot: solid RGB
/// (213,198,107) -> HSV (26,127,213). Range gives margin for anti-aliasing/
/// compression while staying clear of the near-white sparkle (low
/// saturation) and the violet box border (much higher hue).
const BALL_HSV_RANGE: fish_bar_bot::config::HsvRange = ((10, 70, 140), (45, 190, 255));
/// Calibrated against ~12,500 real ball-blob readings pulled from an actual
/// session's log: the real ball's area clusters tightly at 100-119px and
/// 140-159px, while confirmed false positives (verified against the actual
/// saved snapshot -- gold-colored "1/3" catch-count text) land at area=44,
/// in a separate, much smaller cluster. 70 sits in the gap between them.
const BALL_MIN_BLOB_AREA_PX: u32 = 70;

/// Sparkle core measured directly off a real screenshot: RGB (229,210,252)
/// -> HSV (133,43,252) -- very high brightness, low-to-moderate saturation,
/// regardless of hue. That combination is what separates it from the gold
/// ball/box-border (S~127) and the violet box border (S~160) -- both far
/// more saturated than this near-white flare.
const SPARKLE_MIN_VALUE: u8 = 210;
const SPARKLE_MAX_SATURATION: u8 = 100;
const SPARKLE_MIN_BLOB_AREA_PX: u32 = 15;

/// One connected blob of matching pixels -- carries enough (area, bbox,
/// average HSV) to tell a clean detection apart from a near-tie or a
/// wrong-shaped impostor when logged, not just its x. See TrackLogger below;
/// this is what the council's instrumentation pass asked for: "log the
/// runner-up's area alongside the winner" and "log the winning blob's actual
/// HSV and bounding box, not just x".
struct Blob {
    area: u32,
    center_x: f64,
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
    avg_hsv: (u8, u8, u8),
}

/// All connected blobs of pixels whose HSV satisfies `in_range`, at or above
/// `min_area`, sorted largest-first -- shared by the ball and sparkle
/// finders below, which differ only in the HSV test and which band of the
/// combined capture they scan. Returning every qualifying blob (not just the
/// winner) is what lets the control loop log "how far ahead was the winner"
/// each tick instead of only ever seeing the single chosen x.
fn find_blobs(frame: &RgbaImage, min_area: u32, in_range: impl Fn((u8, u8, u8)) -> bool) -> Vec<Blob> {
    let hsv = fish_bar_bot::vision::frame_to_hsv(frame);
    let (width, height) = (hsv.width, hsv.height);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut visited = vec![false; (width * height) as usize];
    let mut blobs = Vec::new();

    for y0 in 0..height {
        for x0 in 0..width {
            let idx0 = (y0 * width + x0) as usize;
            if visited[idx0] || !in_range(hsv.pixels[idx0]) {
                continue;
            }
            let mut stack = vec![(x0, y0)];
            visited[idx0] = true;
            let (mut area, mut sum_x, mut sum_h, mut sum_s, mut sum_v) = (0u32, 0u64, 0u64, 0u64, 0u64);
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (x0, y0, x0, y0);
            while let Some((x, y)) = stack.pop() {
                let idx = (y * width + x) as usize;
                let (h, s, v) = hsv.pixels[idx];
                area += 1;
                sum_x += x as u64;
                sum_h += h as u64;
                sum_s += s as u64;
                sum_v += v as u64;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx as u32 >= width || ny as u32 >= height {
                            continue;
                        }
                        let nidx = (ny as u32 * width + nx as u32) as usize;
                        if !visited[nidx] && in_range(hsv.pixels[nidx]) {
                            visited[nidx] = true;
                            stack.push((nx as u32, ny as u32));
                        }
                    }
                }
            }
            if area >= min_area {
                blobs.push(Blob {
                    area,
                    center_x: sum_x as f64 / area as f64,
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                    avg_hsv: ((sum_h / area as u64) as u8, (sum_s / area as u64) as u8, (sum_v / area as u64) as u8),
                });
            }
        }
    }

    blobs.sort_unstable_by_key(|b| std::cmp::Reverse(b.area));
    blobs
}

fn ball_in_range((h, s, v): (u8, u8, u8)) -> bool {
    let ((h_lo, s_lo, v_lo), (h_hi, s_hi, v_hi)) = BALL_HSV_RANGE;
    h >= h_lo && h <= h_hi && s >= s_lo && s <= s_hi && v >= v_lo && v <= v_hi
}

fn sparkle_in_range((_, s, v): (u8, u8, u8)) -> bool {
    v >= SPARKLE_MIN_VALUE && s <= SPARKLE_MAX_SATURATION
}

/// Real ball blobs measured at ~11-14px across come out close to square
/// (aspect ratio near 1.0) and mostly filled in (a solid circle fills
/// ~79% of its own bounding box; anti-aliasing softens that a bit, but not
/// much). Text sharing the ball's gold/white color range -- system
/// notifications, chat, "Current Bait: ..." -- fails one or both of these
/// on the same single frame, no history needed: a single glyph's stroke is
/// far sparser than a filled circle (low fill ratio), and a run of several
/// glyphs is much wider than tall (high aspect ratio). This is what
/// actually tells "the ball" apart from "text that happens to be the same
/// color", not the color range alone.
// Calibrated against the same ~12,500-reading log: the bulk of real
// readings cluster at aspect 1.0-1.5 and fill 0.5-0.85 (matching a filled
// circle, ~0.785 theoretical). This is a secondary safety net on top of
// BALL_MIN_BLOB_AREA_PX (the area check above already does the heavy
// lifting -- confirmed text passed a looser fill check at 0.46, which is
// why area, not shape, is the primary filter here).
const BALL_MAX_ASPECT_RATIO: f64 = 2.5;
const BALL_MIN_FILL_RATIO: f64 = 0.45;

fn looks_like_ball(b: &Blob) -> bool {
    let w = (b.max_x - b.min_x + 1) as f64;
    let h = (b.max_y - b.min_y + 1) as f64;
    let aspect = w.max(h) / w.min(h).max(1.0);
    let fill_ratio = b.area as f64 / (w * h);
    aspect <= BALL_MAX_ASPECT_RATIO && fill_ratio >= BALL_MIN_FILL_RATIO
}

/// Finds the gold ball's blobs within the ball band -- the pin's real-time
/// position proxy. Color-matching blobs that don't also look roughly
/// circular (see looks_like_ball) are dropped before ranking, so
/// similarly-colored text can't outrank or masquerade as the real ball.
/// Largest-first; `.first()` is the reading actually used, the rest exist
/// for the diagnostic log. Returns (raw_count, filtered_blobs) -- raw_count
/// is how many blobs matched color alone, BEFORE the shape filter, so the
/// log can show exactly how many candidates the shape filter is rejecting
/// each tick, not just the final surviving count.
fn find_ball_blobs(frame: &RgbaImage) -> (usize, Vec<Blob>) {
    let mut blobs = find_blobs(frame, BALL_MIN_BLOB_AREA_PX, ball_in_range);
    let raw_count = blobs.len();
    blobs.retain(looks_like_ball);
    (raw_count, blobs)
}

/// Finds the white sparkle's blobs within the bar band -- the moving
/// decision threshold (center of the current "<"/">" highlight box).
fn find_sparkle_blobs(frame: &RgbaImage) -> Vec<Blob> {
    find_blobs(frame, SPARKLE_MIN_BLOB_AREA_PX, sparkle_in_range)
}

// ----------------------------------------------------------------------
// STRUCTURED DIAGNOSTIC LOG -- one line per tick to
// ~/Library/Application Support/fish_bar_bot/logs/fisch_ash_ser_track.log,
// covering exactly what the council's instrumentation pass said this file
// couldn't currently tell apart: capture failure vs. zero pixels matched vs.
// a match below the area floor vs. a close call against a runner-up blob.
// Runs on its own thread via a channel so file I/O never blocks the capture/
// detection loop's timing -- logging itself must not become a new source of
// the very timing jitter it exists to rule in or out.
// ----------------------------------------------------------------------

struct TrackLogger {
    tx: std::sync::mpsc::Sender<String>,
}

impl TrackLogger {
    fn new() -> Option<Self> {
        let dir = fish_bar_bot::calibration::calibration_dir()?.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("fisch_ash_ser_track.log");
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let mut writer = io::BufWriter::new(file);
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || {
            for line in rx {
                let _ = writeln!(writer, "{line}");
                // Flushed per line (not batched) -- this is a diagnostic log
                // meant to survive the process being killed mid-session, and
                // it runs on its own thread specifically so this I/O never
                // costs the hot loop anything.
                let _ = writer.flush();
            }
        });
        println!("[track-log] writing to {}", path.display());
        Some(Self { tx })
    }

    fn log(&self, line: String) {
        let _ = self.tx.send(line);
    }
}

// ----------------------------------------------------------------------
// ANNOTATED SNAPSHOTS -- one PNG per tick (not just on first detection),
// with a marker drawn directly on top of the raw capture at wherever the
// ball/sparkle were found: red for the ball, cyan for the sparkle, in
// their own bands. This is deliberately the most direct diagnostic
// available -- instead of reasoning about x/area/HSV numbers, just look at
// the image and see exactly what the algorithm saw and where it thinks the
// two markers are. Runs on its own thread via a channel, same reasoning as
// TrackLogger: saving images must not cost the capture loop anything.
// ----------------------------------------------------------------------

const MARKER_BALL: image::Rgba<u8> = image::Rgba([255, 0, 40, 255]);
const MARKER_SPARKLE: image::Rgba<u8> = image::Rgba([0, 220, 255, 255]);

struct SnapshotSaver {
    tx: std::sync::mpsc::Sender<(String, RgbaImage)>,
}

impl SnapshotSaver {
    fn new() -> Option<Self> {
        let dir = fish_bar_bot::calibration::calibration_dir()?.join("logs").join("fisch_ash_ser_snapshots");
        std::fs::create_dir_all(&dir).ok()?;
        let (tx, rx) = std::sync::mpsc::channel::<(String, RgbaImage)>();
        let save_dir = dir.clone();
        thread::spawn(move || {
            for (name, img) in rx {
                let _ = img.save(save_dir.join(name));
            }
        });
        println!("[snapshots] saving to {}", dir.display());
        Some(Self { tx })
    }

    fn save(&self, name: String, img: RgbaImage) {
        let _ = self.tx.send((name, img));
    }
}

/// Draws a solid 3px-wide vertical marker at `x` spanning rows
/// [y_top, y_top+height) of `img`. Silently does nothing if x is out of
/// bounds -- an out-of-range marker would mean a bug elsewhere, not
/// something worth crashing a diagnostic tool over.
fn draw_marker(img: &mut RgbaImage, x: f64, y_top: u32, height: u32, color: image::Rgba<u8>) {
    if !x.is_finite() || x < 0.0 {
        return;
    }
    let cx = x.round() as i64;
    let (w, h) = img.dimensions();
    for dx in -1i64..=1 {
        let px = cx + dx;
        if px < 0 || px as u32 >= w {
            continue;
        }
        for y in y_top..(y_top + height).min(h) {
            img.put_pixel(px as u32, y, color);
        }
    }
}

/// Clones `frame` and draws both markers on it (whichever are present).
fn annotate_frame(frame: &RgbaImage, ball_x: Option<f64>, sparkle_x: Option<f64>) -> RgbaImage {
    let mut img = frame.clone();
    if let Some(x) = ball_x {
        draw_marker(&mut img, x, BALL_BAND_TOP, BALL_BAND_HEIGHT, MARKER_BALL);
    }
    if let Some(x) = sparkle_x {
        draw_marker(&mut img, x, SPARKLE_BAND_TOP, SPARKLE_BAND_HEIGHT, MARKER_SPARKLE);
    }
    img
}

fn blob_summary(blobs: &[Blob]) -> String {
    match blobs.first() {
        None => "none".to_string(),
        Some(b) => {
            let runner_up = blobs.get(1).map_or("-".to_string(), |r| r.area.to_string());
            format!(
                "x={:.1} area={} bbox=({},{})-({},{}) hsv=({},{},{}) runnerup_area={}",
                b.center_x, b.area, b.min_x, b.min_y, b.max_x, b.max_y, b.avg_hsv.0, b.avg_hsv.1, b.avg_hsv.2, runner_up
            )
        }
    }
}

// ----------------------------------------------------------------------
// TERMINAL "INDICATORS" -- ANSI-colored status line instead of
// fish_Onirifalx.rs's plain text, so this run is visually distinguishable
// from that one at a glance. Adjust the color codes here if you want a
// different look; the underlying status text is the same either way.
// ----------------------------------------------------------------------

const ANSI_GREEN: &str = "\x1b[32m"; // release
const ANSI_RED: &str = "\x1b[31m"; // hold
const ANSI_YELLOW: &str = "\x1b[33m"; // waiting / no detection
const ANSI_RESET: &str = "\x1b[0m";

/// \r alone doesn't erase leftover characters from a longer previous line --
/// clear to end-of-line first so a shorter status string doesn't leave a
/// garbled tail behind.
fn print_status(line: &str) {
    print!("\r\x1b[K{line}");
    let _ = io::stdout().flush();
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DayNightPref {
    Day,
    Night,
}

/// Ask which time of day to skip. 'd'/'day' watches for the sun icon (day)
/// and skips it -- net effect: only night. 'n'/'night' watches for the moon
/// icon (night) instead -- net effect: only day. Anything else (including
/// just Enter) skips nothing -- the day/night check is disabled entirely,
/// regardless of whatever control_panel's "skip day/night" setting says.
fn prompt_day_night_preference() -> Option<DayNightPref> {
    print!("What do you want to skip? Type d/day, n/night, or press Enter to skip nothing: ");
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

/// Hardcoded to 0 -- no Enter presses after casting, overriding
/// settings.cast_enter_count (control_panel's saved value, normally 12)
/// for this run only -- the saved settings.toml value is untouched.
const ENTER_COUNT_OVERRIDE: Option<u32> = Some(0);
const EXTRA_ENTER_COUNT: u32 = 0;

const DAY_NIGHT_CHECK_INTERVAL: Duration = Duration::from_secs(2 * 60);
const AQUARIUM_BOOST_INTERVAL: Duration = Duration::from_secs(31 * 60);
const TOTEM_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn press_pre_trigger_keys() {
    for &(key, label) in config::PRE_TRIGGER_KEYS {
        if input::is_stopped() {
            return;
        }
        println!("  pressing '{label}'...");
        input::tap_key(key, config::PRE_TRIGGER_KEY_HOLD);
        thread::sleep(config::PRE_TRIGGER_KEY_DELAY);
    }
}

/// Ask which hotbar slot the sundial totem is in. Blank disables the
/// feature entirely; typing a number 1-9 enables it at that slot.
fn prompt_totem_preference() -> Option<Key> {
    print!("totems?: press Enter to skip, or type the totem's hotbar slot (1-9): ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().parse::<u32>().ok().and_then(fish_bar_bot::settings::key_for_slot) {
        Some(key) => {
            let mins = TOTEM_REFRESH_INTERVAL.as_secs() / 60;
            println!("okay, refreshing the totem every {mins} minutes");
            Some(key)
        }
        None => {
            println!("alright, no totem refreshing");
            None
        }
    }
}

const TOTEM_KEY_HOLD: Duration = Duration::from_millis(120);
const TOTEM_SWITCH_DELAY: Duration = Duration::from_millis(300);
const TOTEM_CLICK_HOLD: Duration = Duration::from_millis(200);

fn run_totem_action(monitor: &Monitor, totem_key: Key, rod_key: Key) {
    println!("\nRefreshing totem: pressing totem slot, clicking center, switching back to rod slot...");
    input::tap_key(totem_key, TOTEM_KEY_HOLD);
    thread::sleep(TOTEM_SWITCH_DELAY);

    let center_x = monitor.x().unwrap_or(0) + monitor.width().unwrap_or(0) as i32 / 2;
    let center_y = monitor.y().unwrap_or(0) + monitor.height().unwrap_or(0) as i32 / 2;
    input::mouse_move(center_x as f64, center_y as f64);
    input::mouse_down(rdev::Button::Left);
    thread::sleep(TOTEM_CLICK_HOLD);
    input::mouse_up(rdev::Button::Left);
    thread::sleep(TOTEM_SWITCH_DELAY);

    input::tap_key(rod_key, TOTEM_KEY_HOLD);
}

const ICON_CHECK_RETRY_WINDOW: Duration = Duration::from_millis(500);
const ICON_CHECK_RETRY_INTERVAL: Duration = Duration::from_millis(100);

fn icon_visible_with_retries(template: Option<&RgbaImage>) -> bool {
    let start = Instant::now();
    loop {
        if control::is_icon_visible(template) {
            return true;
        }
        if input::is_stopped() || start.elapsed() >= ICON_CHECK_RETRY_WINDOW {
            return false;
        }
        thread::sleep(ICON_CHECK_RETRY_INTERVAL);
    }
}

const NO_BITE_TIMEOUT_STOP_LIMIT: u32 = 3;

/// How long to look for the ball, starting immediately after casting,
/// before giving up on this cast and recasting. Detection itself starts
/// right away -- this is only how long a miss is tolerated before bailing,
/// not a blind delay before checking begins.
const BITE_CHECK_WINDOW: Duration = Duration::from_secs(2);

/// Which side of the sparkle (moving threshold) the ball is currently on.
/// Unlike fish_Onirifalx.rs's 7ths/tap-tracking scheme, this skin's
/// mechanic is a plain instant two-way split -- see the module doc comment.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone {
    Release,
    Hold,
}

/// Actually presses/releases the key for a newly-entered zone. Only called
/// on a real zone change -- never on every tick -- so a sustained Hold
/// doesn't spam repeated key-down events.
fn apply_zone(zone: Zone) {
    match zone {
        Zone::Release => input::key_up(config::HOLD_KEY),
        Zone::Hold => input::key_down(config::HOLD_KEY),
    }
}

fn format_elapsed(d: Duration) -> String {
    let total_secs = d.as_secs();
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn checked_sleep(duration: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < duration {
        if input::is_stopped() {
            return false;
        }
        thread::sleep(Duration::from_millis(20).min(duration.saturating_sub(start.elapsed())));
    }
    true
}

/// Bails out to the caller (return false) on ESC, or pauses/waits out any
/// focus loss (releasing the key first) and returns true once clear to
/// keep going.
fn stay_active() -> bool {
    if input::is_stopped() {
        return false;
    }
    if input::is_paused() || !input::frontmost_app_ok() {
        input::key_up(config::HOLD_KEY);
        return input::wait_while_paused_or_unfocused();
    }
    true
}

const POLL_INTERVAL: Duration = Duration::from_millis(30);
const MISSING_GRACE: Duration = Duration::from_millis(1200);

enum ControlOutcome {
    FightOver,
    Stopped,
}

/// Result of the post-cast bite check -- see wait_for_single_ball().
enum BiteCheckOutcome {
    /// Exactly one ball blob was seen -- a real bite, proceed to tracking.
    Confirmed,
    /// The full BITE_CHECK_WINDOW elapsed with no qualifying single-ball
    /// reading (either nothing matched at all, or only ever multiple
    /// candidate blobs at once -- see find_ball_blobs's doc comment: there
    /// is only ever one real ball, so more than one match is noise, not a
    /// sighting, and does not count toward confirming).
    NotSeen,
    Stopped,
}

/// Polls immediately (no blind delay) for up to BITE_CHECK_WINDOW for
/// exactly one ball blob. A tick with zero or with more than one qualifying
/// blob does not confirm anything -- it just means keep waiting out the
/// window; a real bite is a single, unambiguous ball. Every tick is logged
/// and snapshotted (tag "prebite") regardless of outcome, so a "NotSeen"
/// result can be inspected after the fact just as easily as a confirmed one.
fn wait_for_single_ball(cast_num: u32, logger: Option<&TrackLogger>, snapshots: Option<&SnapshotSaver>) -> BiteCheckOutcome {
    let start = Instant::now();
    let mut tick_num = 0u32;
    while start.elapsed() < BITE_CHECK_WINDOW {
        if !stay_active() {
            return BiteCheckOutcome::Stopped;
        }

        let tick_ms = start.elapsed().as_millis();
        let capture_start = Instant::now();
        let capture_result = capture::grab_region_normalized(BALL_REGION);
        let capture_ms = capture_start.elapsed().as_millis();
        match capture_result {
            Ok(frame) => {
                let detect_start = Instant::now();
                let (raw_count, blobs) = find_ball_blobs(&frame);
                let detect_ms = detect_start.elapsed().as_millis();
                if let Some(l) = logger {
                    l.log(format!(
                        "cast={cast_num} phase=prebite t={tick_ms} ball=[{}] raw_blob_count={raw_count} shape_filtered_count={} TIMING capture_ms={capture_ms} detect_ms={detect_ms}",
                        blob_summary(&blobs),
                        blobs.len()
                    ));
                }
                if let Some(s) = snapshots {
                    let ball_x = blobs.first().map(|b| b.center_x);
                    let marked = annotate_frame(&frame, ball_x, None);
                    s.save(format!("cast{cast_num:03}_prebite_t{tick_ms:05}_n{tick_num:03}.png"), marked);
                }
                if blobs.len() == 1 {
                    return BiteCheckOutcome::Confirmed;
                }
            }
            Err(e) => {
                if let Some(l) = logger {
                    l.log(format!("cast={cast_num} phase=prebite t={tick_ms} capture_error msg={e}"));
                }
            }
        }
        tick_num += 1;

        if !checked_sleep(POLL_INTERVAL) {
            return BiteCheckOutcome::Stopped;
        }
    }

    if let Some(l) = logger {
        l.log(format!("cast={cast_num} phase=prebite EVENT=not_seen after {}ms", start.elapsed().as_millis()));
    }
    BiteCheckOutcome::NotSeen
}

// ----------------------------------------------------------------------
// BALL-VS-SPARKLE CONTROL LOOP -- instant two-way bang-bang, reverted from
// the lock/switch-hysteresis version: real sessions never ran long enough
// per cast (see run_pin_tracking_control's fight-over behavior) for the
// lock to matter, and it wasn't earning its complexity. Every tick: find
// the ball's x (BALL_REGION) and the sparkle's x (BAR_REGION). ball_x <
// sparkle_x -> release; ball_x > sparkle_x -> hold. Switches the instant
// the raw reading changes, no confirm-ticks required.
//
// The comparison naturally extends past the sparkle's highlight box's own
// edges (the ball was observed sitting well outside that box and still
// tracking correctly), so no special-casing for "outside the box" is
// needed -- see the module doc comment for the measurements this is based
// on.
//
// "Fight over": a missing-grace window before declaring the fight actually
// over once ball+sparkle have gone missing together -- the caller only
// enters this function after wait_for_single_ball() already confirmed a
// real bite, so (unlike fish_Onirifalx.rs) there's no separate "is this
// detection trustworthy yet" streak needed in here.
// ----------------------------------------------------------------------

/// Running totals accumulated across one cast's whole tracking loop, so a
/// single summary line can report the cast's overall health instead of
/// requiring one to reconstruct it from every raw tick line.
#[derive(Default)]
struct CastStats {
    tick_count: u32,
    capture_sum_ms: u128,
    detect_sum_ms: u128,
    switch_count: u32,
}

/// Logs one summary line per cast covering the whole run -- tick count,
/// average capture/detect time, and how many times the zone actually
/// switched, retrievable with a single `grep EVENT=cast_summary`.
fn log_cast_summary(logger: Option<&TrackLogger>, cast_num: u32, outcome: &str, stats: &CastStats, duration_ms: u128) {
    let Some(l) = logger else { return };
    let avg_capture = if stats.tick_count > 0 { stats.capture_sum_ms as f64 / stats.tick_count as f64 } else { 0.0 };
    let avg_detect = if stats.tick_count > 0 { stats.detect_sum_ms as f64 / stats.tick_count as f64 } else { 0.0 };
    l.log(format!(
        "cast={cast_num} phase=track EVENT=cast_summary outcome={outcome} ticks={} duration_ms={duration_ms} avg_capture_ms={avg_capture:.1} avg_detect_ms={avg_detect:.1} switch_count={}",
        stats.tick_count, stats.switch_count
    ));
}

fn run_pin_tracking_control(cast_num: u32, logger: Option<&TrackLogger>, snapshots: Option<&SnapshotSaver>) -> ControlOutcome {
    println!("Ball/sparkle tracking control engaged...");

    let mut current_zone: Option<Zone> = None;
    let mut first_tick = true;
    let mut missing_since: Option<Instant> = None;
    let session_start = Instant::now();
    let mut stats = CastStats::default();

    loop {
        if !stay_active() {
            log_cast_summary(logger, cast_num, "stopped", &stats, session_start.elapsed().as_millis());
            return ControlOutcome::Stopped;
        }

        // Unlike run_control_until_gone/run_predictive_control_until_gone in
        // control.rs, this loop's only "done" exit is both ball and sparkle
        // vanishing for MISSING_GRACE -- if a detection glitch makes at least
        // one of the two always read as present, there was previously no
        // escape hatch and the bot would hold the key down forever. Mirror
        // control.rs's CONTROL_TIMEOUT behavior: give up on this cast rather
        // than hang the whole run.
        if session_start.elapsed() >= config::CONTROL_TIMEOUT {
            println!(
                "\n{:.0}s of tracking elapsed without the fight ending -- forcing a restart.",
                config::CONTROL_TIMEOUT.as_secs_f64()
            );
            if let Some(l) = logger {
                l.log(format!("cast={cast_num} phase=track t={} EVENT=control_timeout", session_start.elapsed().as_millis()));
            }
            input::key_up(config::HOLD_KEY);
            log_cast_summary(logger, cast_num, "control_timeout", &stats, session_start.elapsed().as_millis());
            return ControlOutcome::FightOver;
        }

        let tick_ms = session_start.elapsed().as_millis();
        let tick_start = Instant::now();

        // One capture instead of two -- see COMBINED_REGION's doc comment.
        // Errors are logged, not silently discarded: a capture failure
        // (display sleep/wake, permission hiccup, Space switch) must not
        // look identical to "genuinely no matching pixels", which is what
        // `.ok()`-and-move-on was doing before.
        let capture_start = Instant::now();
        let frame = match capture::grab_region_normalized(COMBINED_REGION) {
            Ok(f) => f,
            Err(e) => {
                if let Some(l) = logger {
                    l.log(format!("cast={cast_num} phase=track t={tick_ms} capture_error msg={e}"));
                }
                print_status(&format!("[track] {ANSI_YELLOW}capture error: {e}{ANSI_RESET}"));
                if !checked_sleep(POLL_INTERVAL) {
                    log_cast_summary(logger, cast_num, "stopped", &stats, session_start.elapsed().as_millis());
                    return ControlOutcome::Stopped;
                }
                continue;
            }
        };
        let capture_ms = capture_start.elapsed().as_millis();

        let detect_start = Instant::now();
        let ball_band = image::imageops::crop_imm(&frame, 0, BALL_BAND_TOP, frame.width(), BALL_BAND_HEIGHT).to_image();
        let sparkle_band = image::imageops::crop_imm(&frame, 0, SPARKLE_BAND_TOP, frame.width(), SPARKLE_BAND_HEIGHT).to_image();
        let (ball_raw_count, ball_blobs) = find_ball_blobs(&ball_band);
        let sparkle_blobs = find_sparkle_blobs(&sparkle_band);
        let ball_x = ball_blobs.first().map(|b| b.center_x);
        let sparkle_x = sparkle_blobs.first().map(|b| b.center_x);
        let detect_ms = detect_start.elapsed().as_millis();
        let tick_total_ms = tick_start.elapsed().as_millis();
        stats.tick_count += 1;
        stats.capture_sum_ms += capture_ms;
        stats.detect_sum_ms += detect_ms;

        // Times every tick, unconditionally -- this is what pins down
        // whether a slow tick rate (real per-tick period, not just the
        // POLL_INTERVAL sleep) comes from the capture API itself or from
        // our own blob-finding math, without guessing.
        if let Some(l) = logger {
            l.log(format!("cast={cast_num} phase=track t={tick_ms} TIMING capture_ms={capture_ms} detect_ms={detect_ms} tick_total_ms={tick_total_ms}"));
        }

        // One annotated snapshot per tick (tagged by what this tick turned
        // out to be, below) plus one structured log line -- see the
        // "ANNOTATED SNAPSHOTS" section's doc comment for why every tick,
        // not just notable ones: the point is being able to flip through a
        // whole cast frame by frame after the fact, not guess in advance
        // which tick mattered.
        let tag_for_event = |event: &str| format!("cast{cast_num:03}_track_t{tick_ms:05}_{event}.png");
        if let Some(l) = logger {
            l.log(format!(
                "cast={cast_num} phase=track t={tick_ms} ball=[{}] ball_raw_count={ball_raw_count} sparkle=[{}] zone={current_zone:?}",
                blob_summary(&ball_blobs),
                blob_summary(&sparkle_blobs)
            ));
        }

        match (ball_x, sparkle_x) {
            (Some(ball_x), Some(sparkle_x)) => {
                missing_since = None;
                if first_tick {
                    first_tick = false;
                    println!("\n  [tracking] ball={ball_x:.0} sparkle={sparkle_x:.0}");
                }

                let zone = if ball_x < sparkle_x { Zone::Release } else { Zone::Hold };
                let event = if Some(zone) != current_zone {
                    apply_zone(zone);
                    stats.switch_count += 1;
                    if let Some(l) = logger {
                        l.log(format!("cast={cast_num} phase=track t={tick_ms} EVENT=switch zone={zone:?} ball={ball_x:.1} sparkle={sparkle_x:.1}"));
                    }
                    "switch"
                } else {
                    "agree"
                };
                current_zone = Some(zone);

                if let Some(s) = snapshots {
                    s.save(tag_for_event(event), annotate_frame(&frame, Some(ball_x), Some(sparkle_x)));
                }

                let (color, label) = match zone {
                    Zone::Release => (ANSI_GREEN, "RELEASE"),
                    Zone::Hold => (ANSI_RED, "HOLD   "),
                };
                print_status(&format!("[track] ball={ball_x:.0} sparkle={sparkle_x:.0} zone={color}{label}{ANSI_RESET}"));
            }
            (None, None) => {
                // BOTH missing is the only thing that means the fight is
                // actually over -- see below for why one-missing is handled
                // separately and does NOT count toward this.
                if let Some(s) = snapshots {
                    s.save(tag_for_event("gone"), frame.clone());
                }
                print_status(&format!("[track] ball=none sparkle=none {ANSI_YELLOW}waiting{ANSI_RESET}"));
                match missing_since {
                    None => missing_since = Some(Instant::now()),
                    Some(since) if since.elapsed() >= MISSING_GRACE => {
                        println!("\nBall/sparkle both gone -- fight over.");
                        if let Some(l) = logger {
                            l.log(format!("cast={cast_num} phase=track t={tick_ms} EVENT=fight_over"));
                        }
                        input::key_up(config::HOLD_KEY);
                        log_cast_summary(logger, cast_num, "fight_over", &stats, session_start.elapsed().as_millis());
                        return ControlOutcome::FightOver;
                    }
                    _ => {}
                }
            }
            _ => {
                // Exactly one of the two dropped out for this tick (e.g. the
                // sparkle got harder to match near an edge, or a capture
                // glitch) -- the ball is still clearly in play, so this must
                // NOT reset missing_since or end the fight. Just hold the
                // current lock steady (no zone decision possible without
                // both readings) and keep polling; missing_since only
                // starts once NEITHER is found.
                missing_since = None;
                if let Some(s) = snapshots {
                    s.save(tag_for_event("partial"), annotate_frame(&frame, ball_x, sparkle_x));
                }
                print_status(&format!(
                    "[track] ball={} sparkle={} {ANSI_YELLOW}partial (holding lock){ANSI_RESET}",
                    ball_x.map_or("none".to_string(), |v| format!("{v:.0}")),
                    sparkle_x.map_or("none".to_string(), |v| format!("{v:.0}")),
                ));
            }
        }

        if !checked_sleep(POLL_INTERVAL) {
            log_cast_summary(logger, cast_num, "stopped", &stats, session_start.elapsed().as_millis());
            return ControlOutcome::Stopped;
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Safety net: if anything panics anywhere in the loop below, release the
    // held key before unwinding -- otherwise a crash mid-hold leaves Space
    // stuck down in-game forever.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        input::key_up(Key::Space);
        default_hook(info);
    }));

    let day_night_preference = prompt_day_night_preference();
    let totem_preference = prompt_totem_preference();

    for i in (1..=3).rev() {
        println!("{i}");
        thread::sleep(Duration::from_secs(1));
    }

    println!("Autonomous bar bot running. F8 = pause/resume, ESC = quit.");
    input::start_hotkey_listener();

    let logger = TrackLogger::new();
    if logger.is_none() {
        println!("[track-log] couldn't open the log file -- continuing without per-tick diagnostics.");
    }
    let snapshots = SnapshotSaver::new();
    if snapshots.is_none() {
        println!("[snapshots] couldn't open the snapshots directory -- continuing without per-tick images.");
    }

    press_pre_trigger_keys();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let asset_dir = if exe_dir.join("moon.png").exists() || exe_dir.join("sun.png").exists() {
        exe_dir
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };

    let watch_template = match day_night_preference {
        Some(DayNightPref::Day) => control::load_icon_template(&asset_dir.join("sun.png"), "sun"),
        Some(DayNightPref::Night) => control::load_icon_template(&asset_dir.join("moon.png"), "moon"),
        None => None,
    };

    let monitor = capture::find_primary_monitor()?;
    let mut total_casts: u32 = 0;
    let session_start = Instant::now();
    let mut consecutive_no_bite_timeouts: u32 = 0;
    let mut last_day_night_check = session_start;
    let mut last_aquarium_boost = session_start;
    let mut last_totem_refresh = session_start;

    while !input::is_stopped() {
        if !input::wait_while_paused_or_unfocused() {
            break;
        }

        total_casts += 1;
        println!("\ncast {total_casts} | elapsed {}", format_elapsed(session_start.elapsed()));
        let settings = fish_bar_bot::settings::load();

        if day_night_preference.is_some() && last_day_night_check.elapsed() >= DAY_NIGHT_CHECK_INTERVAL {
            last_day_night_check = Instant::now();
            if icon_visible_with_retries(watch_template.as_ref()) {
                let totem_key = fish_bar_bot::settings::key_for_slot(settings.skip_day_night_where).unwrap_or(Key::Num5);
                let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
                control::run_moon_action(&monitor, totem_key, rod_key);
                press_pre_trigger_keys();
            }
            thread::sleep(config::MOON_CHECK_PRE_CAST_DELAY);
        }

        if settings.aquarium_boost_enabled && last_aquarium_boost.elapsed() >= AQUARIUM_BOOST_INTERVAL {
            last_aquarium_boost = Instant::now();
            println!("\n{total_casts} casts done ({} elapsed) -- running aquarium boost...", format_elapsed(session_start.elapsed()));
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            aquarium::run_aquarium_boost(rod_key);
            press_pre_trigger_keys();
        }

        if let Some(totem_key) = totem_preference
            && last_totem_refresh.elapsed() >= TOTEM_REFRESH_INTERVAL
        {
            last_totem_refresh = Instant::now();
            let rod_key = fish_bar_bot::settings::key_for_slot(settings.rod_slot).unwrap_or(Key::Num1);
            run_totem_action(&monitor, totem_key, rod_key);
            press_pre_trigger_keys();
        }

        control::run_trigger_sequence(true, ENTER_COUNT_OVERRIDE);
        if input::is_stopped() {
            break;
        }

        #[allow(clippy::reversed_empty_ranges)] // EXTRA_ENTER_COUNT is a tunable knob, currently 0
        for _ in 0..EXTRA_ENTER_COUNT {
            if input::is_stopped() {
                break;
            }
            input::tap_key(Key::Return, config::TRIGGER_ENTER_HOLD);
            thread::sleep(Duration::from_millis(settings.cast_enter_delay_ms));
        }

        // Look for exactly one ball starting immediately, for up to
        // BITE_CHECK_WINDOW -- see wait_for_single_ball()'s doc comment.
        // Only once that's confirmed do we actually start reactive
        // tracking; otherwise this cast is treated as a miss and recast,
        // regardless of the cast having already been thrown.
        match wait_for_single_ball(total_casts, logger.as_ref(), snapshots.as_ref()) {
            BiteCheckOutcome::Confirmed => match run_pin_tracking_control(total_casts, logger.as_ref(), snapshots.as_ref()) {
                ControlOutcome::FightOver => {
                    consecutive_no_bite_timeouts = 0;
                }
                ControlOutcome::Stopped => break,
            },
            BiteCheckOutcome::NotSeen => {
                consecutive_no_bite_timeouts += 1;
                println!("\nno ball seen -- no-bite timeout {consecutive_no_bite_timeouts}/{NO_BITE_TIMEOUT_STOP_LIMIT} in a row.");
                if consecutive_no_bite_timeouts >= NO_BITE_TIMEOUT_STOP_LIMIT {
                    println!("{NO_BITE_TIMEOUT_STOP_LIMIT} no-bite timeouts in a row -- stopping.");
                    break;
                }
                press_pre_trigger_keys();
            }
            BiteCheckOutcome::Stopped => break,
        }

        let mut slept = Duration::ZERO;
        while slept < config::POST_CHECK_DELAY && !input::is_stopped() {
            thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }

    input::key_up(Key::Space);
    println!("Stopped. Total casts this run: {total_casts} ({} elapsed)", format_elapsed(session_start.elapsed()));
    Ok(())
}
