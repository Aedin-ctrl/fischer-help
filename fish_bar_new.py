"""
Autonomous Bar Bot (macOS, terminal)

FULL LOOP THIS RUNS
---------------------
1. Cast: press 9, then 1, then hold-click for 1s and release, wait
   0.5s, then tap Enter 15 times at a 0.12s interval.
2. Immediately start jitter-clicking to hold position — this starts
   right after casting finishes, whether or not the bar has visibly
   appeared on screen yet. Waiting to check first just wastes time
   and lets the pin drift, so it clicks blind until it sees the whole
   bar for real.
3. Once the whole bar actually appears, switch to reacting to the
   pin/line's real position relative to the control bar (see CONTROL
   LOGIC below) until the whole bar disappears again. Disappearance is
   only ever evaluated after the bar has been seen at least once, so
   "hasn't appeared yet" can never be mistaken for "the check is
   already over."
4. Once the bar is gone, wait 1s. Every SESSIONS_BETWEEN_MOON_CHECKS
   completed sessions, before the NEXT cast: if you chose to keep a
   time of day at startup, check for the icon (moon or sun) marking
   the OTHER time of day, press 5 then click near screen center a few
   times if it's there, then always wait a full
   MOON_CHECK_PRE_CAST_DELAY_SECONDS (1s) before that next cast starts
   regardless of whether the icon was actually found. Then go back to
   step 1. Forever, until you hit ESC.

CONTROL LOGIC (simple, on purpose)
------------------------------------
No history, no velocity, no prediction. Every loop iteration: find the
control bar and the pin/line fresh, from scratch, within the captured
region (the whole bar).
  - control bar/pin not found yet (before first appearance, or a
    mid-check detection blip) -> jitter-click on the keep-alive
    cadence so position is never just left to drift while blind.
  - pin at/left of the control bar's dead center -> do nothing this
    frame, let the constant leftward pull carry the bar back down
    until it's centered over the pin again.
  - pin at/right of center -> fire one short, fixed-length tap (never
    a longer hold, no matter how far right it is), then immediately
    re-read the next frame and decide again.
  - either way, if it's been more than KEEPALIVE_TAP_INTERVAL since
    the last tap, tap anyway — the leftward pull would otherwise keep
    accelerating unchecked while coasting.
The tap length is constant and very short — distance doesn't change
how long it holds, only whether it taps at all, and the loop never
sleeps except for that one short tap, so it reacts as fast as the
screen-capture + detection pipeline allows.

pip install mss numpy opencv-python pynput

macOS: grant Screen Recording + Accessibility to your terminal app in
System Settings -> Privacy & Security, then restart the terminal.

CONTROLS
--------
F8  = pause/resume the whole loop (safe to hit anytime)
ESC = quit immediately (always releases the mouse/key first)
"""

import os
import random
import time
import numpy as np
import cv2
import mss
from pynput import keyboard as pynput_keyboard
from pynput.mouse import Controller as MouseController, Button

try:
    from AppKit import NSWorkspace  # only available on macOS w/ pyobjc
except Exception:
    NSWorkspace = None

# ----------------------------------------------------------------------
# CONFIG
# ----------------------------------------------------------------------

########################################################################
# TARGET APP / SCREEN REGION
########################################################################
TARGET_APP_NAME = "Roblox"

REGION = {"left": 426, "top": 785, "width": 589, "height": 35}
########################################################################

########################################################################
# DETECTION — HSV color ranges for the control bar and the pin/line
########################################################################
# The control bar isn't always the same color — it goes bright/white
# normally, but shifts to a dark, slightly transparent color somewhere
# between maroon/brown and dark green when the pin is off it. That's a
# wide, continuous gradient rather than one fixed color, so it's
# covered here with two generous ranges (low-hue maroon/brown, and
# higher-hue dark green) rather than one guessed color. Detection
# unions ALL of these ranges, so add more (low, high) pairs here as you
# find other color states. These two are still a best-effort guess,
# not calibrated — when the color-debug log prints real HSV numbers
# (see COLOR_DEBUG_PRINT_INTERVAL / sample_unrecognized_color), narrow
# these down to match.
CONTROLLED_BAR_HSV_RANGES = [
    (np.array([0, 0, 150]), np.array([180, 40, 255])),     # normal: bright/white
    (np.array([0, 15, 20]), np.array([25, 180, 150])),     # guess: maroon/brown (low hue) — CALIBRATE THIS
    (np.array([35, 15, 20]), np.array([90, 180, 150])),    # guess: dark green (higher hue) — CALIBRATE THIS
]

# The needle is drawn ON TOP of the bar and can span the bar's full
# height, which visually splits the bar's own color into two separate
# blobs (one on each side of the needle). Bar detection bridges gaps
# between same-color blobs that are at most this many pixels apart
# (see find_bar_x_bounds_from_mask), so the two needle-split halves get
# reunited into the bar's true, stable span instead of bar_left/
# bar_right silently shifting depending on wherever the needle happens
# to be -- which is what was actually causing the bar to look "stuck"/
# untracked. Deliberately a PROXIMITY merge (bridge only NARROW gaps),
# not a blanket union of every matching blob in frame: CONTROLLED_BAR_
# HSV_RANGES includes broad, uncalibrated guesses (maroon/green) that
# can also match unrelated background scenery elsewhere in frame, and
# a blanket union would silently absorb that into the bar's bounds too
# -- tried that first, and a sandbox test confirmed it does exactly
# that. Set well above the needle's expected width, well below the
# likely distance to an unrelated background match.
BAR_MERGE_MAX_GAP_PX = 40

# Blobs smaller than this fraction of the frame's total area are
# treated as noise and ignored entirely (not even used to bridge a
# gap), so a handful of stray matching pixels can't do anything.
BAR_MIN_BLOB_AREA_FRACTION = 0.01

# NOTE: there used to be a BAR_MIN_WIDTH_FRACTION safety net here that
# rejected any detected bar span narrower than 10% of REGION's width,
# on the theory that a real bar would always be wide. Removed: the
# control bar's width isn't constant -- it can genuinely shrink during
# play -- so a fixed width floor risked rejecting a legitimately
# narrow real bar as "not found," which made tracking worse rather
# than better. The actual fix for background falsely matching the
# bar's colors is calibrating CONTROLLED_BAR_HSV_RANGES with real
# sampled data (see [color debug] console output), not a width guess.

# The needle/pin isn't always one color either -- it shows up as a
# blue/purple hue sometimes, and as violet/magenta (confirmed from a
# screenshot) other times. Same approach as the control bar above:
# union multiple ranges instead of assuming one fixed color, and add
# more (low, high) pairs here as more color states turn up -- but ONLY
# once confirmed via the [needle debug] console line's real HSV
# numbers. A broad, unconfirmed "plain grey" guess used to be a third
# entry here; it mathematically overlapped the bar's own dim maroon/
# green "pin is off it" colors (H0-25/35-90, S15-40, V60-140 falls
# inside both), and since detection always keeps the BIGGEST matching
# contour, that let the needle detector lock onto the (much bigger)
# bar itself during completely normal play, corrupting the error
# calculation. Removed rather than narrowed, since it was never
# actually validated against a real sample in the first place.
NEEDLE_HSV_RANGES = [
    (np.array([95, 41, 40]), np.array([125, 150, 255])),    # blue/purple -- V was capped at 160,
                                                             # but real [needle debug] samples showed
                                                             # this color reaching V~249, which is why
                                                             # needle detection was failing on a huge
                                                             # fraction of frames. S-min raised from 20
                                                             # to 41 to match (every real sample showed
                                                             # S>=41, none lower) -- naively widening V
                                                             # alone would overlap the bar's white range
                                                             # (S0-40, V150-255); confirmed by computing
                                                             # the overlap directly. S=41 sits exactly
                                                             # above that ceiling.
    (np.array([126, 60, 90]), np.array([179, 220, 220])),   # violet/magenta -- widened to H179 per
                                                             # real [needle debug] samples (was capped
                                                             # at 150); doesn't overlap any bar range
]

# Real [color debug] samples showed the bar's own dim/"pin not on it"
# tint sits at roughly H103-153, S25-243, V47-248 -- overlapping (or
# right at the edge of) these needle ranges, at least for some
# saturation/value combinations. Since the bar is much WIDER than the
# thin needle line, a needle-range match that's wider than this many
# pixels is almost certainly the wide dim bar getting picked up
# instead of the real needle -- reject it (fall back to the next-
# biggest qualifying match, or None) rather than silently returning
# the bar's own bounds mislabeled as "the needle".
NEEDLE_MAX_WIDTH_PX = 30
########################################################################

########################################################################
# REACTIVE CONTROL LOOP — tap timing + where to hold the pin
########################################################################
# Opening sequence uses a real mouse click-and-hold; the control loop
# (once the bar is up) uses a held key instead, since it needs to fire
# many rapid pulses.
HOLD_KEY = pynput_keyboard.Key.space

# Where along the control bar's width the pin should be held, as a
# fraction from the left edge (0.0) to the right edge (1.0). 0.5 would
# be dead center. This isn't a proportional controller that settles
# exactly on the setpoint -- it's bang-bang (burst-tap right, do
# nothing and let real momentum pull left) on a real physics system,
# so it INHERENTLY oscillates biased to the right of wherever it aims,
# never converging exactly on PIN_TARGET_FRACTION itself. This is a
# separate issue from the (now-fixed) bar-bounds corruption bug --
# aiming left of center is how a bang-bang controller like this one
# has to compensate for its own one-sided actuation, independent of
# whether detection itself is accurate. Confirmed by testing: setting
# this to 1/2 reproduced exactly the "oscillates, never settles"
# symptom this comment already predicted, even with correct bounds.
PIN_TARGET_FRACTION = 1 / 4

# SIMPLE CONTROL RULE:
#   pin left of (or at) the target point -> do nothing this frame,
#     UNLESS it's been more than KEEPALIVE_TAP_INTERVAL since the last
#     tap — the leftward pull otherwise keeps accelerating unchecked.
#   pin right of (or at) the target point -> tap for a fixed, constant
#     duration (never scaled by distance), then re-check next frame.
# The same keep-alive cadence is also what's used to jitter-click
# before the whole bar has appeared at all — see run_control_until_gone().
TAP_SECONDS = 0.05  # length of each tap — bumped up from 0.02: it was tapping every single
                    # iteration (error stayed positive) but each tap was too short to
                    # out-push the constant leftward acceleration, so it kept losing
                    # ground and got stuck at the left edge despite tapping constantly
KEEPALIVE_TAP_INTERVAL = 0.2  # max time allowed between taps, even while "coasting"

# The game itself only updates at 60fps, so re-deciding whether to tap
# far faster than that just spends CPU re-evaluating essentially the
# same on-screen state. Bar/needle detection still runs every single
# loop iteration regardless (needed to stay responsive about "is the
# bar gone"), but the actual tap-or-not decision only re-evaluates at
# most this often -- roughly every 3 of the game's own frames. This is
# real elapsed time, not an actual frame counter, since our loop
# doesn't run in lockstep with Roblox's renderer.
GAME_FPS = 60
DECISION_FRAME_SKIP = 3
DECISION_INTERVAL_SECONDS = DECISION_FRAME_SKIP / GAME_FPS

# How long the bar AND needle must BOTH stay undetected in a row
# (after having been seen at least once) before deciding the stage has
# actually ended, vs. a one-frame blip. Time-based rather than
# frame-count-based, so it's a fixed real-world 2 seconds no matter
# how fast the capture/detect loop happens to be running -- a
# frame-count version of this is what originally caused an ~8s stuck
# space bar, since a fixed frame count silently assumes a roughly
# constant loop rate. Requiring BOTH bar and needle missing (not just
# one) means a one-sided color-detection blip mid-fight can't trip
# this -- this reuses the same bar/needle detection already computed
# every iteration for tapping, so it costs nothing extra to check.
MISSING_GRACE_SECONDS = 2.0

# The bar's own PHYSICAL position doesn't move -- only its color tint
# toggles between white (pin currently on it) and a dim, hard-to-
# calibrate tint (pin off it), per direct user observation. Widening
# CONTROLLED_BAR_HSV_RANGES to also catch that dim tint was tried and
# a sandbox test confirmed it corrupts needle detection (the needle's
# own ranges overlap that same dim color, so the needle detector locks
# onto the much-wider bar instead). So the bar's position is instead
# cached from the last frame it WAS seen white, and reused for that
# many seconds while it's currently dim -- long enough to bridge the
# stretches where the pin isn't on it (which, per the same
# observation, is most of the time), short enough that if the bar is
# genuinely gone for good, this cache expires well before
# MISSING_GRACE_SECONDS and "is it over" detection (which uses the RAW
# detection, not this cache) isn't delayed by it.
BAR_CACHE_MAX_AGE_SECONDS = 1.5

# How often frontmost_app_ok() actually re-queries the OS (via a real
# ObjC/WindowServer round-trip) vs. reusing the last answer. That round
# trip was being made on every single loop iteration and occasionally
# stalls for a noticeable moment — throttling it removes those pauses
# from the hot control loop.
FOCUS_CHECK_INTERVAL = 0.5

# How often (at most) to print a color-sample when the control bar
# can't be found, so you can read off real HSV values for
# CONTROLLED_BAR_HSV_RANGES straight from the console instead of using
# a separate tool.
COLOR_DEBUG_PRINT_INTERVAL = 1.0
########################################################################

########################################################################
# CAST SEQUENCE — 9, 1, hold-click, then a burst of Enters
########################################################################
# Cast sequence timings
# macOS virtual keycodes (US ANSI layout) for the number row. Using an
# explicit vk forces pynput to send a real raw key-code event — plain
# character strings ("9", "1") can silently fall back to a unicode-text
# insert on macOS, which has no keyCode and games like Roblox (reading
# raw keyCodes for hotbar shortcuts) just ignore.
_VK_9 = 0x19
_VK_1 = 0x12
# Same reasoning applies to Enter — sending it via an explicit vk
# removes any doubt about whether it's registering, same as 9/1 above.
_VK_RETURN = 0x24
PRE_TRIGGER_KEY_SEQUENCE = [
    (pynput_keyboard.KeyCode(vk=_VK_9), "9"),
    (pynput_keyboard.KeyCode(vk=_VK_1), "1"),
]
PRE_TRIGGER_KEY_HOLD_SECONDS = 0.12  # bumped up from 0.05 — needs longer than Enter/Space did
PRE_TRIGGER_KEY_DELAY = 0.5  # delay after each key in the sequence above
TRIGGER_HOLD_SECONDS = 1.0
TRIGGER_ENTER_COUNT = 12
TRIGGER_ENTER_INTERVAL = 0.1
TRIGGER_ENTER_HOLD_SECONDS = 0.15  # bumped up from 0.05 -- same fix as TAP_SECONDS: too
                                   # short a hold wasn't registering reliably
POST_HOLD_ENTER_DELAY_SECONDS = 0.5  # pause after the click-hold, before the Enter presses start
########################################################################

########################################################################
# LOOP PACING / SAFETY TIMEOUTS
########################################################################
# After the whole bar disappears, wait this long before casting again.
POST_CHECK_DELAY_SECONDS = 1.0

# If the reactive control loop (jitter-clicking + tapping HOLD_KEY) runs
# longer than this without the whole bar ever clearing on its own,
# force it to give up and recast instead of holding forever on a
# stuck/missed check.
CONTROL_TIMEOUT_SECONDS = 25.0
########################################################################

########################################################################
# DAY/NIGHT CHECK — moon icon (night) and sun icon (day), same spot
########################################################################
# Day/night cycle: every this-many completed cast+control sessions,
# check for whichever icon (moon or sun — see the startup prompt) right
# before the next cast. Detection + the 5/click action were validated
# separately in moon_test.py before being wired in here.
SESSIONS_BETWEEN_MOON_CHECKS = 5

# The two corners pointed at on screen for the check. Both the moon and
# sun icons show up in the same spot (just one or the other, depending
# on the time of day), so this one region/box is shared by both. The
# box they define is smaller than the actual icon artwork (once its
# transparent padding is cropped off), so a margin is added around it
# below to make sure the icon fits inside the searched region
# regardless of a few pixels of drift.
MOON_CHECK_CORNER_A = (1430, 848)
MOON_CHECK_CORNER_B = (1316, 811)
MOON_CHECK_MARGIN = 40

# cv2.matchTemplate (TM_CCOEFF_NORMED) score cutoff to call it "yes,
# that's the icon." 1.0 = pixel-perfect match. This is the plain
# unmasked match — confirmed working in moon_test.py.
MOON_MATCH_THRESHOLD = 0.6

MOON_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon.png")
SUN_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sun.png")

# After the check (whether or not the icon was actually found), wait
# this long before the next cast starts.
MOON_CHECK_PRE_CAST_DELAY_SECONDS = 1.0

# macOS virtual keycode for '5', same reasoning as the vk usage above:
# raw keyCode events are what Roblox hotbar shortcuts expect.
_VK_5 = 0x17
MOON_ACTION_KEY_HOLD_SECONDS = 0.1  # how long each key-down is held before release

# A plain Space tap after pressing 5 wasn't being recognized -- what's
# actually needed is a real mouse click-and-hold near screen center,
# repeated a few times with a random position jitter each time (rather
# than the exact same pixel every time) in case dead center isn't
# clickable for whatever's on screen at that moment.
MOON_ACTION_CLICK_COUNT = 4
MOON_ACTION_CLICK_HOLD_SECONDS = 0.2
MOON_ACTION_CLICK_BREAK_SECONDS = 0.5
MOON_ACTION_CLICK_JITTER_PX = 50  # max random offset from center, in each direction
########################################################################

# ----------------------------------------------------------------------

mouse = MouseController()
kb = pynput_keyboard.Controller()

_paused = False
_stopped = False


def on_press(key):
    global _paused, _stopped
    if key == pynput_keyboard.Key.f8:
        _paused = not _paused
        print("\nPAUSED" if _paused else "\nRESUMED")
    elif key == pynput_keyboard.Key.esc:
        _stopped = True
        print("\nStopping...")


listener = pynput_keyboard.Listener(on_press=on_press)
listener.start()


_focus_cache = {"ok": True, "checked_at": 0.0}


def frontmost_app_ok():
    """Cached — the real ObjC/WindowServer query only actually runs at
    most every FOCUS_CHECK_INTERVAL seconds; focus doesn't change fast
    enough to need checking on every single loop iteration, and that
    round-trip was the source of periodic stalls in the hot loop."""
    now = time.time()
    if now - _focus_cache["checked_at"] < FOCUS_CHECK_INTERVAL:
        return _focus_cache["ok"]

    ok = True
    if NSWorkspace is not None:
        try:
            app = NSWorkspace.sharedWorkspace().frontmostApplication()
            name = app.localizedName() if app else ""
            ok = TARGET_APP_NAME.lower() in name.lower()
        except Exception:
            ok = True  # if the check itself fails, don't block on it

    _focus_cache["ok"] = ok
    _focus_cache["checked_at"] = now
    return ok


def find_x_bounds_from_mask(mask, max_width_px=NEEDLE_MAX_WIDTH_PX):
    """Bounding box of the biggest connected blob in the mask that's
    NOT wider than max_width_px. Used for the needle: its own color
    ranges can overlap the bar's dim "pin not on it" tint (confirmed
    from real samples), so if the biggest match is implausibly wide to
    be the thin needle line, it's almost certainly the wide bar
    getting picked up instead -- skip it and try the next-biggest
    candidate, rather than silently returning the bar's bounds
    mislabeled as "the needle"."""
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None
    for c in sorted(contours, key=cv2.contourArea, reverse=True):
        x, _, w, _ = cv2.boundingRect(c)
        if w <= max_width_px:
            return x, x + w, x + w / 2
    return None


def find_bar_x_bounds_from_mask(mask, min_area_fraction=BAR_MIN_BLOB_AREA_FRACTION,
                                 max_gap_px=BAR_MERGE_MAX_GAP_PX):
    """Bounding box of the bar, bridging small gaps between
    significant blobs (see BAR_MERGE_MAX_GAP_PX) instead of just
    keeping the single biggest one. The needle is visually drawn on
    top of the bar and can span its full height, which severs the
    bar's own color mask into two separate blobs (one on each side of
    the needle) -- picking just the biggest of those two silently
    throws away the other half of the bar's true extent, corrupting
    bar_left/bar_right depending on whichever side the needle happens
    to be nearer to at that instant. This is what was actually causing
    "stuck to one side, not tracking": bar_target kept getting
    recomputed from a half-bar that shifted underneath the needle
    instead of the bar's real, stable span.
    Only NARROW gaps get bridged (not a blanket union of every
    matching blob in frame) -- an unrelated background match far away
    stays separate and gets excluded, since the largest resulting
    merged group is what's returned as "the bar"."""
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None
    min_area = min_area_fraction * mask.shape[0] * mask.shape[1]
    significant = [c for c in contours if cv2.contourArea(c) >= min_area]
    if not significant:
        significant = [max(contours, key=cv2.contourArea)]

    spans = []
    for c in significant:
        x, _, w, _ = cv2.boundingRect(c)
        spans.append([x, x + w])
    spans.sort()

    merged = [spans[0]]
    for left, right in spans[1:]:
        if left - merged[-1][1] <= max_gap_px:
            merged[-1][1] = max(merged[-1][1], right)
        else:
            merged.append([left, right])

    left, right = max(merged, key=lambda span: span[1] - span[0])
    return left, right, left + (right - left) / 2


def hsv_union_mask(hsv, hsv_ranges):
    mask = None
    for low, high in hsv_ranges:
        m = cv2.inRange(hsv, low, high)
        mask = m if mask is None else cv2.bitwise_or(mask, m)
    return mask


def find_x_bounds_multi(hsv, hsv_ranges):
    """Same as find_x_bounds, but unions the masks of multiple (low, high)
    HSV ranges — for things that can show up as more than one color."""
    return find_x_bounds_from_mask(hsv_union_mask(hsv, hsv_ranges))


def find_bar_x_bounds_multi(hsv, hsv_ranges):
    """Same idea as find_x_bounds_multi, but for the bar specifically:
    uses find_bar_x_bounds_from_mask (proximity-merged blobs) so the
    needle occluding/splitting the bar doesn't corrupt its bounds."""
    return find_bar_x_bounds_from_mask(hsv_union_mask(hsv, hsv_ranges))


def grab_region_normalized(sct, region):
    """Grabs `region` and, if the display is HiDPI/Retina, resizes the
    result back down to region's declared width/height. mss captures
    at the screen's actual PHYSICAL pixel resolution, which on a
    Retina display is 2x the logical/point coordinates `region` is
    specified in -- confirmed directly from real runs, where detected
    bar/needle x-coordinates were reaching ~1170 despite REGION only
    declaring a 589px-wide capture (1170 ~= 589 * 2). Every pixel-based
    constant in this file (BAR_MERGE_MAX_GAP_PX, NEEDLE_MAX_WIDTH_PX,
    etc.) was tuned assuming region's declared size, so normalizing
    the frame back to that size here means none of them need to be
    rescaled individually -- this is the one place the mismatch
    actually needs fixing."""
    frame = np.asarray(sct.grab(region))
    if frame.shape[1] != region["width"] or frame.shape[0] != region["height"]:
        frame = cv2.resize(frame, (region["width"], region["height"]), interpolation=cv2.INTER_AREA)
    return frame


def grab_frame(sct):
    return grab_region_normalized(sct, REGION)  # BGRA — this is "the whole bar"


def detect_both(frame):
    """Within the captured whole-bar frame, finds the control bar and
    the pin/line."""
    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)  # cv2 has no direct BGRA->HSV code
    hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)  # convert once, reuse for both masks
    bar = find_bar_x_bounds_multi(hsv, CONTROLLED_BAR_HSV_RANGES)
    needle = find_x_bounds_multi(hsv, NEEDLE_HSV_RANGES)
    return bar, needle, hsv


def sample_unrecognized_color(hsv):
    """When the control bar can't be found by any known range, sample
    whatever colorful (non-background, non-pin) blob is on screen so
    its real HSV range can be read straight off the console."""
    colorful = cv2.inRange(hsv, np.array([0, 25, 20]), np.array([180, 255, 255]))
    needle_mask = hsv_union_mask(hsv, NEEDLE_HSV_RANGES)
    unknown = cv2.bitwise_and(colorful, cv2.bitwise_not(needle_mask))
    ys, xs = np.where(unknown > 0)
    if len(xs) == 0:
        return None
    pixels = hsv[ys, xs]
    return tuple(pixels.min(axis=0)), tuple(pixels.max(axis=0))


def sample_unrecognized_needle_color(hsv):
    """When the needle can't be found by any known range (but the bar
    was), sample whatever colorful (non-background, non-bar) blob is
    on screen so its real HSV range can be read straight off the
    console -- same idea as sample_unrecognized_color, just excluding
    the bar's colors instead of the needle's."""
    colorful = cv2.inRange(hsv, np.array([0, 25, 20]), np.array([180, 255, 255]))
    bar_mask = hsv_union_mask(hsv, CONTROLLED_BAR_HSV_RANGES)
    unknown = cv2.bitwise_and(colorful, cv2.bitwise_not(bar_mask))
    ys, xs = np.where(unknown > 0)
    if len(xs) == 0:
        return None
    pixels = hsv[ys, xs]
    return tuple(pixels.min(axis=0)), tuple(pixels.max(axis=0))


def load_icon_template(path, label):
    """Loads an icon PNG (moon or sun) and crops it down to just its
    opaque pixels (it has transparent padding around the actual icon),
    so matchTemplate is comparing against the icon shape itself rather
    than empty margin."""
    img = cv2.imread(path, cv2.IMREAD_UNCHANGED)
    if img is None:
        print(f"[{label}] couldn't load {path} — {label}-check disabled")
        return None
    if img.ndim == 3 and img.shape[2] == 4:
        ys, xs = np.where(img[:, :, 3] > 10)
        if len(xs):
            img = img[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
        img = img[:, :, :3]
    return img


def day_night_check_region():
    (x1, y1), (x2, y2) = MOON_CHECK_CORNER_A, MOON_CHECK_CORNER_B
    left, right = sorted((x1, x2))
    top, bottom = sorted((y1, y2))
    left -= MOON_CHECK_MARGIN
    top -= MOON_CHECK_MARGIN
    right += MOON_CHECK_MARGIN
    bottom += MOON_CHECK_MARGIN
    return {"left": left, "top": top, "width": right - left, "height": bottom - top}


def is_icon_visible(sct, template):
    """True if the given icon template (moon or sun) matches what's
    currently in the shared day/night check region."""
    if template is None:
        return False
    frame = grab_region_normalized(sct, day_night_check_region())
    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)
    th, tw = template.shape[:2]
    if bgr.shape[0] < th or bgr.shape[1] < tw:
        return False
    result = cv2.matchTemplate(bgr, template, cv2.TM_CCOEFF_NORMED)
    _, max_val, _, _ = cv2.minMaxLoc(result)
    return max_val >= MOON_MATCH_THRESHOLD


def tap_vk(vk_code, label, hold_seconds=MOON_ACTION_KEY_HOLD_SECONDS):
    key = pynput_keyboard.KeyCode(vk=vk_code)
    print(f"  pressing '{label}'...")
    kb.press(key)
    time.sleep(hold_seconds)
    kb.release(key)


def run_moon_action(sct):
    """The icon we're watching for (moon or sun, whichever the startup
    prompt says to skip) was spotted on the every-Nth-session check —
    press 5, then click-and-hold near screen center
    MOON_ACTION_CLICK_COUNT times, each at a randomly jittered
    position, holding MOON_ACTION_CLICK_HOLD_SECONDS with a
    MOON_ACTION_CLICK_BREAK_SECONDS break in between."""
    print("\nDay/night icon detected — pressing 5, then clicking...")
    tap_vk(_VK_5, "5")

    monitor = sct.monitors[1]
    center_x = monitor["left"] + monitor["width"] // 2
    center_y = monitor["top"] + monitor["height"] // 2

    for i in range(MOON_ACTION_CLICK_COUNT):
        if _stopped:
            return
        x = center_x + random.randint(-MOON_ACTION_CLICK_JITTER_PX, MOON_ACTION_CLICK_JITTER_PX)
        y = center_y + random.randint(-MOON_ACTION_CLICK_JITTER_PX, MOON_ACTION_CLICK_JITTER_PX)
        print(f"  click {i + 1}/{MOON_ACTION_CLICK_COUNT} at ({x}, {y})...")
        mouse.position = (x, y)
        mouse.press(Button.left)
        time.sleep(MOON_ACTION_CLICK_HOLD_SECONDS)
        mouse.release(Button.left)
        time.sleep(MOON_ACTION_CLICK_BREAK_SECONDS)


def prompt_day_night_preference():
    """Ask which time of day to skip. 'd'/'day' means skip day, so
    watch for the sun icon (day) and skip it whenever it shows up --
    net effect: only night. 'n'/'night' means skip night, so watch for
    the moon icon (night) instead -- net effect: only day. Anything
    else, including just pressing Enter, means skip nothing — the
    whole check stays disabled."""
    answer = input(
        "What do you want to skip? Type d/day to skip day, "
        "n/night to skip night, or press Enter to skip nothing: "
    ).strip().lower()
    if answer in ("d", "day"):
        print("okay, skipping day whenever it's detected")
        return "day"
    if answer in ("n", "night"):
        print("okay, skipping night whenever it's detected")
        return "night"
    print("alright, skipping nothing — day/night check disabled")
    return None


def wait_while_paused_or_unfocused():
    """Blocks (checking for stop) while paused or the target app isn't
    frontmost. Returns False if we should abort entirely (ESC hit)."""
    while not _stopped and (_paused or not frontmost_app_ok()):
        time.sleep(0.05)
    return not _stopped


def run_trigger_sequence():
    """The cast: 9, 1, then a held click, then a burst of Enters."""
    print("Casting...")

    for key, label in PRE_TRIGGER_KEY_SEQUENCE:
        if _stopped:
            return
        print(f"  pressing '{label}'...")
        kb.press(key)
        time.sleep(PRE_TRIGGER_KEY_HOLD_SECONDS)
        kb.release(key)
        time.sleep(PRE_TRIGGER_KEY_DELAY)

    mouse.press(Button.left)
    time.sleep(TRIGGER_HOLD_SECONDS)
    mouse.release(Button.left)

    time.sleep(POST_HOLD_ENTER_DELAY_SECONDS)

    enter_key = pynput_keyboard.KeyCode(vk=_VK_RETURN)
    for i in range(TRIGGER_ENTER_COUNT):
        if _stopped:
            return
        kb.press(enter_key)
        time.sleep(TRIGGER_ENTER_HOLD_SECONDS)
        kb.release(enter_key)
        time.sleep(TRIGGER_ENTER_INTERVAL)


def run_control_until_gone(sct):
    """Runs immediately after casting. Jitter-clicks to hold position
    from the very first frame onward, whether or not the whole bar has
    appeared yet. Once the control bar/pin are actually seen, switches
    to reacting to the pin's real position -- bar/needle are detected
    fresh every single loop iteration, but the actual tap-or-not
    decision only re-evaluates every DECISION_INTERVAL_SECONDS (real
    elapsed time, not a frame counter), since the game itself only
    updates at GAME_FPS and reacting faster than that just re-decides
    on the same on-screen state. Since the bar only shows up as a
    recognizable color while the pin happens to be on it (its dim
    "pin off it" tint isn't safely distinguishable from the needle's
    own colors -- see NEEDLE_MAX_WIDTH_PX), its position is cached
    from the last frame it WAS seen and reused for BAR_CACHE_MAX_AGE_
    SECONDS while it's currently undetected, so reactive control keeps
    working through the dim stretches instead of falling back to blind
    jitter-clicking every time. This continues until bar AND needle are
    BOTH undetected in the RAW (uncached) detection together for
    MISSING_GRACE_SECONDS in a row (real elapsed time, so it's a fixed
    2 seconds regardless of loop speed), at which point the check is
    over -- disappearance always checks the raw detection, never the
    cache, so a stale cached bar position can't delay noticing the
    stage actually ended. A one-sided blip -- just the bar or just the
    needle missing, not both -- doesn't count toward that either, since
    color detection misses one side sometimes mid-fight without the
    stage actually being over. Disappearance is only ever evaluated
    once the bar has been seen at least once, so "hasn't appeared yet"
    can never be mistaken for "already over"."""
    print("Jitter-clicking while waiting for the whole bar to appear...")
    bar_seen = False
    missing_since = None
    cached_bar = None
    cached_bar_time = 0.0
    last_tap_time = time.time()
    last_color_debug_time = 0.0
    last_decision_time = 0.0  # 0.0 so the very first reactive-control iteration decides right away
    last_error = 0.0
    start_time = time.time()

    frame_count = 0
    fps_timer = time.time()
    grab_time_total = 0.0
    detect_time_total = 0.0
    # Hz/timing are only meaningful averaged over ~1s, but the position/
    # click-state line below prints every tick -- these hold the most
    # recently computed averages so every tick's print can show them
    # without recomputing (or needing to wait for) a fresh 1s window.
    last_loop_hz = 0
    last_avg_grab_ms = 0.0
    last_avg_detect_ms = 0.0
    last_tick_start = 0.0  # 0.0 so the very first iteration never waits

    try:
        while not _stopped:
            if time.time() - start_time >= CONTROL_TIMEOUT_SECONDS:
                print(f"\n{CONTROL_TIMEOUT_SECONDS:.0f}s of holding elapsed without the bar "
                      f"clearing — forcing a restart.")
                return

            if _paused or not frontmost_app_ok():
                kb.release(HOLD_KEY)
                if not wait_while_paused_or_unfocused():
                    return
                continue

            # Pace the whole loop to a fixed cadence (DECISION_INTERVAL_
            # SECONDS, a ratio of GAME_FPS) rather than free-running as
            # fast as capture allows. This doesn't make capture itself
            # any faster -- a real OS-level stall still takes as long as
            # it takes -- but it makes the RATE we attempt to sample at
            # even and predictable instead of jittery, which is what
            # actually matters for reading the [track] output: any tick
            # that still comes out late despite this is unambiguously a
            # real capture stall, not just normal timing noise.
            since_last_tick = time.time() - last_tick_start
            if since_last_tick < DECISION_INTERVAL_SECONDS:
                time.sleep(DECISION_INTERVAL_SECONDS - since_last_tick)
            last_tick_start = time.time()

            now = time.time()
            t0 = now
            frame = grab_frame(sct)
            t1 = time.time()
            bar, needle, hsv = detect_both(frame)
            t2 = time.time()
            grab_time_total += t1 - t0
            detect_time_total += t2 - t1

            if bar is not None:
                cached_bar = bar
                cached_bar_time = now

            if bar is None and needle is None:
                if bar_seen:
                    if missing_since is None:
                        missing_since = now
                    elif now - missing_since >= MISSING_GRACE_SECONDS:
                        print("Bar and needle both gone — check complete.")
                        return

                # Not appeared yet, or within the grace window after it
                # ended — either way keep jitter-clicking on the
                # keep-alive cadence so position is never just left to
                # drift while blind.
                if now - last_tap_time >= KEEPALIVE_TAP_INTERVAL:
                    kb.press(HOLD_KEY)
                    time.sleep(TAP_SECONDS)
                    kb.release(HOLD_KEY)
                    last_tap_time = time.time()
                    clicked = True
                else:
                    kb.release(HOLD_KEY)
                    clicked = False
                print(f"[track] bar=none  needle=none  {'CLICK' if clicked else 'idle '}    ", end="\r")
                continue

            missing_since = None  # at least one of bar/needle visible -- not "both gone"

            # The bar's own color can be genuinely ambiguous while it's
            # dim (pin not currently on it) -- if it's not detected raw
            # this frame but we recently saw it (white, pin was on it),
            # reuse that cached position instead of losing reactive
            # control entirely. This does NOT affect the "both gone"
            # check above, which only ever looks at the raw detection.
            effective_bar = bar
            if effective_bar is None and cached_bar is not None \
                    and now - cached_bar_time <= BAR_CACHE_MAX_AGE_SECONDS:
                effective_bar = cached_bar

            if effective_bar is None or needle is None:
                # Genuinely can't act this frame: no fresh bar (raw or
                # cached), or needle missing -- jitter-click keep-alive,
                # doesn't count toward "gone" on its own.
                if now - last_color_debug_time >= COLOR_DEBUG_PRINT_INTERVAL:
                    if bar is None:
                        sample = sample_unrecognized_color(hsv)
                        if sample is not None:
                            print(f"\n[color debug] control bar not recognized — unknown blob "
                                  f"HSV range: low={sample[0]} high={sample[1]}")
                    else:
                        sample = sample_unrecognized_needle_color(hsv)
                        if sample is not None:
                            print(f"\n[needle debug] needle not recognized — unknown blob "
                                  f"HSV range: low={sample[0]} high={sample[1]}")
                    last_color_debug_time = now

                if now - last_tap_time >= KEEPALIVE_TAP_INTERVAL:
                    kb.press(HOLD_KEY)
                    time.sleep(TAP_SECONDS)
                    kb.release(HOLD_KEY)
                    last_tap_time = time.time()
                    clicked = True
                else:
                    kb.release(HOLD_KEY)
                    clicked = False
                bar_str = f"{effective_bar[0]:.0f}-{effective_bar[1]:.0f}" if effective_bar else "none"
                needle_str = f"{needle[0]:.0f}-{needle[1]:.0f}" if needle else "none"
                print(f"[track] bar={bar_str}  needle={needle_str}  "
                      f"{'CLICK' if clicked else 'idle '}    ", end="\r")
                continue

            if not bar_seen:
                print("Whole bar appeared — engaging reactive control.")
            bar_seen = True

            # Bar/needle are detected fresh every iteration above (so
            # "is the bar gone" stays responsive), but the game itself
            # only updates at GAME_FPS -- re-deciding whether to tap
            # faster than that just spends CPU re-evaluating the same
            # on-screen state, so the actual tap decision is throttled
            # to DECISION_INTERVAL_SECONDS.
            if now - last_decision_time >= DECISION_INTERVAL_SECONDS:
                last_decision_time = now
                bar_left, bar_right, _ = effective_bar
                _, _, needle_center = needle
                bar_target = bar_left + PIN_TARGET_FRACTION * (bar_right - bar_left)

                last_error = needle_center - bar_target

                # error can never legitimately exceed REGION's width --
                # both bar_target and needle_center are x-coordinates
                # within the same captured frame. If it does, something
                # is producing a bad reading (e.g. the needle's width
                # cap falling back to a spurious noise blob) -- print
                # the raw numbers so the actual cause is visible instead
                # of silently acting on garbage.
                if abs(last_error) > REGION["width"]:
                    print(f"\n[error debug] impossible error={last_error:.0f}px -- "
                          f"bar={'cached' if bar is None else 'fresh'} "
                          f"bar_left={bar_left:.0f} bar_right={bar_right:.0f} "
                          f"bar_target={bar_target:.0f} needle_center={needle_center:.0f} "
                          f"needle_bounds={needle[0]}-{needle[1]}")

                clicked = False
                if last_error >= 0 or (now - last_tap_time) >= KEEPALIVE_TAP_INTERVAL:
                    # pin at/right of the target point -> tap. Also tap
                    # on a keep-alive timer even when left of it, so
                    # the leftward pull never gets long enough to keep
                    # accelerating.
                    kb.press(HOLD_KEY)
                    time.sleep(TAP_SECONDS)
                    kb.release(HOLD_KEY)
                    last_tap_time = time.time()
                    clicked = True

                print(f"[track] bar={bar_left:.0f}-{bar_right:.0f} (target={bar_target:.0f})  "
                      f"needle={needle[0]:.0f}-{needle[1]:.0f} (center={needle_center:.0f})  "
                      f"error={last_error:+.0f}px  {'CLICK' if clicked else 'idle '}  "
                      f"[{last_loop_hz}Hz grab={last_avg_grab_ms:.0f}ms detect={last_avg_detect_ms:.0f}ms]   ",
                      end="\r")

            frame_count += 1
            now = time.time()
            if now - fps_timer >= 1.0:
                last_loop_hz = frame_count
                last_avg_grab_ms = (grab_time_total / frame_count * 1000) if frame_count else 0.0
                last_avg_detect_ms = (detect_time_total / frame_count * 1000) if frame_count else 0.0
                frame_count = 0
                fps_timer = now
                grab_time_total = 0.0
                detect_time_total = 0.0
    finally:
        # Guaranteed release no matter which return path above is hit —
        # the bar being gone must never leave the key held.
        kb.release(HOLD_KEY)


def main():
    day_night_preference = prompt_day_night_preference()

    for i in range(3, 0, -1):
        print(i)
        time.sleep(1)

    print("Autonomous bar bot running. F8 = pause/resume, ESC = quit.")

    # Only load the one icon template we actually need to watch for --
    # if skipping nothing (preference is None), never load either, so
    # the periodic check below never has anything to trigger on.
    watch_template = None
    if day_night_preference == "day":
        watch_template = load_icon_template(SUN_TEMPLATE_PATH, "sun")
    elif day_night_preference == "night":
        watch_template = load_icon_template(MOON_TEMPLATE_PATH, "moon")

    session_count = 0

    with mss.mss() as sct:
        while not _stopped:
            if not wait_while_paused_or_unfocused():
                break

            if day_night_preference is not None and session_count >= SESSIONS_BETWEEN_MOON_CHECKS:
                session_count = 0
                if is_icon_visible(sct, watch_template):
                    run_moon_action(sct)
                # Full delay before the cast either way -- whether or
                # not the icon was actually found this time.
                time.sleep(MOON_CHECK_PRE_CAST_DELAY_SECONDS)

            run_trigger_sequence()
            if _stopped:
                break

            run_control_until_gone(sct)
            session_count += 1

            slept = 0.0
            while slept < POST_CHECK_DELAY_SECONDS and not _stopped:
                time.sleep(0.1)
                slept += 0.1
            # loop back around: recast

    kb.release(HOLD_KEY)
    print("Stopped.")


if __name__ == "__main__":
    main()
