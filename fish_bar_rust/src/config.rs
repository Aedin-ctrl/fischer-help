//! Direct port of the CONFIG section of fish_bar_new.py. Values, names, and
//! grouping comments are kept 1:1 with the Python original so the two files
//! stay easy to diff against each other.

use rdev::Key;
use std::time::Duration;

pub const TARGET_APP_NAME: &str = "Roblox";

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

pub const REGION: Region = Region { left: 426, top: 785, width: 589, height: 35 };

/// One HSV range: (low, high), each (h, s, v) with h in 0..=179, s/v in 0..=255 —
/// same scale OpenCV uses, since these values were tuned against cv2's HSV.
pub type HsvRange = ((u8, u8, u8), (u8, u8, u8));

/// The control bar isn't always the same color — see fish_bar_new.py's
/// CONTROLLED_BAR_HSV_RANGES comment for the full story. Union of all of
/// these ranges is what counts as "the bar".
///
/// The former third range, a "dark green" guess (H35-90, S15-180, V20-150),
/// is REMOVED: calibration data (calib_bar_idle_background.png and every
/// active-cycle frame alike) showed it exactly matching the game's plain
/// grass/foliage background scenery -- present in literally every frame
/// regardless of whether the bar was even on screen. BAR_MERGE_MAX_GAP_PX
/// then bridged that false background blob straight into the real white
/// bar, which is what was producing nonsense readings like "bar=0-589"
/// (the whole captured region) in practice. No evidence was found of a
/// legitimate bar state actually needing this range -- the confirmed dim
/// "pin not on it" state samples as brown (H15, see range below), not
/// green.
pub const CONTROLLED_BAR_HSV_RANGES: &[HsvRange] = &[
    ((0, 0, 150), (180, 40, 255)),  // normal: bright/white
    ((8, 50, 50), (22, 100, 100)), // confirmed dim/"pin not on it": brown, H15 S76-78 V74-78 --
                                    // tightened from the old (0,15,20)-(25,180,150) guess, which
                                    // was ALSO catching unrelated reddish background texture
                                    // (confirmed H5-7 S101-114 V73-76 in calib_bar_cycle1_end.png,
                                    // spanning most of the frame). This box sits tightly around
                                    // the one real sample with margin, while excluding that
                                    // background reading on both H and S.
];

/// See BAR_MERGE_MAX_GAP_PX in the Python original: bridges gaps between
/// same-color blobs split by the needle drawn on top of the bar, without
/// blanket-unioning unrelated background matches elsewhere in frame.
pub const BAR_MERGE_MAX_GAP_PX: u32 = 40;

/// A final merged bar span narrower than this is rejected outright (no
/// bar detected this frame), rather than trusting it. Real calibration
/// data showed the genuine bar staying at least ~300px wide across every
/// actively-observed frame (its narrowest measured width was 346px), while
/// stray leftover matches once the bar was truly gone measured only a few
/// px up to a few dozen px. Without this floor, a tiny noise match could
/// keep registering as "the bar is still here" indefinitely -- which is
/// what was causing the control loop to keep tapping well after the bar
/// had actually disappeared on screen. 150px sits with generous margin
/// below the smallest real reading and above the largest observed noise.
pub const BAR_MIN_VALID_WIDTH_PX: f64 = 150.0;

/// Max plausible frame-to-frame needle movement, in px, once a recent
/// known-good position exists. Real captured gameplay showed legitimate
/// tracking taking smooth ~5-30px steps per tick, versus 100-400px+
/// teleports across the whole region when some unrelated colorful pixel
/// elsewhere in frame briefly matched the needle's HSV ranges instead.
/// 80px sits with generous margin above real movement and well below
/// the observed teleports.
pub const NEEDLE_MAX_JUMP_PX: f64 = 80.0;

/// How long a known-good needle position stays trusted as a continuity
/// hint before being dropped (falling back to plain biggest-area
/// selection). Needs to outlive a normal decision tick (DECISION_INTERVAL)
/// with margin, but not so long that a real, larger jump after a longer
/// gap (e.g. right as the bar first appears) gets incorrectly rejected
/// forever.
pub const NEEDLE_HINT_MAX_AGE: Duration = Duration::from_millis(500);

/// Blobs smaller than this fraction of the frame's area are noise, ignored
/// even for gap-bridging.
pub const BAR_MIN_BLOB_AREA_FRACTION: f64 = 0.01;

/// The needle/pin isn't always one color either — union of confirmed states.
/// See NEEDLE_HSV_RANGES in the Python original for the calibration history.
///
/// Range 0's S-min lowered from 41 to 20, based on real calibration data
/// (calib_bar_report.txt / calib_bar_cycle*_mid*.png from a live session):
/// the pin's actual color reads H105-121, with saturation peaking at only
/// 41-62 in a ~6px core and trailing off to S9-24 across its ~15-20px
/// anti-aliased edges. Requiring S>=41 only ever caught that narrow core,
/// so ordinary frame-to-frame rendering variance (motion blur, sub-pixel
/// position) dipping just under 41 made the needle vanish from detection
/// entirely -- matches the "needle not recognized" spam seen in practice.
/// S=20 still sits safely above the two arrow icons flanking the pin
/// (measured at S1-9, neutral gray) and above general background/water
/// texture in the same hue band (measured at S7-24, but as a wide diffuse
/// region that NEEDLE_MAX_WIDTH_PX already rejects regardless).
pub const NEEDLE_HSV_RANGES: &[HsvRange] = &[
    ((95, 20, 40), (130, 150, 255)),   // blue/purple
    ((131, 60, 90), (179, 220, 220)),  // violet/magenta
];

/// A needle-range match wider than this is almost certainly the wide dim bar
/// getting picked up instead of the thin needle line — reject it.
pub const NEEDLE_MAX_WIDTH_PX: u32 = 30;

// ------------------------------------------------------------------------
// REACTIVE CONTROL LOOP — tap timing + where to hold the pin
// ------------------------------------------------------------------------

pub const HOLD_KEY: Key = Key::Space;

/// Fraction from the left edge (0.0) to right edge (1.0) of the bar the pin
/// should be held at. Deliberately left of dead-center — see
/// PIN_TARGET_FRACTION in the Python original for why a bang-bang controller
/// like this one can never settle exactly on its aim point.
pub const PIN_TARGET_FRACTION: f64 = 1.0 / 4.0;

/// Length of each tap. Constant, never scaled by distance — see TAP_SECONDS
/// in the Python original for why (0.02s wasn't enough to out-push the
/// constant leftward acceleration).
pub const TAP_SECONDS: Duration = Duration::from_millis(50);
pub const KEEPALIVE_TAP_INTERVAL: Duration = Duration::from_millis(200);

pub const GAME_FPS: f64 = 60.0;
pub const DECISION_FRAME_SKIP: f64 = 3.0;
pub const DECISION_INTERVAL: Duration =
    Duration::from_micros((DECISION_FRAME_SKIP / GAME_FPS * 1_000_000.0) as u64);

/// How long bar AND needle must both stay undetected before the stage is
/// considered actually over — real elapsed time, not a frame count. See
/// MISSING_GRACE_SECONDS in the Python original.
pub const MISSING_GRACE: Duration = Duration::from_millis(2000);

/// The bar's cached last-seen position is reused for this long while it's
/// currently dim/undetected. See BAR_CACHE_MAX_AGE_SECONDS in the Python
/// original.
pub const BAR_CACHE_MAX_AGE: Duration = Duration::from_millis(1500);

pub const FOCUS_CHECK_INTERVAL: Duration = Duration::from_millis(500);
pub const COLOR_DEBUG_PRINT_INTERVAL: Duration = Duration::from_millis(1000);

// ------------------------------------------------------------------------
// CAST SEQUENCE — 9, 1, hold-click, then a burst of Enters
// ------------------------------------------------------------------------

pub const PRE_TRIGGER_KEYS: &[(Key, &str)] = &[(Key::Num9, "9"), (Key::Num1, "1")];
pub const PRE_TRIGGER_KEY_HOLD: Duration = Duration::from_millis(120);
pub const PRE_TRIGGER_KEY_DELAY: Duration = Duration::from_millis(500);
pub const TRIGGER_HOLD: Duration = Duration::from_millis(1000);
pub const TRIGGER_ENTER_COUNT: u32 = 12;
pub const TRIGGER_ENTER_INTERVAL: Duration = Duration::from_millis(100);
pub const TRIGGER_ENTER_HOLD: Duration = Duration::from_millis(150);
pub const POST_HOLD_ENTER_DELAY: Duration = Duration::from_millis(500);

// ------------------------------------------------------------------------
// LOOP PACING / SAFETY TIMEOUTS
// ------------------------------------------------------------------------

pub const POST_CHECK_DELAY: Duration = Duration::from_millis(1000);
// Bumped from 25s -- calibration runs (blind-tap only, no reactive
// correction yet) repeatedly hit the old 25s cap mid-fight and forced a
// recast before the bar ever cleared on its own. 45s gives real fights
// more room; re-tune once the needle is reliably detected and the bot is
// actually reacting instead of blind-tapping.
pub const CONTROL_TIMEOUT: Duration = Duration::from_millis(45_000);

// ------------------------------------------------------------------------
// DAY/NIGHT CHECK — moon icon (night) and sun icon (day), same spot
// ------------------------------------------------------------------------

pub const SESSIONS_BETWEEN_MOON_CHECKS: u32 = 5;

pub const MOON_CHECK_CORNER_A: (i32, i32) = (1397, 843);
pub const MOON_CHECK_CORNER_B: (i32, i32) = (1371, 810);
pub const MOON_CHECK_MARGIN: i32 = 4;

pub const MOON_MATCH_THRESHOLD: f64 = 0.6;

pub const MOON_CHECK_PRE_CAST_DELAY: Duration = Duration::from_millis(1000);

pub const MOON_ACTION_KEY: Key = Key::Num5;
pub const MOON_ACTION_KEY_HOLD: Duration = Duration::from_millis(100);
/// Delay before pressing the totem slot -- gives a moment before the press
/// instead of firing it the instant the icon's detected.
pub const MOON_ACTION_PRE_PRESS_DELAY: Duration = Duration::from_millis(200);
/// Held for 0.2s instead of a quick tap -- the quick tap wasn't registering.
pub const MOON_ACTION_TOTEM_HOLD: Duration = Duration::from_millis(200);

pub const MOON_ACTION_CLICK_COUNT: u32 = 4;
pub const MOON_ACTION_CLICK_HOLD: Duration = Duration::from_millis(200);
pub const MOON_ACTION_CLICK_BREAK: Duration = Duration::from_millis(500);
pub const MOON_ACTION_CLICK_JITTER_PX: i32 = 50;
