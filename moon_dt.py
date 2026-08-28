"""
moon_dt.py — standalone moon-icon detection tester

Run this on its own (Roblox in the foreground, daytime/nighttime as
needed) to check whether the moon-check region + template actually
match. It uses the exact same region, margin, template path, and
match logic as auto_bar_bot_mac.py's moon-check — nothing here is
simplified or approximated, so a pass/fail here should transfer
directly back into the main script.

It runs continuously, printing the match score a few times a second,
and saves the last captured region to moon_dt_capture.png each loop
so you can visually confirm the crop is actually framing the icon.

Matching uses moon.png's alpha channel as a mask (cv2.TM_CCORR_NORMED
with `mask=`), so only the actual icon pixels count toward the score —
whatever's behind the icon's transparent edges (sky color, time of
day, clouds, etc.) is excluded instead of being baked into the
comparison. Prints both the masked score and the old unmasked score
side by side so you can see the difference directly.

Every score is also appended (with a timestamp) to moon_dt_log.txt in
this same folder, so you can run this with Roblox fullscreen and not
watch the terminal at all — just play normally through a day/night
cycle, then quit and read (or hand someone else) the log file after.

Press ESC to quit.

pip install mss numpy opencv-python pynput
"""

import os
import time
import numpy as np
import cv2
import mss
from pynput import keyboard as pynput_keyboard

# ----------------------------------------------------------------------
# Same values as auto_bar_bot_mac.py — keep these in sync, or better,
# once detection is confirmed working here, report the numbers back so
# they can be copied into the main script.
# ----------------------------------------------------------------------
MOON_CHECK_CORNER_A = (1430, 848)
MOON_CHECK_CORNER_B = (1316, 811)
MOON_CHECK_MARGIN = 40

MOON_MATCH_THRESHOLD = 0.6

MOON_TEMPLATE_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon.png")
CAPTURE_DEBUG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon_dt_capture.png")
LOG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "moon_dt_log.txt")

CHECK_INTERVAL = 0.5  # seconds between checks

# ----------------------------------------------------------------------

_stopped = False


def on_press(key):
    global _stopped
    if key == pynput_keyboard.Key.esc:
        _stopped = True
        print("\nStopping...")


listener = pynput_keyboard.Listener(on_press=on_press)
listener.start()


def load_moon_template():
    """Returns (bgr, mask) — mask is the alpha channel, binarized, so
    matchTemplate only scores actual icon pixels and ignores whatever
    background happened to be behind the transparent edges when
    moon.png was captured. Returns (bgr, None) if there's no usable
    alpha channel to build a mask from."""
    img = cv2.imread(MOON_TEMPLATE_PATH, cv2.IMREAD_UNCHANGED)
    if img is None:
        print(f"[moon] couldn't load {MOON_TEMPLATE_PATH} — does the file exist?")
        return None, None
    print(f"[moon] loaded template, raw shape={img.shape}")
    if img.ndim != 3 or img.shape[2] != 4:
        print("[moon] WARNING: template has no alpha channel — can't build a mask, "
              "falling back to unmasked matching only.")
        return img[:, :, :3] if img.ndim == 3 else img, None

    ys, xs = np.where(img[:, :, 3] > 10)
    if len(xs):
        img = img[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
        print(f"[moon] cropped to opaque pixels, shape now={img.shape}")
    else:
        print("[moon] WARNING: template has an alpha channel but no pixels "
              "above alpha=10 — it may be fully transparent.")

    alpha = img[:, :, 3]
    mask = np.where(alpha > 10, 255, 0).astype(np.uint8)
    bgr = img[:, :, :3]
    return bgr, mask


def moon_check_region():
    (x1, y1), (x2, y2) = MOON_CHECK_CORNER_A, MOON_CHECK_CORNER_B
    left, right = sorted((x1, x2))
    top, bottom = sorted((y1, y2))
    left -= MOON_CHECK_MARGIN
    top -= MOON_CHECK_MARGIN
    right += MOON_CHECK_MARGIN
    bottom += MOON_CHECK_MARGIN
    return {"left": left, "top": top, "width": right - left, "height": bottom - top}


def main():
    template, mask = load_moon_template()
    if template is None:
        return

    region = moon_check_region()
    print(f"[moon] search region: {region}")
    print(f"[moon] match threshold: {MOON_MATCH_THRESHOLD}")
    print(f"[moon] mask: {'yes (masked matching)' if mask is not None else 'no (unmasked only)'}")
    print(f"[moon] saving each captured frame to: {CAPTURE_DEBUG_PATH}")
    print(f"[moon] logging every score to: {LOG_PATH}")
    print("Running — ESC to quit.\n")

    th, tw = template.shape[:2]

    with open(LOG_PATH, "a") as log, mss.mss() as sct:
        log.write(f"\n=== run started, region={region}, threshold={MOON_MATCH_THRESHOLD}, "
                   f"masked={mask is not None} ===\n")
        log.flush()

        while not _stopped:
            ts = time.strftime("%H:%M:%S")
            frame = np.asarray(sct.grab(region))
            bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)
            cv2.imwrite(CAPTURE_DEBUG_PATH, bgr)

            if bgr.shape[0] < th or bgr.shape[1] < tw:
                line = (f"region {bgr.shape[1]}x{bgr.shape[0]} is smaller than template "
                        f"{tw}x{th} — can't match at all.")
            elif mask is not None:
                unmasked_result = cv2.matchTemplate(bgr, template, cv2.TM_CCOEFF_NORMED)
                _, unmasked_val, _, _ = cv2.minMaxLoc(unmasked_result)
                masked_result = cv2.matchTemplate(bgr, template, cv2.TM_CCORR_NORMED, mask=mask)
                _, masked_val, _, max_loc = cv2.minMaxLoc(masked_result)
                verdict = "MOON DETECTED" if masked_val >= MOON_MATCH_THRESHOLD else "no match"
                line = (f"masked={masked_val:.3f}  unmasked={unmasked_val:.3f}  "
                        f"best_loc={max_loc}  -> {verdict}")
            else:
                unmasked_result = cv2.matchTemplate(bgr, template, cv2.TM_CCOEFF_NORMED)
                _, unmasked_val, _, _ = cv2.minMaxLoc(unmasked_result)
                verdict = "MOON DETECTED" if unmasked_val >= MOON_MATCH_THRESHOLD else "no match"
                line = f"unmasked={unmasked_val:.3f}  -> {verdict}"

            print(f"{ts}  {line}          ", end="\r")
            log.write(f"{ts}  {line}\n")
            log.flush()

            time.sleep(CHECK_INTERVAL)

        log.write("=== stopped ===\n")

    print("\nStopped.")


if __name__ == "__main__":
    main()
