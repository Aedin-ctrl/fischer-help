//! Tiny helper: prints the live mouse position so you can hover over
//! something on screen (e.g. the sun/moon icon) and read off its pixel
//! coordinates. Ctrl+C to stop.

use std::io::{self, Write};

use rdev::{listen, EventType};

fn main() {
    println!("Move your mouse over the icon and read the coordinates below. Ctrl+C to stop.");
    let callback = |event: rdev::Event| {
        if let EventType::MouseMove { x, y } = event.event_type {
            print!("x={x:.0} y={y:.0}    \r");
            let _ = io::stdout().flush();
        }
    };
    if let Err(e) = listen(callback) {
        eprintln!("failed to listen: {e:?} (check Accessibility permission)");
    }
}
