//! Screen capture, with the same Retina/HiDPI normalization the Python
//! version needed for mss: `Monitor::capture_region` returns an image sized
//! to the display's PHYSICAL pixel resolution, which on a Retina display is
//! 2x the logical/point coordinates the region was requested in — resize
//! back down to the requested size so every pixel-based constant elsewhere
//! (BAR_MERGE_MAX_GAP_PX, NEEDLE_MAX_WIDTH_PX, ...) doesn't need its own
//! rescaling.

use image::{RgbaImage, imageops::FilterType};
use xcap::Monitor;

use crate::config::Region;

pub fn find_primary_monitor() -> anyhow::Result<Monitor> {
    let monitors = Monitor::all()?;
    for m in &monitors {
        if m.is_primary().unwrap_or(false) {
            return Ok(m.clone());
        }
    }
    monitors
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no monitors found"))
}

/// Captures a region given in absolute virtual-desktop coordinates,
/// resolving whichever physical monitor that region actually lives on
/// (rather than assuming the primary display) — matters on multi-monitor
/// setups where the target app isn't on the primary screen.
pub fn grab_region_normalized(region: Region) -> anyhow::Result<RgbaImage> {
    let monitor = Monitor::from_point(region.left, region.top)?;
    let mon_x = monitor.x()?;
    let mon_y = monitor.y()?;
    let rel_x = (region.left - mon_x).max(0) as u32;
    let rel_y = (region.top - mon_y).max(0) as u32;

    let frame = monitor.capture_region(rel_x, rel_y, region.width, region.height)?;
    if frame.width() != region.width || frame.height() != region.height {
        Ok(image::imageops::resize(
            &frame,
            region.width,
            region.height,
            FilterType::Triangle,
        ))
    } else {
        Ok(frame)
    }
}
