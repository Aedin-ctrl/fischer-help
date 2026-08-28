//! Runs either the whole loaded program (Process mode, for sequences that
//! include Function blocks) or a real interpreted sequence of primitive
//! Action blocks (Actions mode) — the latter is the part that actually
//! executes blocks in the order you arrange them, via rdev::simulate().
//!
//! rdev::simulate() posts synthetic input events directly (Quartz Event
//! Services) and doesn't touch the Text Services Manager calls that made
//! rdev::listen() require the main thread (see main.rs's module doc) — so
//! it's safe to drive from a background thread here.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rdev::{simulate, Button, EventType, Key};

use crate::blocks::Block;

enum Mode {
    Process { pid: u32 },
    Actions { stop: Arc<AtomicBool>, paused: Arc<AtomicBool> },
}

pub struct Runner {
    mode: Option<Mode>,
    paused: bool,
    pub output: Arc<Mutex<Vec<String>>>,
    exit_rx: Option<Receiver<()>>,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            mode: None,
            paused: false,
            output: Arc::new(Mutex::new(Vec::new())),
            exit_rx: None,
        }
    }
}

impl Runner {
    pub fn is_running(&self) -> bool {
        self.mode.is_some()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Reaps the exit notification if the run ended on its own since the
    /// last check, so `is_running` reflects reality without blocking.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.exit_rx {
            if rx.try_recv().is_ok() {
                self.mode = None;
                self.paused = false;
                self.exit_rx = None;
            }
        }
    }

    pub fn start_process(&mut self, program: &str, args: &[&str], cwd: &std::path::Path) -> Result<(), String> {
        if self.mode.is_some() {
            return Err("already running".to_string());
        }

        self.output.lock().unwrap().clear();

        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start {program}: {e}"))?;

        pipe_to_output(child.stdout.take(), self.output.clone());
        pipe_to_output(child.stderr.take(), self.output.clone());

        let pid = child.id();
        let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = tx.send(());
        });

        self.mode = Some(Mode::Process { pid });
        self.exit_rx = Some(rx);
        Ok(())
    }

    /// Interprets `blocks` in order, actually pressing keys/clicking/
    /// waiting for real — Repeat loops back to the first block, `count`
    /// times (0 = forever). Function blocks are skipped with a log line,
    /// since there's no safe way to dynamically invoke arbitrary
    /// extracted source.
    pub fn start_actions(&mut self, blocks: Vec<Block>) -> Result<(), String> {
        if self.mode.is_some() {
            return Err("already running".to_string());
        }

        self.output.lock().unwrap().clear();

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let output = self.output.clone();
        let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();

        let thread_stop = stop.clone();
        let thread_paused = paused.clone();
        std::thread::spawn(move || {
            run_action_sequence(&blocks, &output, &thread_stop, &thread_paused);
            let _ = tx.send(());
        });

        self.mode = Some(Mode::Actions { stop, paused });
        self.exit_rx = Some(rx);
        Ok(())
    }

    pub fn pause(&mut self) {
        match &self.mode {
            #[cfg(target_os = "macos")]
            Some(Mode::Process { pid }) => unsafe {
                libc::kill(*pid as i32, libc::SIGSTOP);
            },
            Some(Mode::Actions { paused, .. }) => paused.store(true, Ordering::SeqCst),
            _ => return,
        }
        self.paused = true;
    }

    pub fn resume(&mut self) {
        match &self.mode {
            #[cfg(target_os = "macos")]
            Some(Mode::Process { pid }) => unsafe {
                libc::kill(*pid as i32, libc::SIGCONT);
            },
            Some(Mode::Actions { paused, .. }) => paused.store(false, Ordering::SeqCst),
            _ => return,
        }
        self.paused = false;
    }

    pub fn stop(&mut self) {
        match &self.mode {
            #[cfg(target_os = "macos")]
            Some(Mode::Process { pid }) => unsafe {
                libc::kill(*pid as i32, libc::SIGTERM);
            },
            Some(Mode::Actions { stop, .. }) => stop.store(true, Ordering::SeqCst),
            _ => {}
        }
        self.mode = None;
        self.paused = false;
        self.exit_rx = None;
    }
}

fn run_action_sequence(blocks: &[Block], output: &Arc<Mutex<Vec<String>>>, stop: &AtomicBool, paused: &AtomicBool) {
    let log = |line: String| output.lock().unwrap().push(line);

    // How many passes to make: the LAST Repeat block (if any) sets the
    // loop count for everything before it. No Repeat block means "run
    // the sequence once."
    let repeat_count = blocks.iter().rev().find_map(|b| match b {
        Block::Repeat { count } => Some(*count),
        _ => None,
    });
    let steps: Vec<&Block> = blocks.iter().filter(|b| !matches!(b, Block::Repeat { .. })).collect();

    let mut pass = 0u32;
    loop {
        pass += 1;
        log(format!("--- pass {pass} ---"));

        for block in &steps {
            if wait_while_paused_or_stopped(stop, paused) {
                log("stopped".to_string());
                return;
            }
            run_one_block(block, &log);
        }

        match repeat_count {
            None => break,
            Some(0) => continue, // forever
            Some(n) if pass >= n => break,
            Some(_) => continue,
        }
    }

    log("done".to_string());
}

/// Sleeps in short increments so Stop/Pause take effect promptly instead
/// of only being checked between whole blocks. Returns true if stopped.
fn wait_while_paused_or_stopped(stop: &AtomicBool, paused: &AtomicBool) -> bool {
    while paused.load(Ordering::SeqCst) {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.load(Ordering::SeqCst)
}

fn run_one_block(block: &Block, log: &impl Fn(String)) {
    match block {
        Block::Function { name, .. } => {
            log(format!("skipping '{name}' — Function blocks aren't directly runnable"));
        }
        Block::PressKey { key, hold_ms } => match parse_key_name(key) {
            Some(k) => {
                let _ = simulate(&EventType::KeyPress(k));
                std::thread::sleep(Duration::from_millis(*hold_ms));
                let _ = simulate(&EventType::KeyRelease(k));
                log(format!("pressed {key} ({hold_ms}ms)"));
            }
            None => log(format!("unrecognized key name: {key:?}")),
        },
        Block::Click { hold_ms } => {
            let _ = simulate(&EventType::ButtonPress(Button::Left));
            std::thread::sleep(Duration::from_millis(*hold_ms));
            let _ = simulate(&EventType::ButtonRelease(Button::Left));
            log(format!("clicked ({hold_ms}ms)"));
        }
        Block::Wait { ms } => {
            std::thread::sleep(Duration::from_millis(*ms));
            log(format!("waited {ms}ms"));
        }
        Block::Repeat { .. } => unreachable!("Repeat is filtered out of steps"),
    }
}

/// Covers the key names this bot's own blocks actually use — not a
/// general-purpose key-name parser.
fn parse_key_name(name: &str) -> Option<Key> {
    let n = name.trim().to_lowercase();
    if let Some(rest) = n.strip_prefix("num") {
        return match rest {
            "0" => Some(Key::Num0),
            "1" => Some(Key::Num1),
            "2" => Some(Key::Num2),
            "3" => Some(Key::Num3),
            "4" => Some(Key::Num4),
            "5" => Some(Key::Num5),
            "6" => Some(Key::Num6),
            "7" => Some(Key::Num7),
            "8" => Some(Key::Num8),
            "9" => Some(Key::Num9),
            _ => None,
        };
    }
    if n.len() == 1 && n.chars().next().unwrap().is_ascii_digit() {
        return parse_key_name(&format!("num{n}"));
    }
    if n.len() == 1 && n.chars().next().unwrap().is_ascii_alphabetic() {
        return match n.as_str() {
            "a" => Some(Key::KeyA),
            "b" => Some(Key::KeyB),
            "c" => Some(Key::KeyC),
            "d" => Some(Key::KeyD),
            "e" => Some(Key::KeyE),
            "f" => Some(Key::KeyF),
            "g" => Some(Key::KeyG),
            "h" => Some(Key::KeyH),
            "i" => Some(Key::KeyI),
            "j" => Some(Key::KeyJ),
            "k" => Some(Key::KeyK),
            "l" => Some(Key::KeyL),
            "m" => Some(Key::KeyM),
            "n" => Some(Key::KeyN),
            "o" => Some(Key::KeyO),
            "p" => Some(Key::KeyP),
            "q" => Some(Key::KeyQ),
            "r" => Some(Key::KeyR),
            "s" => Some(Key::KeyS),
            "t" => Some(Key::KeyT),
            "u" => Some(Key::KeyU),
            "v" => Some(Key::KeyV),
            "w" => Some(Key::KeyW),
            "x" => Some(Key::KeyX),
            "y" => Some(Key::KeyY),
            "z" => Some(Key::KeyZ),
            _ => None,
        };
    }
    if let Some(rest) = n.strip_prefix('f') {
        if let Ok(fn_num) = rest.parse::<u8>() {
            return match fn_num {
                1 => Some(Key::F1),
                2 => Some(Key::F2),
                3 => Some(Key::F3),
                4 => Some(Key::F4),
                5 => Some(Key::F5),
                6 => Some(Key::F6),
                7 => Some(Key::F7),
                8 => Some(Key::F8),
                9 => Some(Key::F9),
                10 => Some(Key::F10),
                11 => Some(Key::F11),
                12 => Some(Key::F12),
                _ => None,
            };
        }
    }
    match n.as_str() {
        "space" => Some(Key::Space),
        "enter" | "return" => Some(Key::Return),
        "escape" | "esc" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        _ => None,
    }
}

fn pipe_to_output(stream: Option<impl std::io::Read + Send + 'static>, output: Arc<Mutex<Vec<String>>>) {
    let Some(stream) = stream else { return };
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            output.lock().unwrap().push(line);
        }
    });
}
