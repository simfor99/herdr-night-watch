use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
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

/// Shows or hides the live window's taskbar tab without changing its native
/// frame, decoration, size, or position.
pub fn set_visible(hwnd: HWND, visible: bool) -> bool {
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
