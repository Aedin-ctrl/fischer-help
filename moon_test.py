"""
Moon Detection Test (macOS, terminal)

Standalone test tool — NOT wired into fisch_bar.py yet. Same layout/
conventions as that script (same CONFIG-block style, same pynput
keyboard setup, same frontmost-app-check helper) so this can be dropped
straight into the day-cycle check once detection is confirmed working.

FULL LOOP THIS RUNS
---------------------
1. Every CHECK_INTERVAL_SECONDS (1s), screenshot the moon-check region
   and run template matching against moon.png.
2. Print the match score and a DETECTED/not detected verdict every
   check, so you can watch it live and confirm detection actually
   tracks the real moon icon appearing/disappearing on screen.
3. On the RISING EDGE only (moon just became visible, not every single
   check while it stays visible) — press 5, then press Space.
4. Repeat forever until you hit ESC.

pip install mss numpy opencv-python pynput

macOS: grant Screen Recording + Accessibility to your terminal app in
System Settings -> Privacy & Security, then restart the terminal.

CONTROLS
--------
F8  = pause/resume (safe to hit anytime)
ESC = quit immediately
"""

import os
import time
import numpy as np
import cv2
import mss
from pynput import keyboard as pynput_keyboard

try:
    from AppKit import NSWorkspace  # only available on macOS w/ pyobjc
except Exception:
    NSWorkspace = None

# ----------------------------------------------------------------------
# CONFIG
# ----------------------------------------------------------------------
TARGET_APP_NAME = "Roblox"

# How often to screenshot + check for the moon.
CHECK_INTERVAL_SECONDS = 1.0

# The two corners pointed at on screen for the moon check. The box they
# define is smaller than the actual moon icon artwork (once its
# transparent padding is cropped off), so a margin is added around it
# below to make sure the icon fits inside the searched region
# regardless of a few pixels of drift.
MOON_CHECK_CORNER_A = (1430, 848)
MOON_CHECK_CORNER_B = (1316, 811)
MOON_CHECK_MARGIN = 40

# cv2.matchTemplate (TM_CCOEFF_NORMED) score cutoff to call it "yes,
# that's the moon." 1.0 = pixel-perfect match. Tune this if it's
# missing real moons or false-triggering on non-moon frames. This is
# the plain unmasked match — confirmed working against a real capture
# earlier (0.997 score when the moon was clearly on screen).
MOON_MATCH_THRESHOLD = 0.6

MOON_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon.png")
CAPTURE_DEBUG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon_test_capture.png")

# How often frontmost_app_ok() actually re-queries the OS (via a real
# ObjC/WindowServer round-trip) vs. reusing the last answer.
FOCUS_CHECK_INTERVAL = 0.5

# macOS virtual keycodes (US ANSI layout). Using an explicit vk forces
# pynput to send a real raw key-code event — plain character strings
# ("5") can silently fall back to a unicode-text insert on macOS, which
# has no keyCode and games like Roblox (reading raw keyCodes for
# hotbar shortcuts) just ignore.
_VK_5 = 0x17
MOON_ACTION_KEY_HOLD_SECONDS = 0.1  # how long each key-down is held before release

# ----------------------------------------------------------------------

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
    most every FOCUS_CHECK_INTERVAL seconds."""
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


def wait_while_paused_or_unfocused():
    """Blocks (checking for stop) while paused or the target app isn't
    frontmost. Returns False if we should abort entirely (ESC hit)."""
    while not _stopped and (_paused or not frontmost_app_ok()):
        time.sleep(0.05)
    return not _stopped


def load_moon_template():
    """Loads moon.png and crops it down to just its opaque pixels (it has
    transparent padding around the actual icon), so matchTemplate is
    comparing against the icon shape itself rather than empty margin."""
    img = cv2.imread(MOON_TEMPLATE_PATH, cv2.IMREAD_UNCHANGED)
    if img is None:
        print(f"[moon] couldn't load {MOON_TEMPLATE_PATH} — does the file exist?")
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


def diagnose_frame(bgr, requested_region):
    """Flags the two most likely reasons detection silently fails:
    a blank/black capture (Screen Recording permission not granted to
    whatever app is running this) or a size mismatch between what was
    requested and what was actually returned (a known mss/macOS Retina
    scaling gotcha — the region can come back at e.g. 2x the requested
    width/height, which throws off template matching even if the
    region position itself is correct)."""
    got_h, got_w = bgr.shape[:2]
    want_w, want_h = requested_region["width"], requested_region["height"]
    if (got_w, got_h) != (want_w, want_h):
        print(f"\n[moon] WARNING: requested a {want_w}x{want_h} region but got back "
              f"{got_w}x{got_h} — likely a Retina/display-scaling mismatch. The region "
              f"position may still be right, but matchTemplate needs the template and "
              f"the live capture at the same scale.")

    mean = bgr.mean()
    std = bgr.std()
    if std < 3.0:
        print(f"\n[moon] WARNING: captured region looks blank/uniform (mean={mean:.1f}, "
              f"std={std:.1f}) — check Screen Recording permission for whatever app is "
              f"running this script (System Settings -> Privacy & Security), or confirm "
              f"the region isn't pointing off-screen.")


def moon_match_score(sct, template):
    """Returns the raw match score (or None if the region is too small
    to match at all), so main() can print it every single check. Also
    saves the captured region to CAPTURE_DEBUG_PATH and runs
    diagnose_frame() every call, so a failure to detect can actually be
    diagnosed instead of guessed at."""
    region = moon_check_region()
    frame = np.asarray(sct.grab(region))
    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)
    cv2.imwrite(CAPTURE_DEBUG_PATH, bgr)
    diagnose_frame(bgr, region)

    th, tw = template.shape[:2]
    if bgr.shape[0] < th or bgr.shape[1] < tw:
        return None
    result = cv2.matchTemplate(bgr, template, cv2.TM_CCOEFF_NORMED)
    _, max_val, _, _ = cv2.minMaxLoc(result)
    return max_val


def tap_vk(vk_code, label, hold_seconds=MOON_ACTION_KEY_HOLD_SECONDS):
    key = pynput_keyboard.KeyCode(vk=vk_code)
    print(f"  pressing '{label}'...")
    kb.press(key)
    time.sleep(hold_seconds)
    kb.release(key)


def run_moon_action():
    """Moon icon just appeared (rising edge) — press 5, then Space."""
    print("\nMoon detected — pressing 5, then Space...")
    tap_vk(_VK_5, "5")
    kb.press(pynput_keyboard.Key.space)
    time.sleep(MOON_ACTION_KEY_HOLD_SECONDS)
    kb.release(pynput_keyboard.Key.space)


def main():
    template = load_moon_template()
    if template is None:
        return

    print(f"[moon] search region: {moon_check_region()}")
    print(f"[moon] match threshold: {MOON_MATCH_THRESHOLD}")
    print(f"[moon] saving each captured frame to: {CAPTURE_DEBUG_PATH}")
    print("Scanning for the moon every "
          f"{CHECK_INTERVAL_SECONDS:.1f}s. F8 = pause/resume, ESC = quit.\n")

    was_visible = False

    with mss.mss() as sct:
        while not _stopped:
            if not wait_while_paused_or_unfocused():
                break

            score = moon_match_score(sct, template)
            ts = time.strftime("%H:%M:%S")

            if score is None:
                print(f"{ts}  region smaller than template — can't match at all")
            else:
                is_visible = score >= MOON_MATCH_THRESHOLD
                verdict = "MOON DETECTED" if is_visible else "no match"
                print(f"{ts}  score={score:.3f}  -> {verdict}          ", end="\r")

                if is_visible and not was_visible:
                    print()  # keep the action's own prints on fresh lines
                    run_moon_action()

                was_visible = is_visible

            slept = 0.0
            while slept < CHECK_INTERVAL_SECONDS and not _stopped:
                time.sleep(0.1)
                slept += 0.1

    print("\nStopped.")


if __name__ == "__main__":
    main()
