#!/usr/bin/env python3
"""PrintWindow capture of a top-level window, independent of overlap."""
import ctypes
import sys
from ctypes import wintypes
from PIL import Image

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()

TARGET = sys.argv[1] if len(sys.argv) > 1 else "Live-Status"
OUT = sys.argv[2] if len(sys.argv) > 2 else "nw-window.png"

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
rect = RECT()
user32.GetWindowRect(hwnd, ctypes.byref(rect))
width, height = rect.right - rect.left, rect.bottom - rect.top
if width <= 0 or height <= 0:
    print("bad-rect")
    sys.exit(1)

hwnd_dc = user32.GetWindowDC(hwnd)
memory_dc = gdi32.CreateCompatibleDC(hwnd_dc)
bitmap = gdi32.CreateCompatibleBitmap(hwnd_dc, width, height)
gdi32.SelectObject(memory_dc, bitmap)
# PW_RENDERFULLCONTENT (2) captures DirectComposition/DX content too; 3 would
# add PW_CLIENTONLY and drop the title bar from a full-window bitmap.
result = user32.PrintWindow(hwnd, memory_dc, 2)


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG),
                ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
                ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                ("biClrImportant", wintypes.DWORD)]


info = BITMAPINFOHEADER()
info.biSize = ctypes.sizeof(BITMAPINFOHEADER)
info.biWidth = width
info.biHeight = -height  # top-down rows
info.biPlanes = 1
info.biBitCount = 32
info.biCompression = 0  # BI_RGB
bitmap_data = (ctypes.c_char * (width * height * 4))()
gdi32.GetDIBits(memory_dc, bitmap, 0, height, bitmap_data, ctypes.byref(info), 0)
image = Image.frombytes("RGBA", (width, height), bytes(bitmap_data), "raw", "BGRA")
gdi32.DeleteObject(bitmap)
gdi32.DeleteDC(memory_dc)
user32.ReleaseDC(hwnd, hwnd_dc)
print(f"printwindow={result} {width}x{height} -> {OUT}")
image.save(OUT)
