use fish_bar_bot::{config, vision};
use std::env;

fn main() {
    let path = env::args().nth(1).expect("usage: replay_detect <path.png> [upscale]");
    let upscale: u32 = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(6);

    let img = image::open(&path).unwrap().to_rgba8();
    let (w, h) = img.dimensions();
    let (orig_w, orig_h) = (w / upscale, h / upscale);

    let mut small = image::RgbaImage::new(orig_w, orig_h);
    for y in 0..orig_h {
        for x in 0..orig_w {
            small.put_pixel(x, y, *img.get_pixel(x * upscale, y * upscale));
        }
    }

    let result = vision::detect_both(
        &small,
        config::CONTROLLED_BAR_HSV_RANGES,
        config::NEEDLE_HSV_RANGES,
        config::BAR_MIN_BLOB_AREA_FRACTION,
        config::BAR_MERGE_MAX_GAP_PX,
        config::BAR_MIN_VALID_WIDTH_PX,
        config::NEEDLE_MAX_WIDTH_PX,
        None, // single-frame replay has no continuity to hint from
        config::NEEDLE_MAX_JUMP_PX,
    );

    let bar_str = result.bar.map_or("none".into(), |b| format!("{:.0}-{:.0}", b.left, b.right));
    let needle_str = result.needle.map_or("none".into(), |n| format!("{:.0}-{:.0} (width {:.0})", n.left, n.right, n.right - n.left));
    println!("{path}: bar={bar_str}  needle={needle_str}");
}
