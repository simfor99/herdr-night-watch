import ctypes
import os
import sys
import time
from PIL import ImageGrab

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()

class RECT(ctypes.Structure):
    _fields_ = [('left', ctypes.c_long), ('top', ctypes.c_long), ('right', ctypes.c_long), ('bottom', ctypes.c_long)]

found = {}
def callback(hwnd, extra):
    if user32.IsWindowVisible(hwnd):
        length = user32.GetWindowTextLengthW(hwnd)
        title = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, title, length + 1)
        val = title.value
        if 'Live-Status' in val or 'Live Status' in val:
            found['live'] = hwnd
        elif 'Limits' in val:
            found['limits'] = hwnd
    return True

WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_int, ctypes.c_int)
user32.EnumWindows(WNDENUMPROC(callback), 0)

if 'live' not in found:
    print('Error: Live window not found')
    sys.exit(1)

r_live = RECT()
user32.GetWindowRect(found['live'], ctypes.byref(r_live))
left = r_live.left
top = r_live.top
right = r_live.right
bottom = r_live.bottom

if 'limits' in found:
    r_lim = RECT()
    user32.GetWindowRect(found['limits'], ctypes.byref(r_lim))
    left = min(left, r_lim.left)
    top = min(top, r_lim.top)
    right = max(right, r_lim.right)
    bottom = max(bottom, r_lim.bottom)

out_name = sys.argv[1] if len(sys.argv) > 1 else 'captured.png'
img = ImageGrab.grab(bbox=(left, top, right, bottom), all_screens=True)
img.save(out_name)
print(f'Captured ({left}, {top}, {right}, {bottom}) -> {out_name} size={img.size}')
