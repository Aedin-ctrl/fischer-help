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
   completed sessions, before the NEXT cast: check for the moon icon,
   press 5 then Space if it's there, then always wait a full
   MOON_CHECK_PRE_CAST_DELAY_SECONDS (1s) before that next cast starts
   regardless of whether the moon was actually found. Then go back to
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

NEEDLE_HSV_LOW = np.array([95, 20, 40])
NEEDLE_HSV_HIGH = np.array([125, 150, 160])
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
# so it oscillates around a point biased by that asymmetry rather than
# converging exactly on PIN_TARGET_FRACTION itself. 3/8 was still
# landing right-of-center in practice, so aiming further left (1/4) to
# compensate for that bias.
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

# How many consecutive "not found" frames after the whole bar has
# already been seen once before we decide the check has actually ended
# (vs. a one-frame detection blip).
MISSING_GRACE_FRAMES = 15

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
# DAY/NIGHT MOON CHECK
########################################################################
# Day/night cycle: every this-many completed cast+control sessions,
# check for the moon icon right before the next cast (see MOON CHECK
# below). Detection + the 5/Space action were validated separately in
# moon_test.py before being wired in here.
SESSIONS_BETWEEN_MOON_CHECKS = 5

# The two corners pointed at on screen for the moon check. The box they
# define is smaller than the actual moon icon artwork (once its
# transparent padding is cropped off), so a margin is added around it
# below to make sure the icon fits inside the searched region
# regardless of a few pixels of drift.
MOON_CHECK_CORNER_A = (1430, 848)
MOON_CHECK_CORNER_B = (1316, 811)
MOON_CHECK_MARGIN = 40

# cv2.matchTemplate (TM_CCOEFF_NORMED) score cutoff to call it "yes,
# that's the moon." 1.0 = pixel-perfect match. This is the plain
# unmasked match — confirmed working in moon_test.py.
MOON_MATCH_THRESHOLD = 0.6

MOON_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon.png")

# After the moon check (whether or not the moon was actually found),
# wait this long before the next cast starts.
MOON_CHECK_PRE_CAST_DELAY_SECONDS = 1.0

# macOS virtual keycode for '5', same reasoning as the vk usage above:
# raw keyCode events are what Roblox hotbar shortcuts expect.
_VK_5 = 0x17
MOON_ACTION_KEY_HOLD_SECONDS = 0.1  # how long each key-down is held before release
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


def find_x_bounds_from_mask(mask):
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None
    biggest = max(contours, key=cv2.contourArea)
    x, _, w, _ = cv2.boundingRect(biggest)
    return x, x + w, x + w / 2


def find_x_bounds(hsv, hsv_low, hsv_high):
    return find_x_bounds_from_mask(cv2.inRange(hsv, hsv_low, hsv_high))


def find_x_bounds_multi(hsv, hsv_ranges):
    """Same as find_x_bounds, but unions the masks of multiple (low, high)
    HSV ranges — for things that can show up as more than one color."""
    mask = None
    for low, high in hsv_ranges:
        m = cv2.inRange(hsv, low, high)
        mask = m if mask is None else cv2.bitwise_or(mask, m)
    return find_x_bounds_from_mask(mask)


def grab_frame(sct):
    return np.asarray(sct.grab(REGION))  # BGRA — this is "the whole bar"


def detect_both(frame):
    """Within the captured whole-bar frame, finds the control bar and
    the pin/line."""
    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)  # cv2 has no direct BGRA->HSV code
    hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)  # convert once, reuse for both masks
    bar = find_x_bounds_multi(hsv, CONTROLLED_BAR_HSV_RANGES)
    needle = find_x_bounds(hsv, NEEDLE_HSV_LOW, NEEDLE_HSV_HIGH)
    return bar, needle, hsv


def sample_unrecognized_color(hsv):
    """When the control bar can't be found by any known range, sample
    whatever colorful (non-background, non-pin) blob is on screen so
    its real HSV range can be read straight off the console."""
    colorful = cv2.inRange(hsv, np.array([0, 25, 20]), np.array([180, 255, 255]))
    needle_mask = cv2.inRange(hsv, NEEDLE_HSV_LOW, NEEDLE_HSV_HIGH)
    unknown = cv2.bitwise_and(colorful, cv2.bitwise_not(needle_mask))
    ys, xs = np.where(unknown > 0)
    if len(xs) == 0:
        return None
    pixels = hsv[ys, xs]
    return tuple(pixels.min(axis=0)), tuple(pixels.max(axis=0))


def load_moon_template():
    """Loads moon.png and crops it down to just its opaque pixels (it has
    transparent padding around the actual icon), so matchTemplate is
    comparing against the icon shape itself rather than empty margin."""
    img = cv2.imread(MOON_TEMPLATE_PATH, cv2.IMREAD_UNCHANGED)
    if img is None:
        print(f"[moon] couldn't load {MOON_TEMPLATE_PATH} — moon-check disabled")
        return None
    if img.ndim == 3 and img.shape[2] == 4:
        ys, xs = np.where(img[:, :, 3] > 10)
        if len(xs):
            img = img[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
        img = img[:, :, :3]
    return img


def moon_check_region():
    (x1, y1), (x2, y2) = MOON_CHECK_CORNER_A, MOON_CHECK_CORNER_B
    left, right = sorted((x1, x2))
    top, bottom = sorted((y1, y2))
    left -= MOON_CHECK_MARGIN
    top -= MOON_CHECK_MARGIN
    right += MOON_CHECK_MARGIN
    bottom += MOON_CHECK_MARGIN
    return {"left": left, "top": top, "width": right - left, "height": bottom - top}


def is_moon_visible(sct, template):
    if template is None:
        return False
    frame = np.asarray(sct.grab(moon_check_region()))
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


def run_moon_action():
    """Moon icon spotted on the every-Nth-session check — press 5, then
    Space."""
    print("\nMoon detected — pressing 5, then Space...")
    tap_vk(_VK_5, "5")
    kb.press(pynput_keyboard.Key.space)
    time.sleep(MOON_ACTION_KEY_HOLD_SECONDS)
    kb.release(pynput_keyboard.Key.space)


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
    to reacting to the pin's real position; once the whole bar
    disappears again (debounced over MISSING_GRACE_FRAMES so a single
    blip doesn't end it early) the check is over — but disappearance is
    only ever checked once the bar has been seen, so "not appeared yet"
    can never be mistaken for "already over"."""
    print("Jitter-clicking while waiting for the whole bar to appear...")
    bar_seen = False
    missing_count = 0
    last_tap_time = time.time()
    last_color_debug_time = 0.0
    start_time = time.time()

    frame_count = 0
    fps_timer = time.time()

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

            frame = grab_frame(sct)
            bar, needle, hsv = detect_both(frame)

            if bar is None or needle is None:
                now = time.time()

                if bar_seen:
                    missing_count += 1
                    if missing_count >= MISSING_GRACE_FRAMES:
                        print("Whole bar disappeared — check complete.")
                        return

                # Log the real HSV of whatever's not being recognized as
                # the control bar — whether that's before the whole bar
                # has appeared at all, or a color-shift mid-check (e.g.
                # the pin going off the control bar) that isn't covered
                # by CONTROLLED_BAR_HSV_RANGES yet. Previously this only
                # fired pre-appearance, so a mid-check color shift never
                # printed any diagnostic at all.
                if needle is not None and now - last_color_debug_time >= COLOR_DEBUG_PRINT_INTERVAL:
                    sample = sample_unrecognized_color(hsv)
                    if sample is not None:
                        print(f"\n[color debug] control bar not recognized — unknown blob HSV "
                              f"range: low={sample[0]} high={sample[1]}")
                    last_color_debug_time = now

                # Whether the whole bar hasn't appeared yet or just
                # blipped out of detection mid-check, keep jitter-
                # clicking on the keep-alive cadence so position is
                # never just left to drift while blind.
                if now - last_tap_time >= KEEPALIVE_TAP_INTERVAL:
                    kb.press(HOLD_KEY)
                    time.sleep(TAP_SECONDS)
                    kb.release(HOLD_KEY)
                    last_tap_time = time.time()
                else:
                    kb.release(HOLD_KEY)
                continue

            if not bar_seen:
                print("Whole bar appeared — engaging reactive control.")
            bar_seen = True
            missing_count = 0
            bar_left, bar_right, _ = bar
            _, _, needle_center = needle
            bar_target = bar_left + PIN_TARGET_FRACTION * (bar_right - bar_left)

            error = needle_center - bar_target
            now = time.time()

            if error >= 0 or (now - last_tap_time) >= KEEPALIVE_TAP_INTERVAL:
                # pin at/right of the target point -> tap. Also tap on
                # a keep-alive timer even when left of it, so the
                # leftward pull never gets long enough to keep
                # accelerating.
                kb.press(HOLD_KEY)
                time.sleep(TAP_SECONDS)
                kb.release(HOLD_KEY)
                last_tap_time = time.time()

            frame_count += 1
            now = time.time()
            if now - fps_timer >= 1.0:
                print(f"loop rate: {frame_count} Hz  |  error: {error:.0f}px    ", end="\r")
                frame_count = 0
                fps_timer = now
    finally:
        # Guaranteed release no matter which return path above is hit —
        # the bar being gone must never leave the key held.
        kb.release(HOLD_KEY)


def main():
    for i in range(3, 0, -1):
        print(i)
        time.sleep(1)

    print("Autonomous bar bot running. F8 = pause/resume, ESC = quit.")

    moon_template = load_moon_template()
    session_count = 0

    with mss.mss() as sct:
        while not _stopped:
            if not wait_while_paused_or_unfocused():
                break

            if session_count >= SESSIONS_BETWEEN_MOON_CHECKS:
                session_count = 0
                if is_moon_visible(sct, moon_template):
                    run_moon_action()
                # Full delay before the cast either way -- whether or
                # not the moon was actually found this time.
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
