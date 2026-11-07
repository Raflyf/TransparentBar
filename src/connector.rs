// ponytail: single-monitor gradient strip over Shell_TrayWnd; add per-monitor strips when multi-monitor connectors are requested.

use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

const CLASS: PCWSTR = w!("TransparentBar.Connector");
const SEAM_OVERLAP: i32 = 2;
const STRIP_HEIGHT: i32 = 42;
const TOP_ALPHA: f32 = 0.90;
const FALLOFF: f32 = 1.65;
static CLASS_READY: AtomicBool = AtomicBool::new(false);

pub struct Connector {
    hwnd: HWND,
}

impl Connector {
    pub fn create() -> Result<Self> {
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            if !CLASS_READY.load(Ordering::Acquire) {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    return Err(Error::from_win32());
                }
                CLASS_READY.store(true, Ordering::Release);
            }
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("TransparentBar connector"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                instance,
                None,
            )?;
            Ok(Self { hwnd })
        }
    }

    fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Recomputes the strip. Cosmetic only, so callers treat failure as non-fatal.
    pub fn update(&self, enabled: bool) -> Result<()> {
        if !enabled {
            self.hide();
            return Ok(());
        }
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) };
        let taskbar = match taskbar {
            Ok(hwnd) if !hwnd.is_invalid() => hwnd,
            _ => {
                self.hide();
                return Ok(());
            }
        };
        let mut bar = RECT::default();
        if unsafe { GetWindowRect(taskbar, &mut bar) }.is_err() {
            self.hide();
            return Ok(());
        }
        let width = bar.right - bar.left;
        let height = STRIP_HEIGHT.min(bar.bottom - bar.top + SEAM_OVERLAP);
        if width <= 0 || height < 8 || maximised_above(&bar).is_none() {
            self.hide();
            return Ok(());
        }
        let (red, green, blue) = unsafe {
            let screen = GetDC(None);
            let sample = GetPixel(screen, bar.left + width / 2, (bar.top - 4).max(0));
            let _ = ReleaseDC(None, screen);
            decode_colorref(sample.0)
        };
        self.paint(width, height, red, green, blue)?;
        unsafe {
            let insert_after = match GetWindow(taskbar, GW_HWNDPREV) {
                Ok(prev) if !prev.is_invalid() && prev != self.hwnd => prev,
                _ => HWND_TOPMOST,
            };
            SetWindowPos(
                self.hwnd,
                insert_after,
                bar.left,
                (bar.top - SEAM_OVERLAP).max(0),
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
        }
        Ok(())
    }

    fn paint(&self, width: i32, height: i32, red: u8, green: u8, blue: u8) -> Result<()> {
        unsafe {
            let mut info = BITMAPINFO::default();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB.0;
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let screen = GetDC(None);
            let dib = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let memory = CreateCompatibleDC(screen);
            let _ = ReleaseDC(None, screen);
            let dib = dib?;
            if memory.is_invalid() || bits.is_null() {
                let _ = DeleteObject(dib);
                return Err(Error::from_win32());
            }
            let previous = SelectObject(memory, dib);
            let pixels = std::slice::from_raw_parts_mut(
                bits.cast::<u8>(),
                (width as usize) * (height as usize) * 4,
            );
            fill_gradient(pixels, width as usize, height as usize, red, green, blue);
            let size = SIZE {
                cx: width,
                cy: height,
            };
            let origin = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let window = GetDC(self.hwnd);
            let result = UpdateLayeredWindow(
                self.hwnd,
                window,
                None,
                Some(&size),
                memory,
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            let _ = ReleaseDC(self.hwnd, window);
            let _ = SelectObject(memory, previous);
            let _ = DeleteObject(dib);
            let _ = DeleteDC(memory);
            result
        }
    }
}

impl Drop for Connector {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub(crate) fn decode_colorref(raw: u32) -> (u8, u8, u8) {
    if raw == u32::MAX {
        (17, 16, 14)
    } else {
        (
            (raw & 0xff) as u8,
            ((raw >> 8) & 0xff) as u8,
            ((raw >> 16) & 0xff) as u8,
        )
    }
}

pub(crate) fn fill_gradient(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    red: u8,
    green: u8,
    blue: u8,
) {
    let span = height.saturating_sub(1).max(1) as f32;
    for row in 0..height {
        let falloff = 1.0 - row as f32 / span;
        let alpha = (TOP_ALPHA * falloff.powf(FALLOFF) * 255.0).round() as u32;
        let b = (u32::from(blue) * alpha / 255) as u8;
        let g = (u32::from(green) * alpha / 255) as u8;
        let r = (u32::from(red) * alpha / 255) as u8;
        let a = alpha as u8;
        for column in 0..width {
            let at = (row * width + column) * 4;
            pixels[at] = b;
            pixels[at + 1] = g;
            pixels[at + 2] = r;
            pixels[at + 3] = a;
        }
    }
}

pub(crate) fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Progman"
            | "WorkerW"
            | "Windows.UI.Core.CoreWindow"
            | "TransparentBar.Connector"
    )
}

pub(crate) fn is_docked_above_taskbar(window: &RECT, bar: &RECT) -> bool {
    let bar_height = bar.bottom - bar.top;
    if bar_height <= 0 {
        return false;
    }
    let max_frame_overhang = 14.min(bar_height / 2);
    window.bottom >= bar.top - 4 && window.bottom <= bar.top + max_frame_overhang
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, message, wp, lp)
}

fn maximised_above(bar: &RECT) -> Option<RECT> {
    unsafe {
        if let Ok(state) = SHQueryUserNotificationState() {
            if matches!(
                state,
                QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_APP
            ) {
                return None;
            }
        }
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() || !IsZoomed(hwnd).as_bool() {
            return None;
        }
        let mut name = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut name);
        if length <= 0 {
            return None;
        }
        let class = String::from_utf16_lossy(&name[..length as usize]);
        if is_shell_class(&class) {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        if !is_docked_above_taskbar(&rect, bar) {
            return None;
        }
        Some(rect)
    }
}
