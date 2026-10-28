#!/usr/bin/env python3
"""Capture an OpenGL (egui) window reliably.

The GDI screen grab used before (ImageGrab over the window rect) misses GL
content that the DWM composites: the grab shows a flat leftover color instead
of the window body. PrintWindow with PW_RENDERFULLCONTENT asks the DWM to
re-render the window into our bitmap, which works for the tray's OpenGL
windows. The tool falls back to the legacy GDI screen grab when PrintWindow
fails and reports which method produced the image plus a flatness metric, so
an empty capture is visible in the output instead of silently wrong.

Usage:
    python capture_gl_window.py [title-substring] [output.png]
Defaults: title "Live-Status", output "gl-window.png".

Exit codes: 0 captured, 1 window not found, 2 every method produced a flat
image (suspect rendering, look at the app, not the capture).
"""
import ctypes
import sys
from ctypes import wintypes
from pathlib import Path

from PIL import Image

PW_CLIENTONLY = 0x00000001
PW_RENDERFULLCONTENT = 0x00000002
# Console windows often carry the command line in their title, which would
# match the needle; they never host the app we want.
CONSOLE_WINDOW_CLASSES = {"ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"}
FLAT_TOP_COLOR_SHARE = 0.90
FLAT_DISTINCT_COLORS = 50

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def find_window(needle: str):
    found = []

    @WNDENUMPROC
    def callback(hwnd, _lparam):
        if user32.IsWindowVisible(hwnd):
            class_buffer = ctypes.create_unicode_buffer(64)
            user32.GetClassNameW(hwnd, class_buffer, 64)
            if class_buffer.value in CONSOLE_WINDOW_CLASSES:
                return True
            length = user32.GetWindowTextLengthW(hwnd)
            buffer = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buffer, length + 1)
            if needle in buffer.value:
                found.append(hwnd)
        return True

    user32.EnumWindows(callback, 0)
    return found[0] if found else None


def bitmap_metrics(img: Image.Image) -> tuple[int, float]:
    # A limit below the pixel count makes getcolors return None for rich
    # windows, which would wrongly read as "one flat color".
    colors = img.convert("RGB").getcolors(maxcolors=img.width * img.height)
    if not colors:
        return 0, 1.0
    colors.sort(reverse=True)
    share = colors[0][0] / (img.width * img.height)
    return len(colors), share


def capture_printwindow(hwnd) -> Image.Image | None:
    """DWM re-render including GL content; physical pixels when DPI aware."""
    rect = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(rect))
    width, height = rect.right - rect.left, rect.bottom - rect.top
    if width <= 0 or height <= 0:
        return None

    hdc = user32.GetDC(hwnd)
    memdc = gdi32.CreateCompatibleDC(hdc)
    bitmap = gdi32.CreateCompatibleBitmap(hdc, width, height)
    old = gdi32.SelectObject(memdc, bitmap)
    try:
        rendered = user32.PrintWindow(hwnd, memdc, PW_CLIENTONLY | PW_RENDERFULLCONTENT)
        if not rendered:
            return None

        class BMIH(ctypes.Structure):
            _fields_ = [
                ("biSize", wintypes.UINT), ("biWidth", wintypes.LONG),
                ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
                ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                ("biClrImportant", wintypes.DWORD),
            ]

        class BMI(ctypes.Structure):
            _fields_ = [("bmiHeader", BMIH), ("biColors", wintypes.DWORD * 3)]

        info = BMI()
        info.bmiHeader.biSize = ctypes.sizeof(BMIH)
        info.bmiHeader.biWidth = width
        info.bmiHeader.biHeight = -height  # top-down rows
        info.bmiHeader.biPlanes = 1
        info.bmiHeader.biBitCount = 32
        info.bmiHeader.biCompression = 0  # BI_RGB

        buffer = ctypes.create_string_buffer(width * height * 4)
        rows = gdi32.GetDIBits(memdc, bitmap, 0, height, buffer, ctypes.byref(info), 0)
        if rows != height:
            return None
    finally:
        # deselect before DeleteObject: a bitmap still selected into a DC
        # cannot be deleted.
        gdi32.SelectObject(memdc, old)
        gdi32.DeleteObject(bitmap)
        gdi32.DeleteDC(memdc)
        user32.ReleaseDC(hwnd, hdc)

    img = Image.frombytes("RGBA", (width, height), buffer.raw)
    r, g, b, _a = img.split()
    return Image.merge("RGB", (b, g, r))


def capture_screen_rect(hwnd) -> Image.Image | None:
    """Legacy GDI screen grab over the window rect (physical coordinates)."""
    rect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    try:
        from PIL import ImageGrab
    except ImportError:
        return None
    return ImageGrab.grab(
        bbox=(rect.left, rect.top, rect.right, rect.bottom), all_screens=True
    )


def main() -> int:
    needle = sys.argv[1] if len(sys.argv) > 1 else "Live-Status"
    out = Path(sys.argv[2] if len(sys.argv) > 2 else "gl-window.png")

    user32.SetProcessDPIAware()  # client rect in physical pixels
    hwnd = find_window(needle)
    if not hwnd:
        print("window-not-found:", needle)
        return 1

    attempts = []
    best = None
    for name, capture in (("printwindow-fullcontent", capture_printwindow),
                          ("gdi-screen-grab", capture_screen_rect)):
        try:
            img = capture(hwnd)
        except OSError as error:
            attempts.append(f"{name}: error {error}")
            continue
        if img is None:
            attempts.append(f"{name}: failed")
            continue
        distinct, share = bitmap_metrics(img)
        flat = share >= FLAT_TOP_COLOR_SHARE and distinct <= FLAT_DISTINCT_COLORS
        attempts.append(f"{name}: {distinct} colors, top color {share:.1%}, flat={flat}")
        if best is None:
            best = (img, flat, name)
        if not flat:
            best = (img, False, name)
            break

    if best is None:
        print("; ".join(attempts))
        print("capture-failed:", needle)
        return 2

    img, flat, method = best
    img.save(out)
    print(f"captured {needle} {img.width}x{img.height} -> {out} via {method}")
    print("; ".join(attempts))
    if flat:
        print("flat-image: every method returned a near-monochrome capture;")
        print("the window may genuinely be unrendered - check the app itself.")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
