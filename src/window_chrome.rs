use crate::window_settings;
use eframe::egui;
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongW, WS_EX_LAYERED,
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
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as i32);
        let alpha = ((u16::from(opacity) * 255 + 50) / 100) as u8;
        let _ = SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
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
