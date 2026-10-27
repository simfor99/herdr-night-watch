#!/usr/bin/env python3
"""Bring a top-level window to the front, then screen-capture its rect."""
import ctypes
import sys
import time
from ctypes import wintypes
from PIL import ImageGrab

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()

TARGET = sys.argv[1] if len(sys.argv) > 1 else "Live-Status"
OUT = sys.argv[2] if len(sys.argv) > 2 else r"C:\Users\Simon\Desktop\temp\nw-window.png"

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
found = []


@WNDENUMPROC
def callback(hwnd, _lparam):
    if user32.IsWindowVisible(hwnd):
        length = user32.GetWindowTextLengthW(hwnd)
        buffer = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, buffer, length + 1)
        if TARGET in buffer.value:
            found.append(hwnd)
    return True


class RECT(ctypes.Structure):
    _fields_ = [("left", wintypes.LONG), ("top", wintypes.LONG),
                ("right", wintypes.LONG), ("bottom", wintypes.LONG)]


user32.EnumWindows(callback, 0)
if not found:
    print("window-not-found")
    sys.exit(1)
hwnd = found[0]
user32.SetForegroundWindow(hwnd)
user32.BringWindowToTop(hwnd)
time.sleep(2.5)
rect = RECT()
user32.GetWindowRect(hwnd, ctypes.byref(rect))
box = (rect.left, rect.top, rect.right, rect.bottom)
image = ImageGrab.grab(bbox=box, all_screens=True)
image.save(OUT)
print(f"foreground={hwnd} box={box} -> {OUT}")
