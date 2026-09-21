use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongW, SetWindowLongW, SetWindowLongPtrW, SetWindowPos, GWLP_HWNDPARENT,
    GWL_EXSTYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows_sys::core::{GUID, HRESULT};

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
struct ITaskbarListVtbl {
    parent: IUnknownVtbl,
    hr_init: unsafe extern "system" fn(this: *mut ITaskbarList) -> HRESULT,
    add_tab: unsafe extern "system" fn(this: *mut ITaskbarList, hwnd: HWND) -> HRESULT,
    delete_tab: unsafe extern "system" fn(this: *mut ITaskbarList, hwnd: HWND) -> HRESULT,
    activate_tab: unsafe extern "system" fn(this: *mut ITaskbarList, hwnd: HWND) -> HRESULT,
    set_active_alt: unsafe extern "system" fn(this: *mut ITaskbarList, hwnd: HWND) -> HRESULT,
}

#[repr(C)]
struct ITaskbarList {
    vtbl: *const ITaskbarListVtbl,
}

const CLSID_TASKBAR_LIST: GUID = GUID {
    data1: 0x56fdf344,
    data2: 0xfd6d,
    data3: 0x11d0,
    data4: [0x95, 0x8a, 0x00, 0x60, 0x97, 0xc9, 0xa0, 0x90],
};

const IID_TASKBAR_LIST: GUID = GUID {
    data1: 0x56fdf342,
    data2: 0xfd6d,
    data3: 0x11d0,
    data4: [0x95, 0x8a, 0x00, 0x60, 0x97, 0xc9, 0xa0, 0x90],
};

const S_OK: HRESULT = 0;
const RPC_E_CHANGED_MODE: HRESULT = -2147417850;

pub fn target_taskbar_ex_style(current_ex: u32, visible: bool) -> u32 {
    if visible {
        (current_ex & !WS_EX_TOOLWINDOW) | WS_EX_APPWINDOW
    } else {
        (current_ex | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW
    }
}

pub fn target_satellite_ex_style(current_ex: u32) -> u32 {
    (current_ex | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW
}

pub fn check_taskbar_style_synced(current_ex: u32, visible: bool) -> bool {
    let has_tool = (current_ex & WS_EX_TOOLWINDOW) != 0;
    let has_app = (current_ex & WS_EX_APPWINDOW) != 0;
    if visible {
        !has_tool && has_app
    } else {
        has_tool && !has_app
    }
}

pub fn check_satellite_exempt_synced(current_ex: u32) -> bool {
    let has_tool = (current_ex & WS_EX_TOOLWINDOW) != 0;
    let has_app = (current_ex & WS_EX_APPWINDOW) != 0;
    has_tool && !has_app
}

/// Shows or hides the live window's taskbar tab and synchronizes its extended
/// window styles (WS_EX_TOOLWINDOW / WS_EX_APPWINDOW) so that the Windows Shell
/// does not restore the taskbar button on activation or redraw.
pub fn set_visible(hwnd: HWND, visible: bool) -> bool {
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        let current_ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let target_ex = target_taskbar_ex_style(current_ex, visible);

        if target_ex != current_ex {
            SetWindowLongW(hwnd, GWL_EXSTYLE, target_ex as i32);
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
    set_tab_visible(hwnd, visible) || is_taskbar_style_synced(hwnd, visible)
}

pub fn is_taskbar_style_synced(hwnd: HWND, visible: bool) -> bool {
    if hwnd.is_null() {
        return true;
    }
    unsafe {
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        check_taskbar_style_synced(ex_style, visible)
    }
}

pub fn is_satellite_exempt_synced(sat_hwnd: HWND) -> bool {
    if sat_hwnd.is_null() {
        return true;
    }
    unsafe {
        let ex_style = GetWindowLongW(sat_hwnd, GWL_EXSTYLE) as u32;
        check_satellite_exempt_synced(ex_style)
    }
}

pub fn set_satellite_exempt(sat_hwnd: HWND, owner_hwnd: Option<HWND>) -> bool {
    if sat_hwnd.is_null() {
        return false;
    }
    unsafe {
        let current_ex = GetWindowLongW(sat_hwnd, GWL_EXSTYLE) as u32;
        let target_ex = target_satellite_ex_style(current_ex);

        if target_ex != current_ex {
            SetWindowLongW(sat_hwnd, GWL_EXSTYLE, target_ex as i32);
            SetWindowPos(
                sat_hwnd,
                ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }

        if let Some(owner) = owner_hwnd {
            if !owner.is_null() {
                SetWindowLongPtrW(sat_hwnd, GWLP_HWNDPARENT, owner as isize);
            }
        }
    }
    set_tab_visible(sat_hwnd, false) || is_satellite_exempt_synced(sat_hwnd)
}

fn set_tab_visible(hwnd: HWND, visible: bool) -> bool {
    unsafe {
        let init_hr = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
        if init_hr < 0 && init_hr != RPC_E_CHANGED_MODE {
            return false;
        }
        let uninitialize = init_hr >= 0;

        let result = set_visible_inner(hwnd, visible);

        if uninitialize {
            CoUninitialize();
        }
        result
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn set_visible_inner(hwnd: HWND, visible: bool) -> bool {
    let mut taskbar: *mut ITaskbarList = ptr::null_mut();
    let hr = CoCreateInstance(
        &CLSID_TASKBAR_LIST,
        ptr::null_mut(),
        CLSCTX_ALL,
        &IID_TASKBAR_LIST,
        &mut taskbar as *mut _ as *mut *mut c_void,
    );
    if hr != S_OK || taskbar.is_null() {
        return false;
    }

    let initialized = ((*(*taskbar).vtbl).hr_init)(taskbar) == S_OK;
    let changed = if initialized {
        if visible {
            ((*(*taskbar).vtbl).add_tab)(taskbar, hwnd) == S_OK
        } else {
            ((*(*taskbar).vtbl).delete_tab)(taskbar, hwnd) == S_OK
        }
    } else {
        false
    };

    let release = (*(*taskbar).vtbl).parent.release;
    release(taskbar.cast());
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_ex_style_switching() {
        let initial_winit = WS_EX_APPWINDOW;
        // When hiding from taskbar:
        let hidden = target_taskbar_ex_style(initial_winit, false);
        assert_eq!(hidden & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
        assert_eq!(hidden & WS_EX_APPWINDOW, 0);
        assert!(check_taskbar_style_synced(hidden, false));
        assert!(!check_taskbar_style_synced(hidden, true));

        // When showing in taskbar:
        let shown = target_taskbar_ex_style(hidden, true);
        assert_eq!(shown & WS_EX_TOOLWINDOW, 0);
        assert_eq!(shown & WS_EX_APPWINDOW, WS_EX_APPWINDOW);
        assert!(check_taskbar_style_synced(shown, true));
        assert!(!check_taskbar_style_synced(shown, false));
    }

    #[test]
    fn taskbar_ex_style_preserves_other_flags() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{WS_EX_LAYERED, WS_EX_TOPMOST};
        let complex = WS_EX_APPWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST;
        let hidden = target_taskbar_ex_style(complex, false);
        assert_eq!(hidden & WS_EX_LAYERED, WS_EX_LAYERED);
        assert_eq!(hidden & WS_EX_TOPMOST, WS_EX_TOPMOST);
        assert_eq!(hidden & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
        assert_eq!(hidden & WS_EX_APPWINDOW, 0);
    }

    #[test]
    fn satellite_ex_style_always_exempt() {
        let default_style = WS_EX_APPWINDOW;
        let sat_style = target_satellite_ex_style(default_style);
        assert_eq!(sat_style & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
        assert_eq!(sat_style & WS_EX_APPWINDOW, 0);
        assert!(check_satellite_exempt_synced(sat_style));
    }
}



