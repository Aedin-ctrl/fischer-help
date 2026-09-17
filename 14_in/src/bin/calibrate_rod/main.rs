//! calibrate_rod: drag a rectangle over the control bar once, and this
//! samples real on-screen colors from inside it while you fish, saving
//! everything to calibration.rs's persisted file. The saved region is a
//! FRACTION of the Roblox window's bounds, so future runs — on this
//! device or any other — re-resolve it against whatever window size is
//! live that day. See fish_bar_bot::calibration for the file format, and
//! fish_bar_bot::sampling for the actual cast-and-sample logic (shared
//! with the control panel's "Recalibrate Colors" action).
//!
//! Overlay controls: drag anywhere to move, drag a corner to resize,
//! Enter to confirm and start sampling, Escape to cancel.

mod platform;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;
use fish_bar_bot::config::Region;
use fish_bar_bot::roblox_window::{self, WindowBounds};
use fish_bar_bot::{calibration, config, sampling};

fn countdown(seconds: u64, message: &str) {
    println!("{message}");
    for i in (1..=seconds).rev() {
        println!("{i}...");
        thread::sleep(Duration::from_secs(1));
    }
    println!("Go!\n");
}

fn suggested_rect(window: &WindowBounds) -> (i32, i32, u32, u32) {
    if let Some(cal) = calibration::load() {
        let r = cal.region_for_window(window.x, window.y, window.width, window.height);
        return (r.left, r.top, r.width.max(40), r.height.max(20));
    }
    let r = config::REGION;
    (r.left, r.top, r.width, r.height)
}

fn main() -> anyhow::Result<()> {
    // The countdown here is still worth keeping even though window lookup
    // no longer depends on focus (find_roblox_window() finds it by title
    // regardless) — it gives you a moment to actually be ready, since
    // sampling casts for real a few lines down.
    countdown(6, "calibrate_rod: switch to Roblox now (about to start casting to sample colors).");

    let window = roblox_window::find_roblox_window().ok_or_else(|| anyhow::anyhow!("couldn't find a Roblox window — is it running?"))?;
    println!("window: Roblox at ({:.0}, {:.0}), {:.0}x{:.0}", window.x, window.y, window.width, window.height);

    let (left, top, width, height) = suggested_rect(&window);
    let confirmed: Arc<Mutex<Option<(i32, i32, u32, u32)>>> = Arc::new(Mutex::new(None));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_position([left as f32, top as f32])
            .with_inner_size([width as f32, height as f32])
            .with_min_inner_size([40.0, 20.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_title("calibrate_rod"),
        ..Default::default()
    };

    let app_confirmed = confirmed.clone();
    eframe::run_native(
        "calibrate_rod",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            Ok(Box::new(OverlayApp { rect: (left, top, width, height), confirmed: app_confirmed }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("overlay window failed: {e}"))?;

    let Some((left, top, width, height)) = *confirmed.lock().unwrap() else {
        println!("cancelled — nothing saved.");
        return Ok(());
    };
    let region = Region { left, top, width, height };
    println!("\nconfirmed region: ({left}, {top}, {width}x{height})\n");

    let (active_hist, active_pixels, idle_hist, idle_pixels) = sampling::sample_region(region)?;
    println!("\nsampled {active_pixels} active px, {idle_pixels} idle px.\n");

    let bar_hsv_ranges = sampling::derive_hsv_ranges(&active_hist, active_pixels, &idle_hist, idle_pixels);
    if bar_hsv_ranges.is_empty() {
        println!("couldn't derive confident color ranges from this session — keeping the existing config.rs defaults.");
    } else {
        println!("derived {} HSV range(s):", bar_hsv_ranges.len());
        for (lo, hi) in &bar_hsv_ranges {
            println!("  H{}-{} S{}-{} V{}-{}", lo.0, hi.0, lo.1, hi.1, lo.2, hi.2);
        }
    }

    let (rl, rt, rw, rh) = calibration::Calibration::region_to_fracs(window.x, window.y, window.width, window.height, region);
    // Load-then-mutate rather than building a fresh struct literal, so any
    // already-calibrated day/night region or aquarium points survive this
    // save untouched -- only the region/window/color fields below change.
    let mut cal = calibration::load_raw().unwrap_or_default();
    cal.window_width = window.width;
    cal.window_height = window.height;
    cal.region_left_frac = rl;
    cal.region_top_frac = rt;
    cal.region_width_frac = rw;
    cal.region_height_frac = rh;
    cal.bar_hsv_ranges = bar_hsv_ranges.iter().map(|&((h1, s1, v1), (h2, s2, v2))| (h1, s1, v1, h2, s2, v2)).collect();
    calibration::save(&cal)?;
    println!("saved to {}", calibration::calibration_path().unwrap().display());

    Ok(())
}

struct OverlayApp {
    rect: (i32, i32, u32, u32),
    confirmed: Arc<Mutex<Option<(i32, i32, u32, u32)>>>,
}

impl OverlayApp {
    const HANDLE: f32 = 16.0;
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let full = ui.max_rect();

            ui.painter().rect_filled(full, 0.0, egui::Color32::from_rgba_unmultiplied(255, 200, 0, 40));
            ui.painter().rect_stroke(full, 0.0, egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 200, 0)), egui::StrokeKind::Inside);
            ui.painter().text(
                full.center_top() + egui::vec2(0.0, 6.0),
                egui::Align2::CENTER_TOP,
                format!("{}x{}  ·  drag to move · corners resize · Enter confirm · Esc cancel", full.width() as i32, full.height() as i32),
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );

            let handles = [
                (egui::Rect::from_min_size(full.left_top(), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::NorthWest),
                (egui::Rect::from_min_size(full.right_top() - egui::vec2(Self::HANDLE, 0.0), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::NorthEast),
                (egui::Rect::from_min_size(full.left_bottom() - egui::vec2(0.0, Self::HANDLE), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::SouthWest),
                (egui::Rect::from_min_size(full.right_bottom() - egui::vec2(Self::HANDLE, Self::HANDLE), egui::vec2(Self::HANDLE, Self::HANDLE)), egui::ResizeDirection::SouthEast),
            ];
            for (rect, _) in &handles {
                ui.painter().rect_filled(*rect, 3.0, egui::Color32::from_rgb(255, 200, 0));
            }

            let mut on_handle = false;
            for (rect, dir) in handles {
                let resp = ui.interact(rect, ui.id().with(("handle", format!("{dir:?}"))), egui::Sense::drag());
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
        });

        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.rect = (rect.left() as i32, rect.top() as i32, rect.width() as u32, rect.height() as u32);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            *self.confirmed.lock().unwrap() = Some(self.rect);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        ctx.request_repaint_after(Duration::from_millis(16));
    }
}
