"""
Autonomous Bar Bot (macOS, terminal)

FULL LOOP THIS RUNS
---------------------
1. Cast: press 9, then 1, then hold-click for 1s and release, wait
   0.5s, then tap Enter 15 times at a 0.12s interval.
2. The instant control is handed over, start butterfly-clicking (rapid
   discrete taps, no interval gating — see CONTROL LOGIC below) — the
   bar starts centered on the needle right then, and a constant pull
   immediately drags it left with only repeated taps to resist it
   (holding the key down does nothing — see below), so it's already
   clicking every loop iteration through the wait for the bar to
   visually appear, for up to BAR_APPEAR_TIMEOUT_SECONDS (5s). If it
   never appears in that window, give up and recast (back to step 1)
   instead of clicking forever.
3. The instant the bar/needle are actually detected, keep tapping
   unconditionally (regardless of measured error) for
   INITIAL_REACTIVE_JITTER_SECONDS (2.5s) — it starts centered on the
   needle right then, so measured error reads ~0 even though the pull
   is already dragging it left, and waiting for a positive error before
   reacting means doing nothing for however long that takes. After that
   window, switch to full conditional reactive control of the needle's
   real position (see CONTROL LOGIC below). Keep reacting until the bar
   disappears again. Disappearance is only ever evaluated after the bar
   has been seen at least once, so "hasn't appeared yet" can never be
   mistaken for "the check is already over."
4. Once the bar is gone, wait 1s. Every SESSIONS_BETWEEN_MOON_CHECKS
   sessions, press 9 (de-equips the rod — the day/night UI is hidden
   while it's out) and wait MOON_CHECK_PRE_DELAY_SECONDS before
   checking for the moon icon, since the check only makes sense once
   casting/reeling/fishing has fully finished. Then go back to step 1.
   Forever, until you hit ESC.

CONTROL LOGIC (rapid discrete taps, not a held key)
------------------------------------------------------
The bar is pulled left continuously and immediately the moment control
is handed over, with nothing to resist it but taps — holding the key
down doesn't work in practice here, so beating the pull takes rapid,
repeated discrete taps ("butterfly clicking"), fired every single loop
iteration with no gating interval between them:
  - bar/needle not found yet (before first appearance, or a mid-check
    detection blip) -> tap. There's no fresh position info to justify
    stopping, and the pull is constant and immediate regardless.
  - needle right of the bar's center (the bar hasn't caught up to /
    isn't covering the needle) -> tap, to keep pushing right against
    the pull.
  - needle at/left of center (the bar has overshot past the needle)
    -> do nothing, letting the pull correct back left until it
    re-covers.
No history, no velocity, no prediction — every loop iteration just
finds the bar and needle fresh and decides tap-vs-nothing from
scratch.

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
TARGET_APP_NAME = "Roblox"

REGION = {"left": 426, "top": 785, "width": 589, "height": 35}

# The controlled bar isn't always the same color — it goes bright/white
# normally, but shifts to a dark translucent brownish-green when it's
# outside the target line. Detection unions ALL of these ranges, so add
# more (low, high) pairs here as you find other color states. Use
# color_probe.py to read exact HSV values off the live game and replace
# the second (guessed) range below with real numbers.
CONTROLLED_BAR_HSV_RANGES = [
    (np.array([0, 0, 150]), np.array([180, 40, 255])),      # normal: bright/white
    (np.array([15, 20, 40]), np.array([45, 160, 170])),     # guess: dark brownish-green — CALIBRATE THIS
]

NEEDLE_HSV_LOW = np.array([95, 20, 40])
NEEDLE_HSV_HIGH = np.array([125, 150, 160])

# Opening sequence uses a real mouse click-and-hold; the control loop
# (once the bar is up) uses a tapped key instead. Beating the constant
# leftward pull takes rapid, repeated discrete taps ("butterfly
# clicking"), not a hold.
HOLD_KEY = pynput_keyboard.Key.space

# CONTROL RULE — tap every single loop iteration whenever push is
# needed, with NO interval gating between taps (gating taps to once
# every fixed interval left long gaps for the pull to act unopposed).
# See run_control_until_gone().
TAP_SECONDS = 0.03  # length of each tap; tune this — shorter = faster/more
                    # clicks per second, but too short may stop registering

# The instant the bar appears it's already centered on the needle, so
# the measured error reads ~0 (or even slightly negative) even though
# the pull is already dragging it left — reacting only to a measured
# rightward error means doing nothing for however long it takes the
# drift to become measurable, which is exactly the window it was
# diving left uncorrected. So for this long after "Bar appeared", tap
# unconditionally every iteration regardless of measured error, before
# switching over to the normal conditional reactive rule.
INITIAL_REACTIVE_JITTER_SECONDS = 2.5

# How many consecutive "not found" frames after the bar has already
# been seen once before we decide the check has actually ended (vs. a
# one-frame detection blip).
MISSING_GRACE_FRAMES = 15

# How often frontmost_app_ok() actually re-queries the OS (via a real
# ObjC/WindowServer round-trip) vs. reusing the last answer. That round
# trip was being made on every single loop iteration and occasionally
# stalls for a noticeable moment — throttling it removes those pauses
# from the hot control loop.
FOCUS_CHECK_INTERVAL = 0.5

# How often (at most) to print a color-sample when the bar can't be
# found, so you can read off real HSV values for CONTROLLED_BAR_HSV_RANGES
# straight from the console instead of using a separate tool.
COLOR_DEBUG_PRINT_INTERVAL = 1.0

# Cast sequence timings
# macOS virtual keycodes (US ANSI layout) for the number row. Using an
# explicit vk forces pynput to send a real raw key-code event — plain
# character strings ("9", "1") can silently fall back to a unicode-text
# insert on macOS, which has no keyCode and games like Roblox (reading
# raw keyCodes for hotbar shortcuts) just ignore.
_VK_9 = 0x19
_VK_1 = 0x12
PRE_TRIGGER_KEY_SEQUENCE = [
    (pynput_keyboard.KeyCode(vk=_VK_9), "9"),
    (pynput_keyboard.KeyCode(vk=_VK_1), "1"),
]
PRE_TRIGGER_KEY_HOLD_SECONDS = 0.12  # bumped up from 0.05 — needs longer than Enter/Space did
PRE_TRIGGER_KEY_DELAY = 0.5  # delay after each key in the sequence above
TRIGGER_HOLD_SECONDS = 1.0
TRIGGER_ENTER_COUNT = 15
TRIGGER_ENTER_INTERVAL = 0.12
TRIGGER_ENTER_HOLD_SECONDS = 0.05  # how long each Enter key-down is held before release
POST_HOLD_ENTER_DELAY_SECONDS = 0.5  # pause after the click-hold, before the Enter presses start

# After the bar disappears, wait this long before casting again.
POST_CHECK_DELAY_SECONDS = 1.0

# If the bar hasn't been detected at all within this long after casting,
# give up waiting and recast instead of sitting there watching forever.
BAR_APPEAR_TIMEOUT_SECONDS = 5.0

# If the reactive control loop (tapping HOLD_KEY) runs longer than this
# without the bar ever clearing on its own, force it to give up and
# recast instead of holding forever on a stuck/missed check.
CONTROL_TIMEOUT_SECONDS = 25.0

# Every this-many full sessions (one session = one cast + one control
# loop), pause to check whether a moon icon is on screen (see MOON
# CHECK below). Set aside entirely if the user keeps daytime on.
SESSIONS_BETWEEN_MOON_CHECKS = 5

# The two corners the user pointed at on screen for the moon check.
# The box they define (114x37px) is smaller than the actual moon icon
# artwork (~36x47px once its transparent padding is cropped off), so a
# margin is added around it below to make sure the icon fits inside the
# searched region regardless of a few pixels of drift.
MOON_CHECK_CORNER_A = (1430, 848)
MOON_CHECK_CORNER_B = (1316, 811)
MOON_CHECK_MARGIN = 40

# cv2.matchTemplate (TM_CCOEFF_NORMED) score cutoff to call it "yes,
# that's the moon." 1.0 = pixel-perfect match. Tune this if it's
# missing real moons or false-triggering on non-moon frames.
MOON_MATCH_THRESHOLD = 0.6

MOON_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon.png")

# The day/night UI (and thus the moon icon) is hidden while the rod is
# out — cast, reeling, or fishing. So right before checking, press 9 to
# de-equip it and wait this long for the UI to actually show up on
# screen before taking the screenshot.
MOON_CHECK_PRE_DELAY_SECONDS = 0.5

MOON_POST_DETECT_DELAY = 0.5  # "small delay" before the reset key presses
MOON_CLICK_COUNT = 5
MOON_CLICK_INTERVAL = 0.3  # "slowly" — delay between each of the 5 clicks

# macOS virtual keycodes for '5' and '1', same reasoning as the vk usage
# above: raw keyCode events are what Roblox hotbar shortcuts expect.
_VK_5 = 0x17

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
    return np.asarray(sct.grab(REGION))  # BGRA


def detect_both(frame):
    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)  # cv2 has no direct BGRA->HSV code
    hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)  # convert once, reuse for both masks
    bar = find_x_bounds_multi(hsv, CONTROLLED_BAR_HSV_RANGES)
    needle = find_x_bounds(hsv, NEEDLE_HSV_LOW, NEEDLE_HSV_HIGH)
    return bar, needle, hsv


def sample_unrecognized_color(hsv):
    """When the bar can't be found by any known range, sample whatever
    colorful (non-background, non-needle) blob is on screen so its real
    HSV range can be read straight off the console."""
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


def tap_vk(vk_code, label, hold_seconds=PRE_TRIGGER_KEY_HOLD_SECONDS):
    key = pynput_keyboard.KeyCode(vk=vk_code)
    print(f"  pressing '{label}'...")
    kb.press(key)
    time.sleep(hold_seconds)
    kb.release(key)


def run_moon_sequence():
    """Moon icon spotted on the every-Nth-session check: small delay,
    then 5 -> 5 slow clicks -> 1. Falls through to the normal
    post-check delay + recast afterward, same as any other restart."""
    print("\nMoon detected — running night-reset sequence...")
    time.sleep(MOON_POST_DETECT_DELAY)
    tap_vk(_VK_5, "5")
    for _ in range(MOON_CLICK_COUNT):
        if _stopped:
            return
        mouse.click(Button.left)
        time.sleep(MOON_CLICK_INTERVAL)
    tap_vk(_VK_1, "1")


def prompt_keep_daytime():
    answer = input("Do you want to keep daytime on? (yes/no): ").strip().lower()
    if answer in ("yes", "y"):
        print("okay keep daytime on")
        return True
    print("alright normal time")
    return False


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

    for i in range(TRIGGER_ENTER_COUNT):
        if _stopped:
            return
        kb.press(pynput_keyboard.Key.enter)
        time.sleep(TRIGGER_ENTER_HOLD_SECONDS)
        kb.release(pynput_keyboard.Key.enter)
        time.sleep(TRIGGER_ENTER_INTERVAL)


def run_control_until_gone(sct):
    """Runs immediately after casting. Butterfly-clicks (rapid, discrete
    press/release taps, no interval gating) from the very first frame —
    the bar starts centered on the needle the instant control is handed
    over, and the leftward pull is immediate and constant with nothing
    to resist it but repeated taps, so it clicks every single loop
    iteration even before the bar is visually confirmed (up to
    BAR_APPEAR_TIMEOUT_SECONDS). The moment the bar/needle are actually
    detected, it keeps tapping unconditionally for another
    INITIAL_REACTIVE_JITTER_SECONDS regardless of measured error — right
    at that handoff error reads ~0 even though the pull is already
    acting, so reacting only to a positive error would mean doing
    nothing for however long it takes to become measurable. After that
    window it switches to full conditional reactive control: tap every
    iteration while the needle's right of center (still catching up),
    do nothing when it's at/left of center (overshot — let the pull
    correct back). If the bar never appears within the timeout, gives
    up and returns so the caller recasts. Once reacting, disappearance
    is debounced over MISSING_GRACE_FRAMES so a single detection blip
    doesn't end the check early — a mid-check blip keeps clicking too,
    since there's no fresh position info to justify stopping."""
    print("Butterfly-clicking — watching for the bar to appear...")
    bar_seen = False
    missing_count = 0
    last_color_debug_time = 0.0
    start_time = time.time()
    reactive_start_time = None

    frame_count = 0
    fps_timer = time.time()

    def tap():
        kb.press(HOLD_KEY)
        time.sleep(TAP_SECONDS)
        kb.release(HOLD_KEY)

    try:
        while not _stopped:
            if not bar_seen and time.time() - start_time >= BAR_APPEAR_TIMEOUT_SECONDS:
                print(f"\n{BAR_APPEAR_TIMEOUT_SECONDS:.0f}s elapsed with no bar detected "
                      f"— recasting.")
                return

            if bar_seen and time.time() - start_time >= CONTROL_TIMEOUT_SECONDS:
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
                        print("Bar disappeared — check complete.")
                        return
                    # Mid-check blip: no fresh position info this frame,
                    # so keep clicking rather than stop into the pull.
                    tap()
                    continue

                # Bar hasn't appeared yet at all — click every iteration
                # from frame one. It starts centered on handoff and the
                # pull is immediate, so there's no position info that
                # would justify not clicking.
                if needle is not None and now - last_color_debug_time >= COLOR_DEBUG_PRINT_INTERVAL:
                    sample = sample_unrecognized_color(hsv)
                    if sample is not None:
                        print(f"\n[color debug] bar not recognized — unknown blob HSV range: "
                              f"low={sample[0]} high={sample[1]}")
                    last_color_debug_time = now
                tap()
                continue

            if not bar_seen:
                print("Bar appeared — engaging reactive control.")
                reactive_start_time = time.time()
            bar_seen = True
            missing_count = 0
            bar_left, bar_right, bar_center = bar
            _, _, needle_center = needle

            error = needle_center - bar_center

            if time.time() - reactive_start_time < INITIAL_REACTIVE_JITTER_SECONDS:
                # It starts centered on the needle right when control is
                # handed over, so measured error reads ~0 even though the
                # pull is already dragging it left — waiting for error to
                # read positive before reacting means doing nothing for
                # however long that takes. Tap unconditionally here
                # instead, regardless of measured error.
                tap()
            elif error > 0:
                # needle right of the bar's dead center -> tap, every
                # single iteration, still catching up against the pull.
                tap()
            # else: at/left of center -> bar has overshot past it, do
            # nothing so the pull corrects back until it re-covers.

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
    keep_daytime = prompt_keep_daytime()

    for i in range(3, 0, -1):
        print(i)
        time.sleep(1)

    print("Autonomous bar bot running. F8 = pause/resume, ESC = quit.")

    # Keeping daytime on means night/the moon shouldn't occur at all, so
    # skip loading the template and never bother running the check.
    moon_template = None if keep_daytime else load_moon_template()
    session_count = 0

    with mss.mss() as sct:
        while not _stopped:
            if not wait_while_paused_or_unfocused():
                break

            run_trigger_sequence()
            if _stopped:
                break

            run_control_until_gone(sct)
            session_count += 1

            if not keep_daytime and session_count >= SESSIONS_BETWEEN_MOON_CHECKS:
                session_count = 0
                # The day/night UI is hidden while the rod is out, so
                # de-equip it (9) and give the UI a moment to actually
                # appear before screenshotting for the moon check.
                tap_vk(_VK_9, "9")
                time.sleep(MOON_CHECK_PRE_DELAY_SECONDS)
                if is_moon_visible(sct, moon_template):
                    run_moon_sequence()

            slept = 0.0
            while slept < POST_CHECK_DELAY_SECONDS and not _stopped:
                time.sleep(0.1)
                slept += 0.1
            # loop back around: recast

    kb.release(HOLD_KEY)
    print("Stopped.")


if __name__ == "__main__":
    main()
