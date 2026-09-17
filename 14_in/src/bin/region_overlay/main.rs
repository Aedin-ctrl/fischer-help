//! region_overlay: a persistent, always-on-top rectangle showing the
//! currently calibrated region. Spawned/killed by control_panel as a
//! subprocess when you toggle "Show Region" — this process only shows
//! and (if unlocked) lets you reposition; it doesn't do any sampling
//! itself (see calibrate_rod for the full calibrate-from-scratch flow).
//!
//! Position is auto-saved continuously while unlocked and being dragged
//! — not just on an explicit Enter press — because control_panel's Lock
//! and Hide buttons kill this process outright (SIGTERM, no chance to
//! react), so anything that only saved on a deliberate in-window action
//! silently lost your placement the moment you used those buttons
//! instead of dragging-then-pressing-Enter yourself.
//!
//! Starting position: uses the saved calibration if one exists and is
//! valid; only falls back to screen-center when there's genuinely
//! nothing saved yet (first-ever run, or a stale/invalid file).
//!
//! Unlocked: drag to move, drag a corner to resize. Locked: static
//! display only, no drag/resize interaction. Enter or Escape both just
//! close it — there's no separate "cancel without saving" anymore, since
//! saving isn't a discrete action to opt out of.
//!
//! Usage: region_overlay [--locked] [--day-night]
//!
//! --day-night switches this from the bar-tracking region to the
//! day/night icon-check region -- same interaction, different calibration
//! fields (day_night_*_frac instead of region_*_frac) and a different
//! hardcoded fallback (control::default_day_night_region() instead of
//! config::REGION).

mod platform;

use std::time::Duration;

use eframe::egui;
use fish_bar_bot::calibration;
use fish_bar_bot::config::Region;
use fish_bar_bot::roblox_window::{self, WindowBounds};

#[derive(Clone, Copy, PartialEq)]
enum OverlayMode {
    Region,
    DayNight,
}

fn main() -> anyhow::Result<()> {
    let locked = std::env::args().any(|a| a == "--locked");
    let mode = if std::env::args().any(|a| a == "--day-night") { OverlayMode::DayNight } else { OverlayMode::Region };
    let label = match mode {
        OverlayMode::Region => "Bar region",
        OverlayMode::DayNight => "Day/Night region",
    };

    // Give a few seconds to switch to Roblox before the always-on-top
    // rectangle actually appears -- same reasoning as point_overlay.
    for i in (1..=5).rev() {
        println!("region_overlay starting in {i}s -- switch to Roblox now...");
        std::thread::sleep(Duration::from_secs(1));
    }

    // Finds Roblox by window title, not "whatever's currently focused" —
    // this is launched by clicking a button in control_panel, which is
    // itself the frontmost window at that instant, so trusting focus
    // here grabbed control_panel's bounds instead of Roblox's. Falls
    // back to the primary monitor's own bounds if Roblox can't be found
    // at all, so this always shows something rather than silently
    // failing to appear.
    let monitor = fish_bar_bot::capture::find_primary_monitor()?;
    let monitor_bounds = WindowBounds {
        x: monitor.x().unwrap_or(0) as f64,
        y: monitor.y().unwrap_or(0) as f64,
        width: monitor.width().unwrap_or(1440) as f64,
        height: monitor.height().unwrap_or(900) as f64,
    };
    let window = match roblox_window::find_roblox_window() {
        Some(w) => w,
        None => {
            println!("region_overlay: couldn't find a Roblox window — falling back to screen bounds.");
            monitor_bounds
        }
    };

    // Use the saved position/size if there's a valid calibration for this
    // mode; only fall back to this mode's hardcoded default rect when
    // there's genuinely nothing saved yet. Both arms resolve against the
    // `window` found above (by title, via find_roblox_window()) -- NOT
    // calibration::resolve_day_night_region(), which looks up the
    // currently-FOCUSED window instead. This process is launched by a
    // control_panel button click, so control_panel (not Roblox) is
    // focused at that instant; using the focus-based resolver here
    // computed the day/night rect relative to control_panel's own small
    // window, producing a wrong, seemingly-random position on every
    // respawn (i.e. every Lock/Unlock and every Show/Hide).
    let calibrated = match mode {
        OverlayMode::Region => calibration::load().map(|cal| cal.region_for_window(window.x, window.y, window.width, window.height)),
        OverlayMode::DayNight => calibration::load().and_then(|cal| cal.day_night_region_for_window(window.x, window.y, window.width, window.height)),
    };
    let (left, top, width, height) = match calibrated {
        Some(r) => (r.left, r.top, r.width.max(40), r.height.max(20)),
        None => {
            let default_region = match mode {
                OverlayMode::Region => fish_bar_bot::config::REGION,
                OverlayMode::DayNight => fish_bar_bot::control::default_day_night_region(),
            };
            let (w, h) = (default_region.width, default_region.height);
            let l = monitor_bounds.x as i32 + (monitor_bounds.width as i32 - w as i32) / 2;
            let t = monitor_bounds.y as i32 + (monitor_bounds.height as i32 - h as i32) / 2;
            (l, t, w, h)
        }
    };
    println!(
        "region_overlay [{label}]: showing at ({left}, {top}), {width}x{height}{}",
        if calibrated.is_some() { " — from saved calibration" } else { " — first-time default, screen-centered" }
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_position([left as f32, top as f32])
            .with_inner_size([width as f32, height as f32])
            .with_min_inner_size([40.0, 20.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(!locked)
            .with_title("region_overlay"),
        ..Default::default()
    };

    eframe::run_native(
        "region_overlay",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            Ok(Box::new(OverlayApp {
                rect: (left, top, width, height),
                // Whatever we're starting at is already what's on disk
                // (or, for a fresh center default, isn't worth saving
                // until you've actually moved it) — either way, no
                // reason to write anything back until it changes.
                last_saved_rect: Some((left, top, width, height)),
                locked,
                mode,
                window,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("overlay window failed: {e}"))?;

    Ok(())
}

struct OverlayApp {
    rect: (i32, i32, u32, u32),
    /// What's currently on disk, so we only write when something actually
    /// changed instead of every single frame.
    last_saved_rect: Option<(i32, i32, u32, u32)>,
    locked: bool,
    mode: OverlayMode,
    window: WindowBounds,
}

impl OverlayApp {
    /// Visual size of the corner handle — small and unobtrusive so it
    /// doesn't get in the way while micro-adjusting the rectangle over a
    /// thin bar. The clickable/draggable area is a bit larger than what's
    /// drawn (HIT_PAD), so it's still easy to actually grab despite being
    /// visually thin.
    const HANDLE: f32 = 7.0;
    const HANDLE_HIT_PAD: f32 = 5.0;

    fn save_position(&self) {
        let (left, top, width, height) = self.rect;
        let region = Region { left, top, width, height };
        let (rl, rt, rw, rh) =
            calibration::Calibration::region_to_fracs(self.window.x, self.window.y, self.window.width, self.window.height, region);

        // Load-then-mutate rather than building a fresh struct literal, so
        // whichever mode we're NOT updating right now (bar region vs
        // day/night region, plus any calibrated aquarium points) survives
        // this save untouched.
        let mut cal = calibration::load_raw().unwrap_or_default();
        match self.mode {
            OverlayMode::Region => {
                cal.window_width = self.window.width;
                cal.window_height = self.window.height;
                cal.region_left_frac = rl;
                cal.region_top_frac = rt;
                cal.region_width_frac = rw;
                cal.region_height_frac = rh;
            }
            OverlayMode::DayNight => {
                cal.day_night_left_frac = Some(rl);
                cal.day_night_top_frac = Some(rt);
                cal.day_night_width_frac = Some(rw);
                cal.day_night_height_frac = Some(rh);
            }
        }
        if let Err(e) = calibration::save(&cal) {
            eprintln!("region_overlay: failed to save position: {e}");
        }
    }
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let locked = self.locked;
        let mode_label = match self.mode {
            OverlayMode::Region => "Bar region",
            OverlayMode::DayNight => "Day/Night region",
        };
        // Bright, highly saturated, and distinct from typical game UI
        // colors on purpose — this needs to be unmistakable at a glance,
        // not just technically present.
        let color = if locked { egui::Color32::from_rgb(0, 225, 255) } else { egui::Color32::from_rgb(255, 0, 200) };

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let full = ui.max_rect();
            // A visible fill so it reads as "there" instantly, plus a
            // solid, thick, bright border for the exact boundary.
            ui.painter().rect_filled(full, 0.0, color.gamma_multiply(0.28));
            ui.painter().rect_stroke(full, 0.0, egui::Stroke::new(3.0, color), egui::StrokeKind::Inside);

            let label = if locked {
                format!("{mode_label} · locked")
            } else {
                format!("{mode_label} · {}x{}  ·  drag to move · corners resize", full.width() as i32, full.height() as i32)
            };
            ui.painter().text(full.center_top() + egui::vec2(0.0, 4.0), egui::Align2::CENTER_TOP, label, egui::FontId::proportional(11.0), color);

            if !locked {
                let handles = [
                    (egui::Rect::from_min_size(full.left_top(), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::NorthWest),
                    (egui::Rect::from_min_size(full.right_top() - egui::vec2(Self::HANDLE, 0.0), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::NorthEast),
                    (egui::Rect::from_min_size(full.left_bottom() - egui::vec2(0.0, Self::HANDLE), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::SouthWest),
                    (egui::Rect::from_min_size(full.right_bottom() - egui::vec2(Self::HANDLE, Self::HANDLE), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::SouthEast),
                ];
                for (rect, _) in &handles {
                    ui.painter().rect_filled(*rect, 1.0, color);
                }

                let mut on_handle = false;
                for (rect, dir) in handles {
                    let hit_rect = rect.expand(Self::HANDLE_HIT_PAD);
                    let resp = ui.interact(hit_rect, ui.id().with(("handle", format!("{dir:?}"))), egui::Sense::drag());
                    if resp.hovered() {
                        on_handle = true;
                    }
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                    }
                }
                if !on_handle {
                    let resp = ui.interact(full, ui.id().with("move"), egui::Sense::drag());
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                }
            }
        });

        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.rect = (rect.left() as i32, rect.top() as i32, rect.width() as u32, rect.height() as u32);
        }

        // Auto-save whenever the rect actually changed — covers being
        // killed at any moment (Hide/Lock from control_panel), not just
        // an explicit confirm action.
        if !locked && self.last_saved_rect != Some(self.rect) {
            self.save_position();
            self.last_saved_rect = Some(self.rect);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Locked = static display, nothing on screen ever changes, so
        // there's nothing to gain from redrawing 60x/sec. Unlocked
        // (actively draggable/resizable) still wants a responsive
        // repaint rate.
        let interval = if locked { Duration::from_millis(500) } else { Duration::from_millis(16) };
        ctx.request_repaint_after(interval);
    }
}
