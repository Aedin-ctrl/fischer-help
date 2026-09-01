//! HSV color detection + blob-bounds finding, mirroring detect_both(),
//! find_bar_x_bounds_from_mask(), find_x_bounds_from_mask(), and the
//! color-debug sampling helpers from fish_bar_new.py. No OpenCV dependency:
//! blob bounds come from a hand-rolled 8-connected flood fill, since all we
//! need is bounding boxes + pixel-count "area", not real contours.

use image::RgbaImage;

use crate::config::HsvRange;

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub left: f64,
    pub right: f64,
    pub center: f64,
}

struct Blob {
    min_x: u32,
    max_x: u32,
    area: u32,
}

/// Matches cv2.cvtColor(BGR2HSV) scaling: H in 0..=179, S/V in 0..=255.
fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f64, g as f64, b as f64);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;

    let v = max;
    let s = if max == 0.0 { 0.0 } else { 255.0 * delta / max };

    let mut h = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    h /= 2.0; // fit OpenCV's 0..=179 8-bit hue range

    (h.round() as u8, s.round() as u8, v.round() as u8)
}

pub struct HsvFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<(u8, u8, u8)>,
}

pub fn frame_to_hsv(frame: &RgbaImage) -> HsvFrame {
    let (width, height) = frame.dimensions();
    let pixels = frame
        .pixels()
        .map(|p| rgb_to_hsv(p[0], p[1], p[2]))
        .collect();
    HsvFrame { width, height, pixels }
}

fn in_range(hsv: (u8, u8, u8), range: &HsvRange) -> bool {
    let (low, high) = *range;
    hsv.0 >= low.0 && hsv.0 <= high.0 && hsv.1 >= low.1 && hsv.1 <= high.1 && hsv.2 >= low.2 && hsv.2 <= high.2
}

pub fn union_mask(hsv: &HsvFrame, ranges: &[HsvRange]) -> Vec<bool> {
    hsv.pixels
        .iter()
        .map(|&px| ranges.iter().any(|r| in_range(px, r)))
        .collect()
}

/// 8-connected flood fill over the boolean mask, returning each blob's
/// x-span and pixel-count area — the same two things find_bar_x_bounds_
/// from_mask/find_x_bounds_from_mask actually use out of cv2's contours.
fn find_blobs(mask: &[bool], width: u32, height: u32) -> Vec<Blob> {
    let (w, h) = (width as i32, height as i32);
    let mut visited = vec![false; mask.len()];
    let mut blobs = Vec::new();
    let mut stack: Vec<(i32, i32)> = Vec::new();

    for y0 in 0..h {
        for x0 in 0..w {
            let idx0 = (y0 * w + x0) as usize;
            if !mask[idx0] || visited[idx0] {
                continue;
            }
            visited[idx0] = true;
            stack.push((x0, y0));
            let (mut min_x, mut max_x) = (x0, x0);
            let mut area: u32 = 0;

            while let Some((x, y)) = stack.pop() {
                area += 1;
                if x < min_x {
                    min_x = x;
                }
                if x > max_x {
                    max_x = x;
                }
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= w || ny >= h {
                            continue;
                        }
                        let nidx = (ny * w + nx) as usize;
                        if mask[nidx] && !visited[nidx] {
                            visited[nidx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }

            blobs.push(Blob { min_x: min_x as u32, max_x: max_x as u32, area });
        }
    }

    blobs
}

/// Bounding x-span of the bar, bridging small gaps between significant
/// blobs (see BAR_MERGE_MAX_GAP_PX) instead of just keeping the biggest one
/// — the needle splits the bar's own color mask into two blobs, and picking
/// just the biggest silently throws away the other half.
///
/// Rejects (returns None for) a final merged span narrower than
/// min_valid_width_px: real calibration data showed the genuine bar
/// staying at least ~300px wide across every actively-observed frame,
/// while stray background/noise matches after the bar was truly gone
/// measured only a few px to a few dozen px wide. Without this floor, a
/// tiny leftover match could keep registering as "the bar is still here"
/// indefinitely, which is what was causing the control loop to keep
/// tapping well after the bar had actually disappeared on screen.
pub fn find_bar_x_bounds(
    mask: &[bool],
    width: u32,
    height: u32,
    min_area_fraction: f64,
    max_gap_px: u32,
    min_valid_width_px: f64,
) -> Option<Bounds> {
    let blobs = find_blobs(mask, width, height);
    if blobs.is_empty() {
        return None;
    }

    let min_area = (min_area_fraction * (width * height) as f64) as u32;
    let mut significant: Vec<&Blob> = blobs.iter().filter(|b| b.area >= min_area).collect();
    if significant.is_empty() {
        significant.push(blobs.iter().max_by_key(|b| b.area).unwrap());
    }

    let mut spans: Vec<(u32, u32)> = significant.iter().map(|b| (b.min_x, b.max_x + 1)).collect();
    spans.sort();

    let mut merged: Vec<(u32, u32)> = vec![spans[0]];
    for &(left, right) in &spans[1..] {
        let last = merged.last_mut().unwrap();
        if left as i64 - last.1 as i64 <= max_gap_px as i64 {
            last.1 = last.1.max(right);
        } else {
            merged.push((left, right));
        }
    }

    let (left, right) = *merged.iter().max_by_key(|(l, r)| r - l).unwrap();
    if ((right - left) as f64) < min_valid_width_px {
        return None;
    }
    Some(Bounds { left: left as f64, right: right as f64, center: left as f64 + (right - left) as f64 / 2.0 })
}

/// Bounding x-span of the needle, among blobs NOT wider than max_width_px
/// (since the needle's own color ranges can overlap the bar's dim "pin
/// not on it" tint, and a match that's implausibly wide is almost
/// certainly the bar, not the thin needle line).
///
/// When `preferred_center` is given (the needle's last known-good
/// position), the candidate closest to it is picked instead of the
/// biggest-area one, and rejected entirely (returns None) if even the
/// closest candidate is farther than `max_jump_px` away. Real captured
/// gameplay showed the needle taking smooth ~5-30px steps per tick in
/// legitimate tracking, versus 100-400px+ teleports across the whole
/// region when some unrelated colorful pixel elsewhere in frame briefly
/// matched the needle's (fairly wide) HSV ranges instead. Without this,
/// those teleports fed straight into the tap decision as if they were
/// real needle movement. Pass `preferred_center: None` for plain
/// biggest-area selection (e.g. right when the bar first appears, or
/// once the hint has gone stale).
pub fn find_needle_x_bounds(
    mask: &[bool],
    width: u32,
    height: u32,
    max_width_px: u32,
    preferred_center: Option<f64>,
    max_jump_px: f64,
) -> Option<Bounds> {
    let blobs = find_blobs(mask, width, height);
    let qualifying: Vec<&Blob> = blobs.iter().filter(|b| b.max_x - b.min_x + 1 <= max_width_px).collect();
    if qualifying.is_empty() {
        return None;
    }

    let to_bounds = |b: &Blob| {
        let w = b.max_x - b.min_x + 1;
        Bounds { left: b.min_x as f64, right: (b.max_x + 1) as f64, center: b.min_x as f64 + w as f64 / 2.0 }
    };

    match preferred_center {
        Some(pc) => {
            let best = qualifying
                .iter()
                .min_by(|a, b| {
                    let da = (to_bounds(a).center - pc).abs();
                    let db = (to_bounds(b).center - pc).abs();
                    da.partial_cmp(&db).unwrap()
                })
                .unwrap();
            let bounds = to_bounds(best);
            if (bounds.center - pc).abs() <= max_jump_px { Some(bounds) } else { None }
        }
        None => {
            let best = qualifying.iter().max_by_key(|b| b.area).unwrap();
            Some(to_bounds(best))
        }
    }
}

pub struct DetectResult {
    pub bar: Option<Bounds>,
    pub needle: Option<Bounds>,
    pub hsv: HsvFrame,
}

#[allow(clippy::too_many_arguments)]
pub fn detect_both(
    frame: &RgbaImage,
    bar_ranges: &[HsvRange],
    needle_ranges: &[HsvRange],
    bar_min_area_fraction: f64,
    bar_max_gap_px: u32,
    bar_min_valid_width_px: f64,
    needle_max_width_px: u32,
    needle_preferred_center: Option<f64>,
    needle_max_jump_px: f64,
) -> DetectResult {
    let hsv = frame_to_hsv(frame);
    let bar_mask = union_mask(&hsv, bar_ranges);
    let needle_mask = union_mask(&hsv, needle_ranges);
    let bar = find_bar_x_bounds(&bar_mask, hsv.width, hsv.height, bar_min_area_fraction, bar_max_gap_px, bar_min_valid_width_px);
    let needle = find_needle_x_bounds(&needle_mask, hsv.width, hsv.height, needle_max_width_px, needle_preferred_center, needle_max_jump_px);
    DetectResult { bar, needle, hsv }
}

/// When the control bar (or needle) can't be found by any known range,
/// sample whatever colorful (non-background, non-excluded) blob is on
/// screen so its real HSV range can be read straight off the console —
/// mirrors sample_unrecognized_color / sample_unrecognized_needle_color.
pub fn sample_unrecognized_color(hsv: &HsvFrame, exclude_ranges: &[HsvRange]) -> Option<((u8, u8, u8), (u8, u8, u8))> {
    let colorful_range: HsvRange = ((0, 25, 20), (180, 255, 255));
    let mut min = (255u8, 255u8, 255u8);
    let mut max = (0u8, 0u8, 0u8);
    let mut found = false;

    for &px in &hsv.pixels {
        if !in_range(px, &colorful_range) {
            continue;
        }
        if exclude_ranges.iter().any(|r| in_range(px, r)) {
            continue;
        }
        found = true;
        min = (min.0.min(px.0), min.1.min(px.1), min.2.min(px.2));
        max = (max.0.max(px.0), max.1.max(px.1), max.2.max(px.2));
    }

    found.then_some((min, max))
}

/// Faithful reimplementation of cv2.matchTemplate(TM_CCOEFF_NORMED) over
/// RGB channels jointly (per-channel means, summed numerator/denominator
/// across all channels together — matching OpenCV's multi-channel
/// behavior), used for the moon/sun icon check.
pub fn match_template_max(search: &RgbaImage, template: &RgbaImage) -> f64 {
    let (sw, sh) = search.dimensions();
    let (tw, th) = template.dimensions();
    if tw > sw || th > sh || tw == 0 || th == 0 {
        return 0.0;
    }

    // Icon templates are cropped to their bounding box but stay irregular
    // (a round sun/moon inside a rectangle), so plenty of pixels inside
    // that box are still fully transparent. Comparing RGB on those pixels
    // means whatever leftover color sits in fully-transparent PNG data
    // gets treated as real icon color and dragged into the correlation --
    // masking them out entirely (both here and on the search side, at
    // every candidate window) is what makes this an actual icon-shape
    // match instead of a bounding-box-rectangle match.
    const ALPHA_THRESHOLD: u8 = 10;
    let mask: Vec<bool> = template.pixels().map(|p| p[3] > ALPHA_THRESHOLD).collect();
    let n = mask.iter().filter(|&&m| m).count() as f64;
    if n == 0.0 {
        return 0.0;
    }

    let mut t_mean = [0f64; 3];
    for (p, &m) in template.pixels().zip(&mask) {
        if !m {
            continue;
        }
        for c in 0..3 {
            t_mean[c] += p[c] as f64;
        }
    }
    for c in 0..3 {
        t_mean[c] /= n;
    }

    let mut t_denom = 0f64;
    let mut t_centered = vec![[0f64; 3]; (tw * th) as usize];
    for (i, (p, &m)) in template.pixels().zip(&mask).enumerate() {
        if !m {
            continue;
        }
        for c in 0..3 {
            let d = p[c] as f64 - t_mean[c];
            t_centered[i][c] = d;
            t_denom += d * d;
        }
    }

    let mut best = f64::MIN;
    for oy in 0..=(sh - th) {
        for ox in 0..=(sw - tw) {
            let mut i_mean = [0f64; 3];
            for ty in 0..th {
                for tx in 0..tw {
                    if !mask[(ty * tw + tx) as usize] {
                        continue;
                    }
                    let p = search.get_pixel(ox + tx, oy + ty);
                    for c in 0..3 {
                        i_mean[c] += p[c] as f64;
                    }
                }
            }
            for c in 0..3 {
                i_mean[c] /= n;
            }

            let mut numerator = 0f64;
            let mut i_denom = 0f64;
            for ty in 0..th {
                for tx in 0..tw {
                    let idx = (ty * tw + tx) as usize;
                    if !mask[idx] {
                        continue;
                    }
                    let p = search.get_pixel(ox + tx, oy + ty);
                    for c in 0..3 {
                        let d = p[c] as f64 - i_mean[c];
                        numerator += t_centered[idx][c] * d;
                        i_denom += d * d;
                    }
                }
            }

            let denom = (t_denom * i_denom).sqrt();
            let score = if denom > 1e-10 { numerator / denom } else { 0.0 };
            if score > best {
                best = score;
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_matches_opencv_known_points() {
        // Pure white -> H=0 S=0 V=255 (matches CONTROLLED_BAR_HSV_RANGES "normal" range)
        assert_eq!(rgb_to_hsv(255, 255, 255), (0, 0, 255));
        // Pure red -> H=0 S=255 V=255
        assert_eq!(rgb_to_hsv(255, 0, 0), (0, 255, 255));
        // Pure green -> H=60 (120/2) S=255 V=255
        assert_eq!(rgb_to_hsv(0, 255, 0), (60, 255, 255));
        // Pure blue -> H=120 (240/2) S=255 V=255
        assert_eq!(rgb_to_hsv(0, 0, 255), (120, 255, 255));
        // Black -> V=0 S=0
        assert_eq!(rgb_to_hsv(0, 0, 0), (0, 0, 0));
    }

    fn make_mask(width: u32, height: u32, on: &[(u32, u32)]) -> Vec<bool> {
        let mut m = vec![false; (width * height) as usize];
        for &(x, y) in on {
            m[(y * width + x) as usize] = true;
        }
        m
    }

    #[test]
    fn bar_bounds_bridge_needle_gap() {
        // 40px wide bar split by a needle gap of 5px around x=20 -- should
        // merge back into one 0..40 span, not two halves.
        let width = 40;
        let height = 4;
        let mut on = Vec::new();
        for y in 0..height {
            for x in 0..17 {
                on.push((x, y));
            }
            for x in 22..40 {
                on.push((x, y));
            }
        }
        let mask = make_mask(width, height, &on);
        let bounds = find_bar_x_bounds(&mask, width, height, 0.01, 40, 0.0).unwrap();
        assert_eq!(bounds.left, 0.0);
        assert_eq!(bounds.right, 40.0);
    }

    #[test]
    fn bar_bounds_reject_far_background_blob() {
        // A real bar span plus a small, far-away unrelated blob should NOT
        // get merged in (gap far exceeds max_gap_px).
        let width = 100;
        let height = 4;
        let mut on = Vec::new();
        for y in 0..height {
            for x in 0..20 {
                on.push((x, y)); // "the bar"
            }
        }
        for y in 0..height {
            for x in 90..95 {
                on.push((x, y)); // unrelated background blob far away
            }
        }
        let mask = make_mask(width, height, &on);
        let bounds = find_bar_x_bounds(&mask, width, height, 0.01, 40, 0.0).unwrap();
        // Biggest merged span should still be the real bar (0..20), not
        // absorbing the background blob 70px away.
        assert_eq!(bounds.left, 0.0);
        assert_eq!(bounds.right, 20.0);
    }

    #[test]
    fn bar_bounds_rejects_narrow_noise() {
        // A plausible-looking but tiny blob (stray background match after
        // the real bar is gone) should be rejected outright when it's
        // narrower than the caller's min_valid_width_px floor.
        let width = 100;
        let height = 4;
        let mut on = Vec::new();
        for y in 0..height {
            for x in 40..45 {
                on.push((x, y)); // 5px sliver -- not a real bar
            }
        }
        let mask = make_mask(width, height, &on);
        assert!(find_bar_x_bounds(&mask, width, height, 0.01, 40, 60.0).is_none());
        // Same blob with no floor still resolves normally.
        assert!(find_bar_x_bounds(&mask, width, height, 0.01, 40, 0.0).is_some());
    }

    #[test]
    fn needle_rejects_overwide_match() {
        // A wide blob (the bar bleeding into needle's range) should be
        // skipped in favor of a narrower real needle blob.
        let width = 100;
        let height = 4;
        let mut on = Vec::new();
        for y in 0..height {
            for x in 0..50 {
                on.push((x, y)); // wide "bar" blob -- must be rejected
            }
        }
        for y in 0..height {
            on.push((70, y)); // narrow real needle, 1px wide
        }
        let mask = make_mask(width, height, &on);
        let bounds = find_needle_x_bounds(&mask, width, height, 30, None, 0.0).unwrap();
        assert_eq!(bounds.left, 70.0);
        assert_eq!(bounds.right, 71.0);
    }

    #[test]
    fn needle_continuity_rejects_distant_teleport() {
        // Two qualifying candidates: one near a given preferred_center,
        // one far away (mimicking a stray unrelated pixel elsewhere in
        // frame). With a hint, the near one should win even though it's
        // not the biggest; without a hint, biggest-area wins regardless.
        let width = 200;
        let height = 4;
        let mut on = Vec::new();
        for y in 0..height {
            on.push((50, y)); // near candidate, center ~50
        }
        for y in 0..height {
            for x in 150..153 {
                on.push((x, y)); // far, bigger candidate, center ~151
            }
        }
        let mask = make_mask(width, height, &on);

        // With a hint near 50 and a generous jump budget, picks the near one.
        let bounds = find_needle_x_bounds(&mask, width, height, 30, Some(48.0), 20.0).unwrap();
        assert_eq!(bounds.center, 50.5);

        // With a hint near 50 but a tight jump budget that only the near
        // candidate satisfies -- same result, confirming the far one was
        // never even considered close enough to win.
        let bounds = find_needle_x_bounds(&mask, width, height, 30, Some(48.0), 5.0).unwrap();
        assert_eq!(bounds.center, 50.5);

        // With a hint nothing is near (both candidates far away), rejected
        // entirely rather than snapping to whichever is closest.
        assert!(find_needle_x_bounds(&mask, width, height, 30, Some(1000.0), 20.0).is_none());

        // No hint at all -- falls back to biggest-area (the far one, 3px wide).
        let bounds = find_needle_x_bounds(&mask, width, height, 30, None, 0.0).unwrap();
        assert_eq!(bounds.center, 151.5);
    }
}
