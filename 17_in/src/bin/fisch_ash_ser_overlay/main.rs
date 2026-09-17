//! fisch_ash_ser_overlay: a static, always-on-top box showing EXACTLY the
//! screen region fisch_ash_ser.rs captures for ball detection. Run this
//! while actually fishing to see with your own eyes whether that region is
//! really sitting on the ball, instead of inferring it from logs and
//! numbers. Only the ball region is shown -- the bar/sparkle region isn't
//! drawn since the control decision reduces to "hold or release" either
//! way and a second box was just visual noise for this check.
//!
//! This is a fixed absolute screen coordinate (not resolved against any
//! window), so this needs no calibration step -- it just draws the box and
//! gets out of the way. Close with Escape, or just quit the process
//! (Ctrl+C in the terminal).
//!
//! NOTE: true native macOS fullscreen (its own Space, not just a maximized
//! window) can block this overlay from rendering on top even when the
//! window-level trick below reports success -- if the box doesn't appear
//! over Roblox, take Roblox out of fullscreen (Ctrl+Cmd+F, or its own
//! Settings -> Display fullscreen toggle) first.
//!
//! IMPORTANT: keep BALL_REGION here in sync with the identically-named
//! constant in fisch_ash_ser.rs by hand -- this is a standalone diagnostic
//! binary, not a shared module, so there's no compiler check tying the two
//! together.
//!
//! Usage: cargo run --release --bin fisch_ash_ser_overlay

mod platform;

use std::time::Duration;

use eframe::egui;

/// Must match fisch_ash_ser.rs's BALL_REGION exactly.
const BALL_LEFT: i32 = 440;
const BALL_TOP: i32 = 738;
const BALL_WIDTH: u32 = 1000 - 440;
const BALL_HEIGHT: u32 = 24;

fn main() -> eframe::Result<()> {
    println!("fisch_ash_ser_overlay: showing the ball detection region at ({BALL_LEFT}, {BALL_TOP}), {BALL_WIDTH}x{BALL_HEIGHT}");

    // Same countdown pattern as region_overlay/point_overlay -- gives time
    // to switch to Roblox before the always-on-top window actually appears.
    for i in (1..=5).rev() {
        println!("showing in {i}s -- switch to Roblox now...");
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("Close this window (or Ctrl+C the terminal) when done.");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_position([BALL_LEFT as f32, BALL_TOP as f32])
            .with_inner_size([BALL_WIDTH as f32, BALL_HEIGHT as f32])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_title("fisch_ash_ser_overlay"),
        ..Default::default()
    };

    eframe::run_native(
        "fisch_ash_ser_overlay",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            Ok(Box::new(OverlayApp))
        }),
    )
}

struct OverlayApp;

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let red = egui::Color32::from_rgb(255, 0, 40);

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let full = ui.max_rect();
            let painter = ui.painter();

            painter.rect_filled(full, 0.0, red.gamma_multiply(0.15));
            painter.rect_stroke(full, 0.0, egui::Stroke::new(2.0, red), egui::StrokeKind::Inside);
            painter.text(full.left_top() + egui::vec2(2.0, 0.0), egui::Align2::LEFT_TOP, "ball region", egui::FontId::proportional(10.0), red);
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        ctx.request_repaint_after(Duration::from_millis(200));
    }
}
