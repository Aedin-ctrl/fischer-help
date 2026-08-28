//! mco_spr — visual block editor / control panel for the fish bar bot.
//!
//! Drag a source file in; each top-level function in it becomes a
//! reorderable rounded block (see parser.rs). Primitive action blocks
//! (Press Key / Click / Wait / Repeat) can also be added from the
//! sidebar's palette and composed from scratch — see blocks.rs.
//!
//! Sidebar (☰, top-left): a palette of block types to add, above a list
//! of blocks currently placed — click a Function block's name (there or
//! on its card) to open its real source in the right-side code drawer.
//!
//! Top-right: `+` opens the same palette as a popup menu; to its left,
//! Run/Pause-Resume/Stop drives execution (runner.rs):
//!   - If the sequence is ALL primitive action blocks, Run actually
//!     interprets them in the order you arranged the cards — press/
//!     click/wait for real, Repeat loops back to the start N times (0 =
//!     forever). This is real block-order-drives-execution.
//!   - If any Function block is present, Run instead executes the whole
//!     loaded program as a subprocess, as-is — there's no safe way to
//!     dynamically invoke an arbitrary extracted function, so a mixed or
//!     function-only sequence can't be order-driven yet. That needs
//!     control.rs's loop factored into independently-callable steps,
//!     which is a real follow-up, not something to fake here.
//!
//! Toggle window visibility: F9, from anywhere (see platform.rs for why
//! this uses global-hotkey instead of rdev's listen()).

mod blocks;
mod parser;
mod platform;
mod runner;

use std::path::{Path, PathBuf};

use eframe::egui::{self, CornerRadius, RichText};
use egui_dnd::Dnd;
use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use blocks::Block;
use runner::Runner;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 680.0])
            .with_title("mco_spr"),
        ..Default::default()
    };

    eframe::run_native(
        "mco_spr",
        options,
        Box::new(move |cc| {
            platform::make_float_above_fullscreen(cc);
            apply_rounded_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

fn apply_rounded_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let r = CornerRadius::same(10);
        style.visuals.window_corner_radius = CornerRadius::same(14);
        style.visuals.menu_corner_radius = r;
        style.visuals.widgets.noninteractive.corner_radius = r;
        style.visuals.widgets.inactive.corner_radius = r;
        style.visuals.widgets.hovered.corner_radius = r;
        style.visuals.widgets.active.corner_radius = r;
        style.visuals.widgets.open.corner_radius = r;
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
    });
}

struct App {
    _hotkey_manager: Option<GlobalHotKeyManager>,
    toggle_hotkey_id: u32,
    visible: bool,

    blocks: Vec<Block>,
    loaded_path: Option<PathBuf>,
    sidebar_open: bool,
    selected_block: Option<usize>,
    error_message: Option<String>,

    runner: Runner,
}

impl App {
    fn new() -> Self {
        let (hotkey_manager, toggle_hotkey_id) = match GlobalHotKeyManager::new() {
            Ok(manager) => {
                let hotkey = HotKey::new(None, Code::F9);
                match manager.register(hotkey) {
                    Ok(()) => (Some(manager), hotkey.id()),
                    Err(err) => {
                        eprintln!("mco_spr: failed to register F9 hotkey: {err:?}");
                        (None, 0)
                    }
                }
            }
            Err(err) => {
                eprintln!("mco_spr: failed to create hotkey manager: {err:?}");
                (None, 0)
            }
        };

        Self {
            _hotkey_manager: hotkey_manager,
            toggle_hotkey_id,
            visible: true,
            blocks: Vec::new(),
            loaded_path: None,
            sidebar_open: false,
            selected_block: None,
            error_message: None,
            runner: Runner::default(),
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let Some(file) = dropped.first() else { return };
        let Some(path) = &file.path else {
            self.error_message = Some("dropped item has no filesystem path".to_string());
            return;
        };
        self.load_file(path.clone());
    }

    /// Native file picker — the reliable alternative to drag-and-drop.
    fn open_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .add_filter("Rust / Python", &["rs", "py"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.load_file(path);
        }
    }

    fn load_file(&mut self, path: std::path::PathBuf) {
        match parser::parse_file(&path) {
            Ok(new_blocks) => {
                self.blocks = new_blocks;
                self.loaded_path = Some(path);
                self.selected_block = None;
                self.error_message = None;
            }
            Err(e) => self.error_message = Some(e),
        }
    }

    fn add_block(&mut self, block: Block) {
        self.blocks.push(block);
        self.selected_block = Some(self.blocks.len() - 1);
    }

    /// The four primitive block types, rendered as buttons — used by both
    /// the sidebar palette and the top-right `+` popup menu.
    fn palette_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button("＋ Press Key").clicked() {
            self.add_block(Block::new_press_key());
        }
        if ui.button("＋ Click").clicked() {
            self.add_block(Block::new_click());
        }
        if ui.button("＋ Wait").clicked() {
            self.add_block(Block::new_wait());
        }
        if ui.button("＋ Repeat").clicked() {
            self.add_block(Block::new_repeat());
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(RichText::new("☰").size(18.0)).clicked() {
                self.sidebar_open = !self.sidebar_open;
            }
            if ui.button("📂 Open File…").clicked() {
                self.open_file_dialog();
            }
            ui.label(RichText::new("mco_spr").strong());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("+").size(18.0), |ui| {
                    self.palette_buttons(ui);
                });

                self.runner.poll();
                if self.runner.is_running() {
                    let label = if self.runner.is_paused() { "▶ Resume" } else { "⏸ Pause" };
                    if ui.button(label).clicked() {
                        if self.runner.is_paused() {
                            self.runner.resume();
                        } else {
                            self.runner.pause();
                        }
                    }
                    if ui.button("⏹ Stop").clicked() {
                        self.runner.stop();
                    }
                } else if ui.button("▶ Run").clicked() {
                    self.try_run();
                }
            });
        });
    }

    /// All-primitive sequences run for real, in card order, via the
    /// interpreter; anything with a Function block falls back to running
    /// the loaded program as a subprocess. See the module doc for why.
    fn try_run(&mut self) {
        if self.blocks.is_empty() {
            self.error_message = Some("add or load some blocks first".to_string());
            return;
        }

        let all_primitive = self.blocks.iter().all(Block::is_runnable_primitive);
        let result = if all_primitive {
            self.runner.start_actions(self.blocks.clone())
        } else {
            let Some(path) = &self.loaded_path else {
                self.error_message = Some(
                    "this sequence has Function blocks but no loaded file to run — drag one in".to_string(),
                );
                return;
            };
            let Some((program, args, cwd)) = resolve_run_target(path) else {
                self.error_message = Some("couldn't figure out how to run this file".to_string());
                return;
            };
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.runner.start_process(&program, &arg_refs, &cwd)
        };

        if let Err(e) = result {
            self.error_message = Some(e);
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 2.0 - 80.0);
            ui.label(RichText::new("Drag a .rs or .py file in, or:").size(16.0).weak());
            if ui.button(RichText::new("📂 Open File…").size(14.0)).clicked() {
                self.open_file_dialog();
            }
            ui.label(RichText::new("its top-level functions become blocks").size(12.0).weak());
            ui.add_space(12.0);
            ui.label(RichText::new("— or —").size(12.0).weak());
            ui.add_space(12.0);
            ui.label(RichText::new("add a Press Key / Click / Wait / Repeat block from ＋").size(12.0).weak());
        });
    }

    fn block_list(&mut self, ui: &mut egui::Ui) {
        let mut clicked: Option<usize> = None;

        let response = Dnd::new(ui, "block_list").show_vec(&mut self.blocks, |ui, block, handle, state| {
            handle.ui(ui, |ui| {
                if blocks::card_ui(ui, block) {
                    clicked = Some(state.index);
                }
            });
        });

        if let Some(update) = response.update {
            egui_dnd::utils::shift_vec(update.from, update.to, &mut self.blocks);
        }
        if clicked.is_some() {
            self.selected_block = clicked;
        }
    }

    fn code_drawer(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected_block else { return };
        let Some(Block::Function { name, source, .. }) = self.blocks.get_mut(idx) else {
            // Primitive blocks are edited inline on their card; nothing
            // to show in the drawer for those.
            return;
        };

        let mut close_requested = false;
        egui::Panel::right("code_drawer")
            .resizable(true)
            .default_size(360.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name.as_str()).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").clicked() {
                            close_requested = true;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(source)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(30),
                    );
                });
            });

        if close_requested {
            self.selected_block = None;
        }
    }

    fn output_pane(&self, ui: &mut egui::Ui) {
        let output = self.runner.output.lock().unwrap();
        if output.is_empty() {
            return;
        }
        ui.separator();
        ui.label(RichText::new("output").weak().size(11.0));
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in output.iter() {
                    ui.label(RichText::new(line).monospace().size(11.0));
                }
            });
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
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));

        self.handle_dropped_files(ui.ctx());

        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });

        if self.sidebar_open {
            egui::Panel::left("sidebar").default_size(200.0).show(ui, |ui| {
                ui.label(RichText::new("Add a block").strong());
                ui.separator();
                self.palette_buttons(ui);

                ui.add_space(16.0);
                ui.label(RichText::new("On the canvas").strong());
                ui.separator();
                for i in 0..self.blocks.len() {
                    let title = self.blocks[i].title();
                    if ui.selectable_label(self.selected_block == Some(i), title).clicked() {
                        self.selected_block = Some(i);
                    }
                }
            });
        }

        self.code_drawer(ui);

        if let Some(err) = &self.error_message {
            ui.colored_label(ui.visuals().error_fg_color, err);
            ui.separator();
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.blocks.is_empty() {
                self.empty_state(ui);
            } else {
                self.block_list(ui);
            }
        });

        self.output_pane(ui);
    }
}

/// Figures out how to actually run a dropped file: for a Rust file inside
/// a cargo crate, `cargo run --bin <file-stem>` from the crate root; for a
/// .py file, `python3 <path>` from its own directory.
fn resolve_run_target(path: &Path) -> Option<(String, Vec<String>, PathBuf)> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => {
            if let Some(crate_root) = path.ancestors().find(|dir| dir.join("Cargo.toml").exists()) {
                let bin_name = path.file_stem()?.to_str()?.to_string();
                return Some((
                    "cargo".to_string(),
                    vec!["run".to_string(), "--bin".to_string(), bin_name],
                    crate_root.to_path_buf(),
                ));
            }
            // Not part of a cargo crate — compile it standalone with
            // rustc and run the result. Only works for files with no
            // external crate dependencies (std only), since there's no
            // Cargo.toml here to resolve anything else against.
            let bin_name = path.file_stem()?.to_str()?.to_string();
            let tmp_bin = std::env::temp_dir().join(format!("mco_spr_{bin_name}"));
            let cmd = format!(
                "rustc {src} -o {bin} && {bin}",
                src = shell_quote(path.to_str()?),
                bin = shell_quote(tmp_bin.to_str()?),
            );
            let cwd = path.parent()?.to_path_buf();
            Some(("sh".to_string(), vec!["-c".to_string(), cmd], cwd))
        }
        Some("py") => {
            let cwd = path.parent()?.to_path_buf();
            Some(("python3".to_string(), vec![path.to_str()?.to_string()], cwd))
        }
        _ => None,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
