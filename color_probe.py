"""
Live color probe (macOS)

Prints the BGR + HSV of whatever pixel is under your mouse cursor,
5x/sec, as long as the cursor is inside REGION. Use this to hover over
the bar in Roblox at the moment it goes into its "off-line" discolored
state and read off the exact HSV numbers, then plug them into
CONTROLLED_BAR_HSV_RANGES in auto_bar_bot_mac.py.

Ctrl+C to quit.

pip install mss numpy opencv-python pynput
"""

import time
import numpy as np
import cv2
import mss
from pynput.mouse import Controller as MouseController

# Keep this in sync with REGION in auto_bar_bot_mac.py
REGION = {"left": 426, "top": 785, "width": 589, "height": 35}

mouse = MouseController()


def main():
    print("Move your mouse over the region in Roblox — printing BGR/HSV under the cursor.")
    print(f"REGION = {REGION}")
    with mss.mss() as sct:
        while True:
            x, y = mouse.position
            rel_x = int(x - REGION["left"])
            rel_y = int(y - REGION["top"])

            if 0 <= rel_x < REGION["width"] and 0 <= rel_y < REGION["height"]:
                frame = np.asarray(sct.grab(REGION))  # BGRA
                b, g, r, _ = frame[rel_y, rel_x]
                hsv = cv2.cvtColor(np.uint8([[[b, g, r]]]), cv2.COLOR_BGR2HSV)[0][0]
                print(
                    f"pos=({rel_x:3d},{rel_y:3d})  BGR=({b:3d},{g:3d},{r:3d})  "
                    f"HSV=({hsv[0]:3d},{hsv[1]:3d},{hsv[2]:3d})          ",
                    end="\r",
                )
            else:
                print("cursor outside REGION                                        ", end="\r")

            time.sleep(0.2)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nStopped.")
