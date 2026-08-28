"""
Standalone physics measurement tool (macOS, terminal)

Run this by itself, separate from the bot. It opens one skill check,
waits out the frozen warm-up period, measures pull and boost
acceleration a few times each (averaging via median to reject a run
corrupted by a wall bounce), prints the results, and saves them to
bar_bot_calibration.json in the current folder.

The main bot script loads that file automatically if present, so you
only need to run this once — not every session.

pip install mss numpy opencv-python pynput
"""

import time
import json
import numpy as np
import cv2
import mss
from pynput import keyboard as pynput_keyboard
from pynput.mouse import Controller as MouseController, Button

# ----------------------------------------------------------------------
# CONFIG — keep these matching your main bot script
# ----------------------------------------------------------------------

TARGET_APP_NAME = "REPLACE_ME"

REGION = {"left": 426, "top": 785, "width": 589, "height": 35}

CONTROLLED_BAR_HSV_LOW = np.array([0, 0, 150])
CONTROLLED_BAR_HSV_HIGH = np.array([180, 40, 255])

NEEDLE_HSV_LOW = np.array([95, 20, 40])
NEEDLE_HSV_HIGH = np.array([125, 150, 160])

TRIGGER_HOLD_SECONDS = 1.0
TRIGGER_ENTER_COUNT = 25
TRIGGER_ENTER_INTERVAL = 0.3

# The bar appears (sliding up, <0.25s) then sits frozen ~2s before it's
# actually controllable. Wait this long past detection before trusting
# any position samples for physics measurement. Padded a bit for safety.
STARTUP_FREEZE_SECONDS = 2.4

SAMPLE_SECONDS_PER_TRIAL = 0.25  # short: bar can reach a wall in <1s
TRIALS_PER_PHASE = 4
WALL_MARGIN_FRACTION = 0.12  # abort a trial if bar gets this close to
                               # either edge of REGION (fraction of width)

# How many full open-a-check-and-measure rounds to run. Results from
# all rounds are pooled together before taking the final median, so
# more rounds = more trials = a steadier result, at the cost of
# needing to sit through the whole thing a few extra times.
CALIBRATION_RUNS = 3

LOOP_HZ = 200
CALIBRATION_FILE = "bar_bot_calibration.json"

# ----------------------------------------------------------------------

mouse = MouseController()
kb = pynput_keyboard.Controller()

try:
    from AppKit import NSWorkspace
except Exception:
    NSWorkspace = None

_TARGET_APP_NAME_LOWER = TARGET_APP_NAME.lower()


def frontmost_app_ok():
    if NSWorkspace is None:
        return True  # AppKit unavailable — don't block on it
    try:
        app = NSWorkspace.sharedWorkspace().frontmostApplication()
        name = app.localizedName() if app else ""
        return _TARGET_APP_NAME_LOWER in name.lower()
    except Exception:
        return True  # if the check itself fails, don't block on it


def set_boost(on, current_state):
    if on == current_state:
        return current_state
    (mouse.press if on else mouse.release)(Button.left)
    return on


def find_x_bounds(hsv, hsv_low, hsv_high):
    mask = cv2.inRange(hsv, hsv_low, hsv_high)
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None
    biggest = max(contours, key=cv2.contourArea)
    x, _, w, _ = cv2.boundingRect(biggest)
    return x, x + w, x + w / 2


def grab_frame(sct):
    return np.asarray(sct.grab(REGION))  # BGRA, no need to drop alpha before cvtColor


def detect_both(frame):
    hsv = cv2.cvtColor(frame, cv2.COLOR_BGRA2HSV)  # convert once, reuse for both masks
    bar = find_x_bounds(hsv, CONTROLLED_BAR_HSV_LOW, CONTROLLED_BAR_HSV_HIGH)
    needle = find_x_bounds(hsv, NEEDLE_HSV_LOW, NEEDLE_HSV_HIGH)
    return bar, needle


def run_trigger_sequence():
    print("Running opening sequence...")
    boost_state = set_boost(True, False)
    time.sleep(TRIGGER_HOLD_SECONDS)
    set_boost(False, boost_state)
    for _ in range(TRIGGER_ENTER_COUNT):
        kb.press(pynput_keyboard.Key.enter)
        kb.release(pynput_keyboard.Key.enter)
        time.sleep(TRIGGER_ENTER_INTERVAL)


def fit_acceleration(samples):
    if len(samples) < 5:
        return None
    ts = np.array([s[0] for s in samples])
    xs = np.array([s[1] for s in samples])
    coeffs = np.polyfit(ts, xs, 2)  # x = 0.5*a*t^2 + v0*t + x0
    return coeffs[0] * 2


def measure_phase(sct, hold_boost):
    """One trial: hold boost on/off, sampling until either the timer
    runs out or the bar gets close to a wall (whichever is first) —
    a bounce mid-measurement would corrupt the acceleration fit, so we
    bail out before that can happen rather than trusting a fixed timer.
    Also aborts immediately if the target app loses focus, checked
    every iteration (every ~1/LOOP_HZ seconds)."""
    margin = WALL_MARGIN_FRACTION * REGION["width"]
    boost_state = set_boost(hold_boost, False)
    samples = []
    t0 = time.time()
    while time.time() - t0 < SAMPLE_SECONDS_PER_TRIAL:
        if not frontmost_app_ok():
            set_boost(False, boost_state)
            return None
        frame = grab_frame(sct)
        bar, needle = detect_both(frame)
        if bar is None or needle is None:
            set_boost(False, boost_state)
            return None
        bar_left, bar_right, bar_center = bar
        if bar_center < margin or bar_center > REGION["width"] - margin:
            break  # too close to a wall, stop before a bounce corrupts the fit
        samples.append((time.time() - t0, bar_center))
        time.sleep(1.0 / LOOP_HZ)
    set_boost(False, boost_state)
    return fit_acceleration(samples)


def run_one_round(sct, round_num, total_rounds):
    """Opens one fresh check and runs all trials against it. Returns
    (pull_results, boost_results) lists — possibly empty if it never
    detected the check or it ended too early."""
    print(f"\n=== Round {round_num}/{total_rounds}: opening check ===")
    run_trigger_sequence()

    deadline = time.time() + 5
    bar = needle = None
    while time.time() < deadline:
        if not frontmost_app_ok():
            print("Lost focus on target app — skipping this round.")
            return [], []
        frame = grab_frame(sct)
        bar, needle = detect_both(frame)
        if bar is not None and needle is not None:
            break
        time.sleep(0.05)

    if bar is None:
        print("Never detected the check appearing this round — skipping.")
        return [], []

    print(f"Detected. Waiting {STARTUP_FREEZE_SECONDS}s for the freeze to end...")
    time.sleep(STARTUP_FREEZE_SECONDS)

    pull_results = []
    boost_results = []
    for i in range(TRIALS_PER_PHASE):
        print(f"Pull trial {i+1}/{TRIALS_PER_PHASE}...")
        a = measure_phase(sct, hold_boost=False)
        if a is not None:
            pull_results.append(a)
        else:
            print("  (check ended mid-trial, skipping)")

        print(f"Boost trial {i+1}/{TRIALS_PER_PHASE}...")
        a = measure_phase(sct, hold_boost=True)
        if a is not None:
            boost_results.append(a)
        else:
            print("  (check ended mid-trial, skipping)")

    return pull_results, boost_results


def main():
    print("Standalone physics measurement. This will open a skill")
    print("check on its own, multiple times — make sure the game window")
    print("is focused and nothing else needs your mouse/keyboard right now.")
    input("Press Enter to begin...")
    print("Switch to the game now — starting in:")
    for n in (3, 2, 1):
        print(f"  {n}...")
        time.sleep(1)

    all_pull = []
    all_boost = []

    with mss.mss() as sct:
        for round_num in range(1, CALIBRATION_RUNS + 1):
            p, b = run_one_round(sct, round_num, CALIBRATION_RUNS)
            all_pull.extend(p)
            all_boost.extend(b)
            if round_num < CALIBRATION_RUNS:
                time.sleep(1.0)  # brief gap before triggering the next one

    if not all_pull or not all_boost:
        print("Not enough clean trials collected across all rounds. Try")
        print("again — you may need more room on the bar, or checks may")
        print("be ending too soon after opening.")
        return

    pull = float(np.median(all_pull))
    boost = float(np.median(all_boost))
    vmax = float(0.5 * np.sqrt(abs(boost) * REGION["width"]))

    print()
    print(f"Pooled across {CALIBRATION_RUNS} rounds:")
    print(f"Pull acceleration:  {pull:.1f} px/s^2  (all trials: {all_pull})")
    print(f"Boost acceleration: {boost:.1f} px/s^2  (all trials: {all_boost})")
    print(f"Suggested VMAX:     {vmax:.0f}")

    with open(CALIBRATION_FILE, "w") as f:
        json.dump({"pull_accel": pull, "boost_accel": boost, "vmax": vmax}, f, indent=2)
    print(f"Saved to {CALIBRATION_FILE} — the main bot script will load this automatically.")


if __name__ == "__main__":
    main()
