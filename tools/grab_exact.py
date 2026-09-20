import ctypes
from PIL import ImageGrab

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()

class RECT(ctypes.Structure):
    _fields_ = [('left', ctypes.c_long), ('top', ctypes.c_long), ('right', ctypes.c_long), ('bottom', ctypes.c_long)]

found = []
def callback(hwnd, extra):
    if user32.IsWindowVisible(hwnd):
        length = user32.GetWindowTextLengthW(hwnd)
        title = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, title, length + 1)
        if 'Live-Status' in title.value:
            found.append((hwnd, title.value))
    return True

WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_int, ctypes.c_int)
user32.EnumWindows(WNDENUMPROC(callback), 0)

if found:
    hwnd, title = found[0]
    user32.ShowWindow(hwnd, 5) # SW_SHOW
    user32.SetForegroundWindow(hwnd)
    import time
    time.sleep(0.5)
    rect = RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    print(f"Found {title} HWND: {hwnd} rect: {rect.left} {rect.top} {rect.right} {rect.bottom}")
    pad = 20
    box = (rect.left - pad, rect.top - pad, rect.right + pad, rect.bottom + 350)
    img = ImageGrab.grab(bbox=box, all_screens=True)
    out_path = r"C:\Users\Simon\Desktop\temp\live_grab_verified.png"
    img.save(out_path)
    print("Saved to", out_path)
else:
    print("No Live-Status window found!")
