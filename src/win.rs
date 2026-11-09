use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result};

pub const WM_APP_CLIP: u32 = WM_APP + 1;
pub const WM_APP_OPEN: u32 = WM_APP + 2;
pub const WM_APP_TRAY: u32 = WM_APP + 3;
pub const WM_APP_DOORBELL: u32 = WM_APP + 4;
pub const WM_APP_KEY: u32 = WM_APP + 5;
pub const WM_APP_QUIT: u32 = WM_APP + 6;
pub const WM_MOUSELEAVE: u32 = 0x02A3;
pub const NIN_SELECT: u32 = 0x0400;
pub const NIN_KEYSELECT: u32 = 0x0401;

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

pub fn post(hwnd: isize, message: u32) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), message, WPARAM(0), LPARAM(0));
    }
}

pub fn message_window(class: PCWSTR, proc: WNDPROC) -> Result<HWND> {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: proc, hInstance: instance(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance()),
            None,
        )
    }
}

#[derive(Clone, Copy)]
pub struct Display {
    pub work: RECT,
    pub dpi: u32,
}

pub fn active_display(follow_cursor: bool) -> Display {
    unsafe {
        let mut point = POINT::default();
        if follow_cursor {
            let _ = GetCursorPos(&mut point);
        }
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        Display { work: info.rcWork, dpi: dpi_x.max(96) }
    }
}
