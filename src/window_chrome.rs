use crate::window_settings;
use eframe::egui;
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, WS_EX_LAYERED, WS_EX_TOPMOST,
};

const BG_TOP: egui::Color32 = egui::Color32::from_rgb(26, 34, 54);
const BG_BOTTOM: egui::Color32 = egui::Color32::from_rgb(14, 19, 33);

pub fn window_level(level: window_settings::WindowLevel) -> egui::WindowLevel {
    match level {
        window_settings::WindowLevel::Normal => egui::WindowLevel::Normal,
        window_settings::WindowLevel::AlwaysOnTop => egui::WindowLevel::AlwaysOnTop,
        window_settings::WindowLevel::AlwaysOnBottom => egui::WindowLevel::AlwaysOnBottom,
    }
}

pub fn apply_window_transparency(transparency: u8, title: &str) {
    apply_window_opacity(100u8.saturating_sub(transparency), title);
}

pub fn apply_window_transparency_hwnd(hwnd: HWND, transparency: u8) {
    apply_window_opacity_hwnd(hwnd, 100u8.saturating_sub(transparency));
}

pub fn apply_window_opacity(opacity: u8, title: &str) {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut search = WindowSearch {
        title,
        process_id: unsafe { GetCurrentProcessId() },
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_owned_window_callback),
            &mut search as *mut _ as LPARAM,
        );
    }
    let Some(hwnd) = search.found else {
        return;
    };
    apply_window_opacity_hwnd(hwnd, opacity);
}

#[cfg(windows)]
pub fn apply_window_level_hwnd(hwnd: HWND, level: window_settings::WindowLevel) {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE,
        };
        let insert_after = match level {
            window_settings::WindowLevel::AlwaysOnTop => HWND_TOPMOST,
            window_settings::WindowLevel::AlwaysOnBottom => HWND_BOTTOM,
            window_settings::WindowLevel::Normal => HWND_NOTOPMOST,
        };
        let _ = SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(not(windows))]
pub fn apply_window_level_hwnd(_hwnd: HWND, _level: window_settings::WindowLevel) {}

#[cfg(windows)]
pub fn ensure_window_chrome_synced(hwnd: HWND, opacity: u8, level: window_settings::WindowLevel) {
    unsafe {
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let has_layered = (ex_style & (WS_EX_LAYERED as i32)) != 0;
        let should_have_layered = opacity < 100;

        let has_topmost = (ex_style & (WS_EX_TOPMOST as i32)) != 0;
        let should_have_topmost = level == window_settings::WindowLevel::AlwaysOnTop;

        if has_layered != should_have_layered {
            apply_window_opacity_hwnd(hwnd, opacity);
        }

        if has_topmost != should_have_topmost {
            apply_window_level_hwnd(hwnd, level);
        }
    }
}

#[cfg(not(windows))]
pub fn ensure_window_chrome_synced(_hwnd: HWND, _opacity: u8, _level: window_settings::WindowLevel) {}

#[cfg(windows)]
pub fn apply_window_opacity_hwnd(hwnd: HWND, opacity: u8) {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        if let Some(alpha) = layered_window_alpha(opacity) {
            let target_style = style | WS_EX_LAYERED as i32;
            if target_style != style {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, target_style);
                let _ = SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
            let _ = SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
            return;
        }
        // Glow/OpenGL cannot present into a layered HWND. At full opacity the
        // layered bit must be cleared, otherwise the first frame flashes and
        // the window stays white.
        let cleared = style & !(WS_EX_LAYERED as i32);
        if cleared != style {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, cleared);
            let _ = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }
}

#[cfg(not(windows))]
pub fn apply_window_opacity_hwnd(_hwnd: HWND, _opacity: u8) {}

pub(crate) fn layered_window_alpha(opacity: u8) -> Option<u8> {
    if opacity >= 100 {
        None
    } else {
        Some(((u16::from(opacity) * 255 + 50) / 100) as u8)
    }
}

#[allow(dead_code)]
pub(crate) fn layered_window_alpha_from_transparency(transparency: u8) -> Option<u8> {
    if transparency == 0 {
        None
    } else {
        layered_window_alpha(100u8.saturating_sub(transparency))
    }
}

struct WindowSearch {
    title: Vec<u16>,
    process_id: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn find_owned_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam as *mut WindowSearch);
        if search.found.is_some() {
            return 0;
        }
        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id != search.process_id {
            return 1;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return 1;
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if title[..copied as usize] == search.title[..search.title.len() - 1] {
            search.found = Some(hwnd);
            return 0;
        }
    }
    1
}

pub fn paint_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    top: egui::Color32,
    bottom: egui::Color32,
) {
    let mesh = egui::epaint::Mesh {
        vertices: vec![
            egui::epaint::Vertex {
                pos: rect.left_top(),
                uv: egui::Pos2::ZERO,
                color: top,
            },
            egui::epaint::Vertex {
                pos: rect.right_top(),
                uv: egui::Pos2::ZERO,
                color: top,
            },
            egui::epaint::Vertex {
                pos: rect.right_bottom(),
                uv: egui::Pos2::ZERO,
                color: bottom,
            },
            egui::epaint::Vertex {
                pos: rect.left_bottom(),
                uv: egui::Pos2::ZERO,
                color: bottom,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    painter.add(egui::epaint::Shape::Mesh(std::sync::Arc::new(mesh)));
}

pub fn default_gradient(painter: &egui::Painter, rect: egui::Rect) {
    paint_gradient(painter, rect, BG_TOP, BG_BOTTOM);
}

pub fn glass_sheen(painter: &egui::Painter, rect: egui::Rect) {
    let inset = 12.0_f32.min(rect.width() / 4.0);
    let band_bottom = (rect.top() + rect.height() * 0.18).min(rect.bottom() - 1.0);
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 1.0, band_bottom),
    );
    painter.rect_filled(
        band,
        egui::CornerRadius {
            nw: 9,
            ne: 9,
            sw: 0,
            se: 0,
        },
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + inset, rect.top() + 1.5),
            egui::pos2(rect.right() - inset, rect.top() + 1.5),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
        ),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + inset + 8.0, rect.top() + 3.0),
            egui::pos2(rect.right() - inset - 8.0, rect.top() + 3.0),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
        ),
    );
}

#[cfg(windows)]
static PREV_WNDPROCS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<isize, isize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(windows)]
unsafe extern "system" fn borderless_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, WM_NCCALCSIZE, WM_NCPAINT,
    };
    if msg == WM_NCCALCSIZE {
        // Returning 0 indicates that the client area covers the entire window rectangle.
        // This eliminates the 1px non-client top border inserted by winit's default handler.
        return 0;
    }
    if msg == WM_NCPAINT {
        // Suppress any non-client border painting by Windows/DWM.
        return 0;
    }
    if msg == windows_sys::Win32::UI::WindowsAndMessaging::WM_GETMINMAXINFO {
        let prev = {
            let map = PREV_WNDPROCS.lock().unwrap();
            map.get(&(hwnd as isize)).copied()
        };
        let res = unsafe {
            let r = if let Some(prev_addr) = prev {
                let prev_fn: windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC =
                    std::mem::transmute(prev_addr);
                CallWindowProcW(prev_fn, hwnd, msg, wparam, lparam)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            };
            let mmi = lparam as *mut windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO;
            if !mmi.is_null() {
                (*mmi).ptMinTrackSize.x = 50;
                (*mmi).ptMinTrackSize.y = 50;
                (*mmi).ptMaxTrackSize.x = 10000;
                (*mmi).ptMaxTrackSize.y = 10000;
            }
            r
        };
        return res;
    }

    let prev = {
        let map = PREV_WNDPROCS.lock().unwrap();
        map.get(&(hwnd as isize)).copied()
    };

    unsafe {
        if let Some(prev_addr) = prev {
            let prev_fn: windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC =
                std::mem::transmute(prev_addr);
            CallWindowProcW(prev_fn, hwnd, msg, wparam, lparam)
        } else {
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

#[cfg(windows)]
pub fn strip_system_frame(hwnd: HWND) {
    unsafe {
        use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowLongW, SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC,
            GWL_STYLE, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_BORDER,
            WS_CAPTION, WS_THICKFRAME,
        };
        // DWMWA_BORDER_COLOR = 34, DWMWA_COLOR_NONE = 0xFFFFFFFE
        let none_color: u32 = 0xFFFFFFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            34,
            &none_color as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );

        let mut needs_frame_change = false;

        // Subclass WndProc to intercept WM_NCCALCSIZE
        {
            let mut map = PREV_WNDPROCS.lock().unwrap();
            let key = hwnd as isize;
            if !map.contains_key(&key) {
                let prev = SetWindowLongPtrW(
                    hwnd,
                    GWLP_WNDPROC,
                    borderless_wndproc as *const () as isize,
                );
                if prev != 0 {
                    map.insert(key, prev);
                    needs_frame_change = true;
                }
            }
        }

        let style = GetWindowLongW(hwnd, GWL_STYLE);
        let border_flags = (WS_CAPTION | WS_BORDER | WS_THICKFRAME) as i32;
        if (style & border_flags) != 0 {
            let cleared = style & !border_flags;
            let _ = SetWindowLongW(hwnd, GWL_STYLE, cleared);
            needs_frame_change = true;
        }

        if needs_frame_change {
            let _ = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }
}

#[cfg(not(windows))]
pub fn strip_system_frame(_hwnd: HWND) {}

#[cfg(windows)]
pub fn apply_corner_preference(
    hwnd: HWND,
    radius: u8,
    is_satellite: bool,
    is_docked: bool,
    _scale: f32,
) {
    strip_system_frame(hwnd);
    unsafe {
        use windows_sys::Win32::Graphics::Gdi::{
            CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn, RGN_OR,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
        use windows_sys::Win32::Foundation::RECT;

        if radius == 0 {
            SetWindowRgn(hwnd, std::ptr::null_mut(), 1);
            return;
        }

        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return;
        }

        let ratio = width as f32 / 393.0;
        let phys_r = (radius as f32 * ratio).round().max(1.0) as i32;

        if !is_docked {
            let rgn = CreateRoundRectRgn(0, 0, width + 1, height + 1, phys_r * 2, phys_r * 2);
            SetWindowRgn(hwnd, rgn, 1);
        } else if !is_satellite {
            // Main window docked: top rounded, bottom flat
            let rgn_round = CreateRoundRectRgn(0, 0, width + 1, height + 1, phys_r * 2, phys_r * 2);
            let rgn_bottom = CreateRectRgn(0, height / 2, width + 1, height + 1);
            let rgn_combined = CreateRectRgn(0, 0, 0, 0);
            CombineRgn(rgn_combined, rgn_round, rgn_bottom, RGN_OR);
            DeleteObject(rgn_round as _);
            DeleteObject(rgn_bottom as _);
            SetWindowRgn(hwnd, rgn_combined, 1);
        } else {
            // Satellite window docked: top flat, bottom rounded
            let rgn_top = CreateRectRgn(0, 0, width + 1, height / 2);
            let rgn_round = CreateRoundRectRgn(0, 0, width + 1, height + 1, phys_r * 2, phys_r * 2);
            let rgn_combined = CreateRectRgn(0, 0, 0, 0);
            CombineRgn(rgn_combined, rgn_top, rgn_round, RGN_OR);
            DeleteObject(rgn_top as _);
            DeleteObject(rgn_round as _);
            SetWindowRgn(hwnd, rgn_combined, 1);
        }
    }
}

#[cfg(not(windows))]
pub fn apply_corner_preference(
    _hwnd: HWND,
    _radius: u8,
    _is_satellite: bool,
    _is_docked: bool,
    _scale: f32,
) {}

#[cfg(windows)]
pub fn sync_satellite_size(
    hwnd: HWND,
    design_height: f32,
    main_hwnd: Option<HWND>,
    radius: u8,
    is_docked: bool,
) {
    strip_system_frame(hwnd);
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowRect, SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
        };
        use windows_sys::Win32::Foundation::RECT;

        let mut width = 0i32;
        if let Some(main) = main_hwnd {
            let mut main_rect: RECT = std::mem::zeroed();
            if GetWindowRect(main, &mut main_rect) != 0 {
                width = main_rect.right - main_rect.left;
            }
        }
        if width <= 0 {
            let mut sat_rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut sat_rect) != 0 {
                width = sat_rect.right - sat_rect.left;
            }
        }
        if width <= 0 {
            width = 393;
        }

        let ratio = width as f32 / 393.0;
        let phys_h = (design_height * ratio).round().max(50.0) as i32;

        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            width,
            phys_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    apply_corner_preference(hwnd, radius, true, is_docked, 1.0);
}

#[cfg(not(windows))]
pub fn sync_satellite_size(
    _hwnd: HWND,
    _design_height: f32,
    _main_hwnd: Option<HWND>,
    _radius: u8,
    _is_docked: bool,
) {}

#[cfg(test)]
mod tests {
    use super::layered_window_alpha;

    #[test]
    fn full_opacity_does_not_use_layered_style() {
        assert_eq!(layered_window_alpha(100), None);
        assert_eq!(layered_window_alpha(101), None);
    }

    #[test]
    fn reduced_opacity_uses_layered_alpha() {
        assert_eq!(layered_window_alpha(90), Some(230));
        assert_eq!(layered_window_alpha(50), Some(128));
        assert_eq!(layered_window_alpha(10), Some(26));
    }

    #[test]
    fn window_level_mapping() {
        use super::window_level;
        use crate::window_settings::WindowLevel;
        assert_eq!(window_level(WindowLevel::Normal), eframe::egui::WindowLevel::Normal);
        assert_eq!(window_level(WindowLevel::AlwaysOnTop), eframe::egui::WindowLevel::AlwaysOnTop);
        assert_eq!(window_level(WindowLevel::AlwaysOnBottom), eframe::egui::WindowLevel::AlwaysOnBottom);
    }

    #[test]
    fn transparency_alpha_mapping() {
        use super::layered_window_alpha_from_transparency;
        assert_eq!(layered_window_alpha_from_transparency(0), None);
        assert_eq!(layered_window_alpha_from_transparency(10), Some(230));
        assert_eq!(layered_window_alpha_from_transparency(50), Some(128));
        assert_eq!(layered_window_alpha_from_transparency(90), Some(26));
    }
}
