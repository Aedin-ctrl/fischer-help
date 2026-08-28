//! A block is either a parsed top-level function (see parser.rs) or a
//! primitive action (press a key, click, wait, repeat). Primitive blocks
//! have known, real semantics, so runner.rs can actually execute a
//! sequence of them in whatever order you arrange the cards — unlike
//! Function blocks, which stay view/edit-only (see main.rs's module doc).

use eframe::egui;

#[derive(Clone, Debug, Hash)]
pub enum Block {
    Function {
        name: String,
        doc: String,
        source: String,
    },
    PressKey {
        key: String,
        hold_ms: u64,
    },
    Click {
        hold_ms: u64,
    },
    Wait {
        ms: u64,
    },
    /// Loops back to the start of the sequence. `count == 0` means forever.
    Repeat {
        count: u32,
    },
}

impl Block {
    pub fn new_press_key() -> Self {
        Block::PressKey { key: "Space".to_string(), hold_ms: 50 }
    }
    pub fn new_click() -> Self {
        Block::Click { hold_ms: 50 }
    }
    pub fn new_wait() -> Self {
        Block::Wait { ms: 500 }
    }
    pub fn new_repeat() -> Self {
        Block::Repeat { count: 0 }
    }

    pub fn title(&self) -> String {
        match self {
            Block::Function { name, .. } => name.clone(),
            Block::PressKey { key, .. } => format!("Press {key}"),
            Block::Click { .. } => "Click".to_string(),
            Block::Wait { ms } => format!("Wait {ms}ms"),
            Block::Repeat { count } => {
                if *count == 0 {
                    "Repeat (forever)".to_string()
                } else {
                    format!("Repeat ({count}x)")
                }
            }
        }
    }

    /// Function blocks can only be viewed/edited as text; primitive
    /// blocks are the ones runner.rs can actually interpret and run.
    pub fn is_runnable_primitive(&self) -> bool {
        !matches!(self, Block::Function { .. })
    }
}

/// Renders one block as a rounded card, with inline editable fields for
/// primitive blocks. Returns true if the card's title was clicked
/// (Function blocks only — opens the code drawer).
pub fn card_ui(ui: &mut egui::Ui, block: &mut Block) -> bool {
    use egui::RichText;

    let frame = egui::Frame::group(ui.style())
        .corner_radius(14)
        .inner_margin(egui::Margin::same(12))
        .fill(ui.visuals().faint_bg_color);

    let mut clicked = false;
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("⠿").weak()); // drag handle glyph
            ui.vertical(|ui| {
                match block {
                    Block::Function { name, doc, .. } => {
                        let response = ui.label(RichText::new(name.as_str()).strong().size(15.0));
                        if !doc.is_empty() {
                            ui.label(RichText::new(doc.as_str()).weak().size(11.0));
                        }
                        clicked = response.interact(egui::Sense::click()).clicked();
                    }
                    Block::PressKey { key, hold_ms } => {
                        ui.label(RichText::new("Press Key").strong().size(15.0));
                        ui.horizontal(|ui| {
                            ui.label("key");
                            ui.text_edit_singleline(key);
                            ui.label("hold (ms)");
                            ui.add(egui::DragValue::new(hold_ms).range(0..=5000));
                        });
                    }
                    Block::Click { hold_ms } => {
                        ui.label(RichText::new("Click").strong().size(15.0));
                        ui.horizontal(|ui| {
                            ui.label("hold (ms)");
                            ui.add(egui::DragValue::new(hold_ms).range(0..=5000));
                        });
                    }
                    Block::Wait { ms } => {
                        ui.label(RichText::new("Wait").strong().size(15.0));
                        ui.horizontal(|ui| {
                            ui.label("ms");
                            ui.add(egui::DragValue::new(ms).range(0..=60_000));
                        });
                    }
                    Block::Repeat { count } => {
                        ui.label(RichText::new("Repeat").strong().size(15.0));
                        ui.horizontal(|ui| {
                            ui.label("times (0 = forever)");
                            ui.add(egui::DragValue::new(count).range(0..=10_000));
                        });
                    }
                }
            });
        });
    });

    clicked
}
