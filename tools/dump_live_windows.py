#!/usr/bin/env python3
"""Dump Nachtwaechter processes and their top-level windows."""

from __future__ import annotations

import ctypes
from ctypes import wintypes
import json
import subprocess
import sys

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
GWL_EXSTYLE = -20
GWL_STYLE = -16
WS_VISIBLE = 0x10000000
WS_MINIMIZE = 0x20000000
WS_EX_TOOLWINDOW = 0x00000080
WS_EX_APPWINDOW = 0x00040000
WS_EX_LAYERED = 0x00080000
WS_EX_NOACTIVATE = 0x08000000
WS_EX_TRANSPARENT = 0x00000020

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

user32.EnumWindows.argtypes = [WNDENUMPROC, wintypes.LPARAM]
user32.EnumWindows.restype = wintypes.BOOL
user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.restype = ctypes.c_int
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.IsWindowVisible.restype = wintypes.BOOL
user32.IsIconic.argtypes = [wintypes.HWND]
user32.IsIconic.restype = wintypes.BOOL
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowRect.restype = wintypes.BOOL
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetClassNameW.restype = ctypes.c_int
if ctypes.sizeof(ctypes.c_void_p) == 8:
    user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.GetWindowLongPtrW.restype = ctypes.c_longlong
    get_long = user32.GetWindowLongPtrW
else:
    user32.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.GetWindowLongW.restype = ctypes.c_long
    get_long = user32.GetWindowLongW

kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.QueryFullProcessImageNameW.argtypes = [
    wintypes.HANDLE,
    wintypes.DWORD,
    wintypes.LPWSTR,
    ctypes.POINTER(wintypes.DWORD),
]
kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.CloseHandle.restype = wintypes.BOOL


def process_path(pid: int) -> str | None:
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    try:
        length = wintypes.DWORD(512)
        buffer = ctypes.create_unicode_buffer(512)
        if not kernel32.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length)):
            return None
        return buffer.value
    finally:
        kernel32.CloseHandle(handle)


def class_name(hwnd: int) -> str:
    buffer = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, buffer, 256)
    return buffer.value


def title_of(hwnd: int) -> str:
    length = user32.GetWindowTextLengthW(hwnd)
    if length <= 0:
        return ""
    buffer = ctypes.create_unicode_buffer(length + 1)
    copied = user32.GetWindowTextW(hwnd, buffer, length + 1)
    return buffer.value[:copied]


def windows_for(pids: set[int]) -> list[dict]:
    found: list[dict] = []

    @WNDENUMPROC
    def callback(hwnd, _lparam):
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value not in pids:
            return True
        rect = wintypes.RECT()
        ok = bool(user32.GetWindowRect(hwnd, ctypes.byref(rect)))
        style = int(get_long(hwnd, GWL_STYLE))
        exstyle = int(get_long(hwnd, GWL_EXSTYLE))
        found.append(
            {
                "hwnd": int(hwnd),
                "pid": int(pid.value),
                "title": title_of(hwnd),
                "class": class_name(hwnd),
                "visible": bool(user32.IsWindowVisible(hwnd)),
                "iconic": bool(user32.IsIconic(hwnd)),
                "rect": {
                    "left": rect.left,
                    "top": rect.top,
                    "right": rect.right,
                    "bottom": rect.bottom,
                    "w": rect.right - rect.left,
                    "h": rect.bottom - rect.top,
                }
                if ok
                else None,
                "style_visible": bool(style & WS_VISIBLE),
                "style_minimize": bool(style & WS_MINIMIZE),
                "ex_tool": bool(exstyle & WS_EX_TOOLWINDOW),
                "ex_app": bool(exstyle & WS_EX_APPWINDOW),
                "ex_layered": bool(exstyle & WS_EX_LAYERED),
                "ex_noactivate": bool(exstyle & WS_EX_NOACTIVATE),
                "ex_transparent": bool(exstyle & WS_EX_TRANSPARENT),
            }
        )
        return True

    user32.EnumWindows(callback, 0)
    return found


def processes() -> list[dict]:
    raw = subprocess.check_output(
        [
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'Nacht|herdr-night' } | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress",
        ],
        text=True,
    ).strip()
    if not raw:
        return []
    data = json.loads(raw)
    if isinstance(data, dict):
        return [data]
    return data


def main() -> int:
    procs = processes()
    pids = {int(item["ProcessId"]) for item in procs}
    payload = {
        "processes": procs,
        "windows": windows_for(pids) if pids else [],
    }
    json.dump(payload, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
