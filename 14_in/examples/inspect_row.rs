use fish_bar_bot::vision;
use std::env;

fn main() {
    let path = env::args().nth(1).expect("usage: inspect_row <path.png> [row] [upscale]");
    let row_arg: u32 = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(17);
    let upscale: u32 = env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(6);

    let img = image::open(&path).unwrap().to_rgba8();
    let (w, h) = img.dimensions();
    let orig_w = w / upscale;
    let orig_h = h / upscale;

    let mut small = image::RgbaImage::new(orig_w, orig_h);
    for y in 0..orig_h {
        for x in 0..orig_w {
            let p = img.get_pixel(x * upscale, y * upscale);
            small.put_pixel(x, y, *p);
        }
    }

    let hsv = vision::frame_to_hsv(&small);
    let row = row_arg.min(orig_h - 1);
    println!("# {} ({orig_w}x{orig_h} downsampled), row {row}", path);

    let mut prev: Option<(u8, u8, u8)> = None;
    let mut run_start = 0u32;
    for x in 0..orig_w {
        let idx = (row * orig_w + x) as usize;
        let cur = hsv.pixels[idx];
        let similar = prev.is_some_and(|p: (u8, u8, u8)| {
            (p.0 as i16 - cur.0 as i16).abs() <= 4
                && (p.1 as i16 - cur.1 as i16).abs() <= 12
                && (p.2 as i16 - cur.2 as i16).abs() <= 12
        });
        if !similar {
            if let Some(p) = prev {
                println!("cols {run_start:>3}-{:>3}  H{:>3} S{:>3} V{:>3}", x.saturating_sub(1), p.0, p.1, p.2);
            }
            run_start = x;
            prev = Some(cur);
        }
    }
    if let Some(p) = prev {
        println!("cols {run_start:>3}-{:>3}  H{:>3} S{:>3} V{:>3}", orig_w - 1, p.0, p.1, p.2);
    }
}
