//! control_panel: the day-to-day way to run/test the bot. F9 toggles the
//! whole panel, floats above fullscreen Roblox (see platform.rs). Every
//! setting here (rod placement, rod-reset, skip day/night, totems,
//! re-seat) is edited live and auto-saved to settings.toml — fisch_new
//! reads that file at startup instead of its old interactive stdin
//! prompts, so there's nothing to configure in a terminal anymore.
//!
//! Bot execution and calibration still run as separate helper processes
//! (fisch_new, calibrate_rod, region_overlay, point_overlay) rather than
//! in-process:
//! both rely on input::start_hotkey_listener(), which spawns rdev::listen()
//! on a background thread — safe in their own single-purpose process, but
//! that exact pattern is what crashed with a main-thread SIGTRAP when
//! tried inside an already-running eframe app earlier. Keeping them as
//! separate processes sidesteps that entirely. From your side this is
//! invisible: one app, one panel, no terminal — the process boundary is
//! an implementation detail.

mod platform;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke};
use fish_bar_bot::sampling;
use fish_bar_bot::settings::{DayNight, Settings};
use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

const CRATE_ROOT: &str = env!("CARGO_MANIFEST_DIR");
const ACCENT: Color32 = Color32::from_rgb(90, 169, 255);
/// How long the panel counts down, visibly, before actually spawning the
/// selected script.
const RUN_COUNTDOWN: Duration = Duration::from_secs(5);

/// Which overall bot script to run. Just one entry today ("Simple" ==
/// fisch_new, the existing predictive-controller bot) -- laid out so
/// adding another script later is just one more match arm plus a bubble.
#[derive(Clone, Copy, PartialEq)]
enum Script {
    Simple,
}

impl Script {
    const ALL: [Script; 1] = [Script::Simple];

    fn label(self) -> &'static str {
        match self {
            Script::Simple => "Simple",
        }
    }

    /// Bundled helper binary name this script maps to.
    fn binary_name(self) -> &'static str {
        match self {
            Script::Simple => "fisch_new",
        }
    }
}

/// Defensive cleanup for the case on_exit() can't cover: a force-quit or
/// crash from a PREVIOUS run leaves region_overlay/fisch_new/calibrate_rod
/// orphaned, still holding their always-on-top window open. Kills any
/// running process that's an exact path match for our own bundled
/// helpers — not a name match, so this can't touch anything unrelated.
fn cleanup_stale_helpers() {
    for name in ["region_overlay", "fisch_new", "calibrate_rod", "point_overlay"] {
        let bin = find_helper(name, name == "fisch_new");
        // Anchored at the start and requiring either end-of-line or a
        // space right after — NOT a bare "^{bin}$", since these run with
        // arguments (e.g. "region_overlay --locked"), which a plain "$"
        // anchor would never match. Still can't accidentally hit some
        // unrelated "region_overlay_v2"-type binary, since that space-or-
        // end requirement pins it to this exact path.
        let _ = Command::new("pkill").args(["-f", &format!("^{bin}( |$)")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

fn main() -> eframe::Result<()> {
    cleanup_stale_helpers();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([400.0, 660.0]).with_title("control_panel"),
        ..Default::default()
    };

    eframe::run_native(
        "control_panel",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            apply_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let r = CornerRadius::same(8);
        style.visuals.widgets.noninteractive.corner_radius = r;
        style.visuals.widgets.inactive.corner_radius = r;
        style.visuals.widgets.hovered.corner_radius = r;
        style.visuals.widgets.active.corner_radius = r;
        style.visuals.widgets.open.corner_radius = r;
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.indent = 14.0;
        style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    });
}

/// A titled, bordered, rounded card — the basic visual unit of the whole
/// panel, so every group of controls reads as one distinct thing instead
/// of a flat wall of labels.
fn section(ui: &mut egui::Ui, icon: &str, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let stroke_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, stroke_color))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(14))
        .outer_margin(egui::Margin { bottom: 12, ..Default::default() })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(15.0));
                ui.label(RichText::new(title).strong().size(14.0));
            });
            ui.add_space(6.0);
            add_contents(ui);
        });
}

/// A spawned child, tracked by pid only (Child isn't Clone and we only
/// need to signal it, not read its output here).
struct ChildProc {
    pid: u32,
}

impl ChildProc {
    fn spawn(program: &str, args: &[&str], cwd: &str) -> anyhow::Result<Self> {
        let child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .spawn()?;
        let pid = child.id();
        // We only need to signal this pid, not wait on it from here — let
        // the OS reap it; a background thread just avoids a zombie.
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
        Ok(Self { pid })
    }

    #[cfg(target_os = "macos")]
    fn stop(&self) {
        unsafe { libc::kill(self.pid as i32, libc::SIGTERM) };
    }
}

/// Bundled helper binaries live next to this executable once packaged as
/// a .app; during development (`cargo run`) they're under the workspace's
/// target/ dir instead (this is a workspace, not a standalone crate, so
/// that's one level above CRATE_ROOT). Try the bundle location first,
/// fall back to the dev build location.
fn find_helper(name: &str, prefer_release: bool) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(name);
            if bundled.exists() {
                return bundled.to_string_lossy().into_owned();
            }
        }
    }
    let profile = if prefer_release { "release" } else { "debug" };
    PathBuf::from(CRATE_ROOT)
        .parent()
        .map(|workspace_root| workspace_root.join("target").join(profile).join(name))
        .unwrap_or_else(|| PathBuf::from(CRATE_ROOT).join("target").join(profile).join(name))
        .to_string_lossy()
        .into_owned()
}

struct App {
    _hotkey_manager: Option<GlobalHotKeyManager>,
    toggle_hotkey_id: u32,
    visible: bool,

    settings: Settings,

    region_overlay: Option<ChildProc>,
    region_locked: bool,

    day_night_region_overlay: Option<ChildProc>,
    day_night_region_locked: bool,

    /// 0 or 3 -- one point_overlay child per aquarium point, always
    /// spawned/killed together as one group.
    aquarium_overlays: Vec<ChildProc>,
    aquarium_points_locked: bool,

    selected_script: Script,
    bot: Option<ChildProc>,
    /// Set the instant "Run" is clicked, cleared once RUN_COUNTDOWN
    /// elapses (which is when the script actually gets spawned) or if the
    /// countdown is cancelled.
    run_countdown_start: Option<Instant>,

    recalibrate_status: Arc<Mutex<String>>,
    recalibrating: Arc<AtomicBool>,
}

impl App {
    fn new() -> Self {
        let (hotkey_manager, toggle_hotkey_id) = match GlobalHotKeyManager::new() {
            Ok(manager) => {
                let hotkey = HotKey::new(None, Code::F9);
                match manager.register(hotkey) {
                    Ok(()) => (Some(manager), hotkey.id()),
                    Err(err) => {
                        eprintln!("control_panel: failed to register F9 hotkey: {err:?}");
                        (None, 0)
                    }
                }
            }
            Err(err) => {
                eprintln!("control_panel: failed to create hotkey manager: {err:?}");
                (None, 0)
            }
        };

        Self {
            _hotkey_manager: hotkey_manager,
            toggle_hotkey_id,
            visible: true,
            settings: fish_bar_bot::settings::load(),
            region_overlay: None,
            region_locked: false,
            day_night_region_overlay: None,
            day_night_region_locked: false,
            aquarium_overlays: Vec::new(),
            aquarium_points_locked: false,
            selected_script: Script::Simple,
            bot: None,
            run_countdown_start: None,
            recalibrate_status: Arc::new(Mutex::new(String::new())),
            recalibrating: Arc::new(AtomicBool::new(false)),
        }
    }

    fn respawn_region_overlay(&mut self) {
        if let Some(child) = self.region_overlay.take() {
            child.stop();
        }
        let bin = find_helper("region_overlay", false);
        let args: &[&str] = if self.region_locked { &["--locked"] } else { &[] };
        match ChildProc::spawn(&bin, args, CRATE_ROOT) {
            Ok(child) => self.region_overlay = Some(child),
            Err(e) => eprintln!("control_panel: couldn't start region_overlay: {e}"),
        }
    }

    fn toggle_region_shown(&mut self) {
        if self.region_overlay.is_some() {
            if let Some(child) = self.region_overlay.take() {
                child.stop();
            }
        } else {
            self.respawn_region_overlay();
        }
    }

    fn toggle_region_locked(&mut self) {
        self.region_locked = !self.region_locked;
        if self.region_overlay.is_some() {
            self.respawn_region_overlay();
        }
    }

    fn respawn_day_night_overlay(&mut self) {
        if let Some(child) = self.day_night_region_overlay.take() {
            child.stop();
        }
        let bin = find_helper("region_overlay", false);
        let mut args: Vec<&str> = vec!["--day-night"];
        if self.day_night_region_locked {
            args.push("--locked");
        }
        match ChildProc::spawn(&bin, &args, CRATE_ROOT) {
            Ok(child) => self.day_night_region_overlay = Some(child),
            Err(e) => eprintln!("control_panel: couldn't start region_overlay --day-night: {e}"),
        }
    }

    fn toggle_day_night_shown(&mut self) {
        if self.day_night_region_overlay.is_some() {
            if let Some(child) = self.day_night_region_overlay.take() {
                child.stop();
            }
        } else {
            self.respawn_day_night_overlay();
        }
    }

    fn toggle_day_night_locked(&mut self) {
        self.day_night_region_locked = !self.day_night_region_locked;
        if self.day_night_region_overlay.is_some() {
            self.respawn_day_night_overlay();
        }
    }

    fn respawn_aquarium_overlays(&mut self) {
        for child in self.aquarium_overlays.drain(..) {
            child.stop();
        }
        let bin = find_helper("point_overlay", false);
        for which in ["menu", "item", "close"] {
            let mut args: Vec<&str> = vec!["--which", which];
            if self.aquarium_points_locked {
                args.push("--locked");
            }
            match ChildProc::spawn(&bin, &args, CRATE_ROOT) {
                Ok(child) => self.aquarium_overlays.push(child),
                Err(e) => eprintln!("control_panel: couldn't start point_overlay --which {which}: {e}"),
            }
        }
    }

    fn toggle_aquarium_shown(&mut self) {
        if self.aquarium_overlays.is_empty() {
            self.respawn_aquarium_overlays();
        } else {
            for child in self.aquarium_overlays.drain(..) {
                child.stop();
            }
        }
    }

    fn toggle_aquarium_locked(&mut self) {
        self.aquarium_points_locked = !self.aquarium_points_locked;
        if !self.aquarium_overlays.is_empty() {
            self.respawn_aquarium_overlays();
        }
    }

    fn recalibrate_colors(&mut self) {
        if self.recalibrating.swap(true, Ordering::SeqCst) {
            return; // already running
        }
        let status = self.recalibrate_status.clone();
        let flag = self.recalibrating.clone();
        *status.lock().unwrap() = "recalibrating — switch to Roblox now...".to_string();

        std::thread::spawn(move || {
            let region = fish_bar_bot::calibration::region();
            let result = sampling::sample_region(region).and_then(|(active_hist, active_pixels, idle_hist, idle_pixels)| {
                let ranges = sampling::derive_hsv_ranges(&active_hist, active_pixels, &idle_hist, idle_pixels);
                if ranges.is_empty() {
                    return Ok("no confident ranges found — kept existing colors.".to_string());
                }
                if fish_bar_bot::calibration::load().is_none() {
                    return Ok("no calibration file yet — run Calibrate first.".to_string());
                }
                // Load-then-mutate rather than a fresh struct literal, so
                // the region fields (and any calibrated day/night region
                // or aquarium points) survive this save untouched -- only
                // bar_hsv_ranges changes here.
                let mut cal = fish_bar_bot::calibration::load_raw().unwrap_or_default();
                cal.bar_hsv_ranges = ranges.iter().map(|&((h1, s1, v1), (h2, s2, v2))| (h1, s1, v1, h2, s2, v2)).collect();
                fish_bar_bot::calibration::save(&cal)?;
                Ok(format!("saved {} new HSV range(s).", ranges.len()))
            });

            *status.lock().unwrap() = match result {
                Ok(msg) => msg,
                Err(e) => format!("recalibration failed: {e}"),
            };
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Actually spawns the selected script -- called once RUN_COUNTDOWN
    /// finishes, not directly from the Run button (that just starts the
    /// countdown, see ui() below).
    fn start_run(&mut self) {
        let name = self.selected_script.binary_name();
        let bin = find_helper(name, true);
        match ChildProc::spawn(&bin, &[], CRATE_ROOT) {
            Ok(child) => self.bot = Some(child),
            Err(e) => eprintln!("control_panel: couldn't start {name}: {e}"),
        }
    }

    fn stop_run(&mut self) {
        if let Some(child) = self.bot.take() {
            child.stop();
        }
    }

    fn calibrate(&mut self) {
        let bin = find_helper("calibrate_rod", false);
        if let Err(e) = ChildProc::spawn(&bin, &[], CRATE_ROOT) {
            eprintln!("control_panel: couldn't start calibrate_rod: {e}");
        }
    }

    /// One aligned "label | value" row inside a Grid — used for every
    /// Where/Count field so they all line up instead of each drawing
    /// their own ad-hoc horizontal group.
    fn slot_row(ui: &mut egui::Ui, label: &str, value: &mut u32) -> bool {
        ui.label(label);
        let changed = ui.add(egui::DragValue::new(value).range(1..=9).suffix(" ")).changed();
        ui.end_row();
        changed
    }

    fn count_row(ui: &mut egui::Ui, label: &str, value: &mut u32) -> bool {
        ui.label(label);
        let changed = ui.add(egui::DragValue::new(value).range(1..=999)).changed();
        ui.end_row();
        changed
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == self.toggle_hotkey_id && event.state == HotKeyState::Pressed {
                self.visible = !self.visible;
                if let Some(window) = frame.winit_window() {
                    window.set_visible(self.visible);
                }
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));

        // If the countdown just finished, actually spawn the script now.
        if let Some(start) = self.run_countdown_start {
            if start.elapsed() >= RUN_COUNTDOWN {
                self.run_countdown_start = None;
                self.start_run();
            }
        }

        egui::Panel::top("script_picker").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for script in Script::ALL {
                    let selected = self.selected_script == script;
                    let bubble = egui::Button::new(RichText::new(script.label()).strong())
                        .corner_radius(CornerRadius::same(14))
                        .fill(if selected { ACCENT } else { ui.visuals().faint_bg_color })
                        .stroke(Stroke::new(1.0, if selected { ACCENT } else { ui.visuals().widgets.noninteractive.bg_stroke.color }))
                        .min_size(egui::vec2(0.0, 28.0));
                    if ui.add(bubble).clicked() {
                        self.selected_script = script;
                    }
                }
            });
            ui.add_space(4.0);
        });

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("FishBot").heading().strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("F9 hide/show").weak().size(11.0));
                });
            });
            ui.add_space(8.0);
        });

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(8.0);
            ui.small("F8 pause/resume and Esc quit are the bot's own hotkeys once running.");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let running = self.bot.is_some();
                let counting_down = self.run_countdown_start;

                let (text, fill) = if running {
                    ("⏹  Stop".to_string(), Color32::from_rgb(190, 70, 70))
                } else if let Some(start) = counting_down {
                    let remaining = RUN_COUNTDOWN.saturating_sub(start.elapsed()).as_secs_f32().ceil().max(1.0) as u64;
                    (format!("Starting in {remaining}… (cancel)"), Color32::from_rgb(210, 150, 40))
                } else {
                    ("▶  Run".to_string(), ACCENT)
                };
                let run_button = egui::Button::new(RichText::new(text).strong()).fill(fill).min_size(egui::vec2(120.0, 32.0));
                if ui.add(run_button).clicked() {
                    if running {
                        self.stop_run();
                    } else if counting_down.is_some() {
                        self.run_countdown_start = None; // cancel
                    } else {
                        self.run_countdown_start = Some(Instant::now());
                    }
                }
                if ui.add(egui::Button::new("🎯  Calibrate").min_size(egui::vec2(120.0, 32.0))).clicked() {
                    self.calibrate();
                }
            });
            ui.add_space(8.0);
        });

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut changed = false;

            section(ui, "📍", "Bar region", |ui| {
                ui.horizontal(|ui| {
                    let shown = self.region_overlay.is_some();
                    if ui.button(if shown { "Hide Region" } else { "Show Region" }).clicked() {
                        self.toggle_region_shown();
                    }
                    if ui.button(if self.region_locked { "🔒 Locked" } else { "🔓 Unlocked" }).clicked() {
                        self.toggle_region_locked();
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Unlocked + shown: drag to reposition, Enter on the overlay saves it.").weak().size(11.0));
            });

            section(ui, "🐠", "Aquarium", |ui| {
                ui.horizontal(|ui| {
                    let shown = !self.aquarium_overlays.is_empty();
                    if ui.button(if shown { "Hide Points" } else { "Show Points" }).clicked() {
                        self.toggle_aquarium_shown();
                    }
                    if ui.button(if self.aquarium_points_locked { "🔒 Locked" } else { "🔓 Unlocked" }).clicked() {
                        self.toggle_aquarium_locked();
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("1 = open aquarium menu · 2 = click/scroll anchor · 3 = exit — shown/locked together.").weak().size(11.0));
                ui.add_space(8.0);
                changed |= ui.checkbox(&mut self.settings.aquarium_boost_enabled, "Enabled").changed();
                ui.add_enabled_ui(self.settings.aquarium_boost_enabled, |ui| {
                    ui.add_space(4.0);
                    egui::Grid::new("aquarium_grid").num_columns(2).show(ui, |ui| {
                        changed |= Self::count_row(ui, "Count (every N casts)", &mut self.settings.aquarium_boost_count);
                    });
                });
            });

            section(ui, "🌗", "Day/night region", |ui| {
                ui.horizontal(|ui| {
                    let shown = self.day_night_region_overlay.is_some();
                    if ui.button(if shown { "Hide Region" } else { "Show Region" }).clicked() {
                        self.toggle_day_night_shown();
                    }
                    if ui.button(if self.day_night_region_locked { "🔒 Locked" } else { "🔓 Unlocked" }).clicked() {
                        self.toggle_day_night_locked();
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Where the sun/moon icon check looks — same drag/lock behavior as the region above.").weak().size(11.0));
            });

            section(ui, "🎨", "Colors", |ui| {
                let recalibrating = self.recalibrating.load(Ordering::SeqCst);
                ui.add_enabled_ui(!recalibrating, |ui| {
                    if ui.button("Recalibrate Colors").clicked() {
                        self.recalibrate_colors();
                    }
                });
                let status = self.recalibrate_status.lock().unwrap().clone();
                if !status.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(status).weak().size(11.0));
                }
            });

            section(ui, "🎬", "Cast", |ui| {
                ui.label(RichText::new("The hold-then-Enter-burst sequence every cast starts with.").weak().size(11.0));
                ui.add_space(4.0);
                egui::Grid::new("cast_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Hold duration (ms)");
                    changed |= ui.add(egui::DragValue::new(&mut self.settings.cast_hold_ms).range(0..=10_000)).changed();
                    ui.end_row();

                    ui.label("Enter presses");
                    changed |= ui.add(egui::DragValue::new(&mut self.settings.cast_enter_count).range(0..=100)).changed();
                    ui.end_row();

                    ui.label("Delay between presses (ms)");
                    changed |= ui.add(egui::DragValue::new(&mut self.settings.cast_enter_delay_ms).range(0..=2_000)).changed();
                    ui.end_row();
                });
            });

            section(ui, "🎣", "Rod placement", |ui| {
                egui::Grid::new("rod_placement_grid").num_columns(2).show(ui, |ui| {
                    changed |= Self::slot_row(ui, "Slot", &mut self.settings.rod_slot);
                });
            });

            section(ui, "🔁", "Rod reset", |ui| {
                changed |= ui.checkbox(&mut self.settings.rod_reset_enabled, "Enabled").changed();
                ui.add_enabled_ui(self.settings.rod_reset_enabled, |ui| {
                    ui.add_space(4.0);
                    egui::Grid::new("rod_reset_grid").num_columns(2).show(ui, |ui| {
                        changed |= Self::slot_row(ui, "Where", &mut self.settings.rod_reset_where);
                        changed |= Self::count_row(ui, "Count (every N casts)", &mut self.settings.rod_reset_count);
                    });
                });
            });

            section(ui, "🌗", "Skip day/night", |ui| {
                ui.horizontal(|ui| {
                    let day_on = self.settings.skip_day_night == Some(DayNight::Day);
                    if ui.selectable_label(day_on, "☀ Day").clicked() {
                        self.settings.skip_day_night = if day_on { None } else { Some(DayNight::Day) };
                        changed = true;
                    }
                    let night_on = self.settings.skip_day_night == Some(DayNight::Night);
                    if ui.selectable_label(night_on, "🌙 Night").clicked() {
                        self.settings.skip_day_night = if night_on { None } else { Some(DayNight::Night) };
                        changed = true;
                    }
                });
                ui.add_enabled_ui(self.settings.skip_day_night.is_some(), |ui| {
                    ui.add_space(4.0);
                    egui::Grid::new("day_night_grid").num_columns(2).show(ui, |ui| {
                        changed |= Self::slot_row(ui, "Where", &mut self.settings.skip_day_night_where);
                        changed |= Self::count_row(ui, "Count (check every N casts)", &mut self.settings.skip_day_night_count);
                    });
                });
            });

            section(ui, "🗿", "Use totems", |ui| {
                changed |= ui.checkbox(&mut self.settings.use_totems_enabled, "Enabled").changed();
                ui.add_enabled_ui(self.settings.use_totems_enabled, |ui| {
                    ui.add_space(4.0);
                    egui::Grid::new("totems_grid").num_columns(2).show(ui, |ui| {
                        changed |= Self::slot_row(ui, "Where", &mut self.settings.totems_where);
                        changed |= Self::count_row(ui, "Count (every N casts)", &mut self.settings.totems_count);
                    });
                });
            });

            section(ui, "💺", "Re-seat", |ui| {
                changed |= ui.checkbox(&mut self.settings.reseat_enabled, "Enabled").changed();
            });

            if changed {
                if let Err(e) = fish_bar_bot::settings::save(&self.settings) {
                    eprintln!("control_panel: failed to save settings: {e}");
                }
            }
        });
    }

    /// Without this, closing/quitting the panel orphans anything it
    /// spawned (region_overlay, fisch_new) — they keep running
    /// invisibly, still holding their always-on-top screen-saver-level
    /// window open, which is exactly what made the app "not seem to
    /// open" on relaunch: a leaked window sitting on top of everything,
    /// forever, blocking the new one.
    fn on_exit(&mut self) {
        if let Some(child) = self.region_overlay.take() {
            child.stop();
        }
        if let Some(child) = self.day_night_region_overlay.take() {
            child.stop();
        }
        for child in self.aquarium_overlays.drain(..) {
            child.stop();
        }
        if let Some(child) = self.bot.take() {
            child.stop();
        }
    }
}
