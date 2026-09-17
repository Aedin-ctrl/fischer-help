//! point_overlay: a small, persistent, always-on-top circle+crosshair
//! marker showing one of aquarium.rs's calibrated click points. Spawned/
//! killed by control_panel (three at a time, one per point) when you
//! toggle "Show Aquarium Points" -- same drag/lock/auto-save idea as
//! region_overlay, just tracking a single point (this window's center)
//! instead of a whole rectangle.
//!
//! Usage: point_overlay --which menu|item|close [--locked]

mod platform;

use std::time::Duration;

use eframe::egui;
use fish_bar_bot::aquarium;
use fish_bar_bot::calibration::{self, Calibration};
use fish_bar_bot::roblox_window::{self, WindowBounds};

const MARKER_SIZE: f32 = 64.0;

#[derive(Clone, Copy)]
enum Which {
    Menu,
    Item,
    Close,
}

impl Which {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "menu" => Some(Which::Menu),
            "item" => Some(Which::Item),
            "close" => Some(Which::Close),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Which::Menu => "menu",
            Which::Item => "item",
            Which::Close => "close",
        }
    }

    /// Numbered label painted on the marker itself, matching the order
    /// you'd actually use them in: 1 = open the aquarium menu, 2 = where
    /// to click and scroll, 3 = exit/close.
    fn marker_text(self) -> &'static str {
        match self {
            Which::Menu => "1",
            Which::Item => "2",
            Which::Close => "3",
        }
    }

    fn default_point(self) -> (f64, f64) {
        match self {
            Which::Menu => aquarium::DEFAULT_MENU_BUTTON,
            Which::Item => aquarium::DEFAULT_AQUARIUM_ITEM,
            Which::Close => aquarium::DEFAULT_CLOSE_BUTTON,
        }
    }

    fn fracs(self, cal: &Calibration) -> (Option<f64>, Option<f64>) {
        match self {
            Which::Menu => (cal.aquarium_menu_x_frac, cal.aquarium_menu_y_frac),
            Which::Item => (cal.aquarium_item_x_frac, cal.aquarium_item_y_frac),
            Which::Close => (cal.aquarium_close_x_frac, cal.aquarium_close_y_frac),
        }
    }

    fn set_fracs(self, cal: &mut Calibration, x_frac: f64, y_frac: f64) {
        match self {
            Which::Menu => {
                cal.aquarium_menu_x_frac = Some(x_frac);
                cal.aquarium_menu_y_frac = Some(y_frac);
            }
            Which::Item => {
                cal.aquarium_item_x_frac = Some(x_frac);
                cal.aquarium_item_y_frac = Some(y_frac);
            }
            Which::Close => {
                cal.aquarium_close_x_frac = Some(x_frac);
                cal.aquarium_close_y_frac = Some(y_frac);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let locked = args.iter().any(|a| a == "--locked");
    let which = args
        .iter()
        .position(|a| a == "--which")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| Which::parse(s))
        .ok_or_else(|| anyhow::anyhow!("usage: point_overlay --which menu|item|close [--locked]"))?;

    // Give a few seconds to switch to Roblox before the always-on-top
    // marker actually appears -- launched from a terminal, there's no time
    // to bring Roblox to the front otherwise before it's covering whatever
    // window happened to be focused when this started.
    for i in (1..=5).rev() {
        println!("point_overlay starting in {i}s -- switch to Roblox now...");
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Same reasoning as region_overlay: find Roblox by window title, not
    // focus -- this is launched from a control_panel button click, so
    // control_panel (not Roblox) is frontmost at that instant.
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
            println!("point_overlay: couldn't find a Roblox window — falling back to screen bounds.");
            monitor_bounds
        }
    };

    // load_raw(), not load() -- an uncalibrated/never-sane bar region
    // shouldn't hide an otherwise-valid aquarium point.
    let cal = calibration::load_raw();
    let calibrated_fracs = cal.as_ref().and_then(|c| {
        let (xf, yf) = which.fracs(c);
        Some((xf?, yf?))
    });
    let (center_x, center_y, calibrated) = match calibrated_fracs {
        Some((xf, yf)) => {
            let (x, y) = Calibration::point_for_window(xf, yf, window.x, window.y, window.width, window.height);
            (x, y, true)
        }
        None => {
            let (x, y) = which.default_point();
            (x, y, false)
        }
    };
    let left = (center_x - MARKER_SIZE as f64 / 2.0).round() as i32;
    let top = (center_y - MARKER_SIZE as f64 / 2.0).round() as i32;
    println!(
        "point_overlay [{}/{}]: showing at center ({center_x:.0}, {center_y:.0}){}",
        which.marker_text(),
        which.label(),
        if calibrated { " — from saved calibration" } else { " — first-time default" }
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_position([left as f32, top as f32])
            .with_inner_size([MARKER_SIZE, MARKER_SIZE])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_title("point_overlay"),
        ..Default::default()
    };

    eframe::run_native(
        "point_overlay",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            Ok(Box::new(OverlayApp {
                rect: (left, top, MARKER_SIZE as u32, MARKER_SIZE as u32),
                last_saved_rect: Some((left, top, MARKER_SIZE as u32, MARKER_SIZE as u32)),
                locked,
                which,
                window,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("overlay window failed: {e}"))?;

    Ok(())
}

struct OverlayApp {
    rect: (i32, i32, u32, u32),
    last_saved_rect: Option<(i32, i32, u32, u32)>,
    locked: bool,
    which: Which,
    window: WindowBounds,
}

impl OverlayApp {
    fn save_position(&self) {
        let (left, top, width, height) = self.rect;
        let center_x = left as f64 + width as f64 / 2.0;
        let center_y = top as f64 + height as f64 / 2.0;
        let (xf, yf) = Calibration::point_to_frac(self.window.x, self.window.y, self.window.width, self.window.height, center_x, center_y);

        // Load-then-mutate rather than a fresh struct literal, so the bar
        // region, day/night region, and the other two aquarium points
        // survive this save untouched.
        let mut cal = calibration::load_raw().unwrap_or_default();
        self.which.set_fracs(&mut cal, xf, yf);
        if let Err(e) = calibration::save(&cal) {
            eprintln!("point_overlay: failed to save position: {e}");
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
        let color = if locked { egui::Color32::from_rgb(0, 225, 255) } else { egui::Color32::from_rgb(255, 0, 200) };

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let full = ui.max_rect();
            let center = full.center();
            let radius = full.width().min(full.height()) / 2.0 - 3.0;

            ui.painter().circle_filled(center, radius, color.gamma_multiply(0.28));
            ui.painter().circle_stroke(center, radius, egui::Stroke::new(3.0, color));
            let arm = radius * 0.55;
            ui.painter().line_segment([center - egui::vec2(arm, arm), center + egui::vec2(arm, arm)], egui::Stroke::new(2.0, color));
            ui.painter().line_segment([center + egui::vec2(-arm, arm), center + egui::vec2(arm, -arm)], egui::Stroke::new(2.0, color));

            // Sits in the free space of the top half -- halfway between the
            // circle's rim and the crosshair's center, so it doesn't touch
            // either. Half the size of the earlier centered version, with a
            // black outline (offset copies behind the white fill) so it
            // stays legible over any game background.
            let number_center = center - egui::vec2(0.0, radius * 0.5);
            let font = egui::FontId::proportional(radius * 0.65);
            let text = self.which.marker_text();
            for dx in [-1.0, 0.0, 1.0] {
                for dy in [-1.0, 0.0, 1.0] {
                    if dx != 0.0 || dy != 0.0 {
                        ui.painter().text(number_center + egui::vec2(dx, dy), egui::Align2::CENTER_CENTER, text, font.clone(), egui::Color32::BLACK);
                    }
                }
            }
            ui.painter().text(number_center, egui::Align2::CENTER_CENTER, text, font, egui::Color32::WHITE);

            if !locked {
                let resp = ui.interact(full, ui.id().with("move"), egui::Sense::drag());
                if resp.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }
        });

        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.rect = (rect.left() as i32, rect.top() as i32, rect.width() as u32, rect.height() as u32);
        }

        if !locked && self.last_saved_rect != Some(self.rect) {
            self.save_position();
            self.last_saved_rect = Some(self.rect);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let interval = if locked { Duration::from_millis(500) } else { Duration::from_millis(16) };
        ctx.request_repaint_after(interval);
    }
}
