//! Emergency utility: sends a real key-up for Space, in case a crashed or
//! killed bot run left it stuck held down.

fn main() {
    let _ = rdev::simulate(&rdev::EventType::KeyRelease(rdev::Key::Space));
    println!("Sent Space key-up.");
}
