// ponytail: primary bottom taskbar only; add per-monitor strips when secondary taskbar connectors are requested.

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
const SAMPLE_ROWS: i32 = 4;
const BLUR_RADIUS: usize = 8;
const STRIP_HEIGHT: i32 = 42;
const TOP_ALPHA: f32 = 0.90;
const FALLOFF: f32 = 1.65;
static CLASS_READY: AtomicBool = AtomicBool::new(false);

struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
    len: usize,
}

impl Dib {
    fn new(width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(Error::from(E_INVALIDARG));
        }
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|size| size.checked_mul(4))
            .ok_or_else(|| Error::from(E_INVALIDARG))?;
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(Error::new(
                    E_FAIL,
                    "Could not create the connector bitmap DC.",
                ));
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) => bitmap,
                Err(error) => {
                    let _ = DeleteDC(dc);
                    return Err(error);
                }
            };
            let previous = SelectObject(dc, bitmap);
            if bits.is_null() || previous.is_invalid() {
                let _ = DeleteDC(dc);
                let _ = DeleteObject(bitmap);
                return Err(Error::new(E_FAIL, "Could not select the connector bitmap."));
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                width,
                height,
                len,
            })
        }
    }

    fn resize(slot: &mut Option<Self>, width: i32, height: i32) -> Result<&mut Self> {
        if slot
            .as_ref()
            .is_none_or(|dib| dib.width != width || dib.height != height)
        {
            *slot = Some(Self::new(width, height)?);
        }
        Ok(slot.as_mut().unwrap())
    }

    fn pixels(&mut self) -> Result<&mut [u8]> {
        // GDI batches must finish before CPU access to DIB memory.
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(Error::new(
                E_FAIL,
                "Could not synchronize the connector bitmap.",
            ));
        }
        Ok(unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) })
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bitmap);
        }
    }
}

pub struct Connector {
    hwnd: HWND,
    sample: Option<Dib>,
    image: Option<Dib>,
    last_rect: Option<RECT>,
    last_columns: Vec<(u8, u8, u8)>,
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
            Ok(Self {
                hwnd,
                sample: None,
                image: None,
                last_rect: None,
                last_columns: Vec::new(),
            })
        }
    }

    pub fn hide(&mut self) {
        unsafe {
            if IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        self.last_rect = None;
        self.last_columns.clear();
        self.sample = None;
        self.image = None;
    }

    /// True while sampling a maximized window; errors never leave a stale strip visible.
    pub fn update(&mut self, enabled: bool) -> Result<bool> {
        let result = self.update_inner(enabled);
        if result.is_err() {
            self.hide();
        }
        result
    }

    fn update_inner(&mut self, enabled: bool) -> Result<bool> {
        if !enabled {
            self.hide();
            return Ok(false);
        }
        let taskbar = match unsafe { FindWindowW(w!("Shell_TrayWnd"), None) } {
            Ok(hwnd) if !hwnd.is_invalid() => hwnd,
            _ => {
                self.hide();
                return Ok(false);
            }
        };
        let mut bar = RECT::default();
        unsafe {
            GetWindowRect(taskbar, &mut bar)?;
        }
        let width = bar.right - bar.left;
        let height = STRIP_HEIGHT.min(bar.bottom - bar.top + SEAM_OVERLAP);
        if width <= 0 || height < 8 || maximised_above(taskbar, &bar).is_none() {
            self.hide();
            return Ok(false);
        }
        let rect = RECT {
            left: bar.left,
            top: bar.top - SEAM_OVERLAP,
            right: bar.right,
            bottom: bar.top - SEAM_OVERLAP + height,
        };
        if self.last_rect.is_some_and(|previous| previous != rect) {
            self.hide(); // The old strip must not overlap a relocated capture row.
        }
        let sample = Dib::resize(&mut self.sample, width, SAMPLE_ROWS)?;
        let screen = unsafe { GetDC(None) };
        if screen.is_invalid() {
            return Err(Error::new(
                E_FAIL,
                "Could not read the visible window edge.",
            ));
        }
        let capture = unsafe {
            BitBlt(
                sample.dc,
                0,
                0,
                width,
                SAMPLE_ROWS,
                screen,
                rect.left,
                sample_top(rect.top),
                SRCCOPY,
            )
        };
        let _ = unsafe { ReleaseDC(None, screen) };
        capture?;
        let columns = smooth_scanline(
            sample.pixels()?,
            width as usize,
            SAMPLE_ROWS as usize,
            BLUR_RADIUS,
        );
        if self.last_rect != Some(rect) || self.last_columns != columns {
            let image = Dib::resize(&mut self.image, width, height)?;
            fill_gradient_columns(image.pixels()?, width as usize, height as usize, &columns);
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE {
                cx: width,
                cy: height,
            };
            let origin = POINT::default();
            let position = POINT {
                x: rect.left,
                y: rect.top,
            };
            unsafe {
                UpdateLayeredWindow(
                    self.hwnd,
                    None,
                    Some(&position),
                    Some(&size),
                    image.dc,
                    Some(&origin),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                )?;
            }
        }
        unsafe {
            let previous = GetWindow(taskbar, GW_HWNDPREV).unwrap_or_default();
            let flags = SWP_NOACTIVATE
                | SWP_SHOWWINDOW
                | if previous == self.hwnd {
                    SWP_NOZORDER
                } else {
                    SET_WINDOW_POS_FLAGS(0)
                };
            let after = if previous.is_invalid() {
                HWND_TOPMOST
            } else {
                previous
            };
            SetWindowPos(self.hwnd, after, rect.left, rect.top, width, height, flags)?;
        }
        self.last_rect = Some(rect);
        self.last_columns = columns;
        Ok(true)
    }
}

impl Drop for Connector {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn sample_top(overlay_top: i32) -> i32 {
    overlay_top - SAMPLE_ROWS
}

pub(crate) fn smooth_scanline(
    raw_bgra: &[u8],
    width: usize,
    rows: usize,
    radius: usize,
) -> Vec<(u8, u8, u8)> {
    if width == 0 || rows == 0 {
        return Vec::new();
    }
    assert_eq!(raw_bgra.len(), width * rows * 4);
    let mut prefix = vec![(0u64, 0u64, 0u64); width + 1];
    for x in 0..width {
        let (mut red, mut green, mut blue) = prefix[x];
        for row in 0..rows {
            let at = (row * width + x) * 4;
            red += u64::from(raw_bgra[at + 2]);
            green += u64::from(raw_bgra[at + 1]);
            blue += u64::from(raw_bgra[at]);
        }
        prefix[x + 1] = (red, green, blue);
    }
    (0..width)
        .map(|x| {
            let start = x.saturating_sub(radius);
            let end = x.saturating_add(radius).saturating_add(1).min(width);
            let count = ((end - start) * rows) as u64;
            let a = prefix[start];
            let b = prefix[end];
            (
                ((b.0 - a.0) / count) as u8,
                ((b.1 - a.1) / count) as u8,
                ((b.2 - a.2) / count) as u8,
            )
        })
        .collect()
}

pub(crate) fn fill_gradient_columns(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    columns: &[(u8, u8, u8)],
) {
    assert_eq!(columns.len(), width);
    assert_eq!(pixels.len(), width * height * 4);
    let span = height.saturating_sub(1).max(1) as f32;
    for row in 0..height {
        let falloff = 1.0 - row as f32 / span;
        let alpha = (TOP_ALPHA * falloff.powf(FALLOFF) * 255.0).round() as u32;
        for (x, &(red, green, blue)) in columns.iter().enumerate() {
            let at = (row * width + x) * 4;
            pixels[at..at + 4].copy_from_slice(&[
                (u32::from(blue) * alpha / 255) as u8,
                (u32::from(green) * alpha / 255) as u8,
                (u32::from(red) * alpha / 255) as u8,
                alpha as u8,
            ]);
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
    if bar_height <= 0 || window.left >= bar.right || window.right <= bar.left {
        return false;
    }
    let max_frame_overhang = 14.min(bar_height / 2);
    window.bottom >= bar.top - 4 && window.bottom <= bar.top + max_frame_overhang
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, message, wp, lp)
}

fn maximised_above(taskbar: HWND, bar: &RECT) -> Option<RECT> {
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
        let monitor = MonitorFromWindow(taskbar, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() || MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) != monitor {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool()
            || bar.bottom != info.rcMonitor.bottom
            || bar.left != info.rcMonitor.left
            || bar.right != info.rcMonitor.right
            || info.rcWork.bottom > bar.top + 14
            || sample_top(bar.top - SEAM_OVERLAP) < info.rcMonitor.top
        {
            return None;
        }
        let mut name = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut name);
        if length <= 0 || is_shell_class(&String::from_utf16_lossy(&name[..length as usize])) {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        is_docked_above_taskbar(&rect, bar).then_some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_stays_above_overlay_including_negative_coordinates() {
        for top in [1030, 30, -52] {
            assert_eq!(sample_top(top) + SAMPLE_ROWS, top);
            assert!(sample_top(top) < top);
        }
    }

    #[test]
    fn gdi_scanline_reads_current_pixels_after_each_copy() -> Result<()> {
        let mut source = Dib::new(3, 4)?;
        let mut destination = Dib::new(3, 4)?;
        for colors in [
            [(12, 24, 36), (150, 180, 210), (230, 60, 10)],
            [(20, 200, 80), (100, 50, 120), (5, 15, 25)],
        ] {
            for row in source.pixels()?.as_chunks_mut::<12>().0 {
                for (pixel, &(r, g, b)) in row.as_chunks_mut::<4>().0.iter_mut().zip(&colors) {
                    pixel.copy_from_slice(&[b, g, r, 255]);
                }
            }
            unsafe {
                BitBlt(destination.dc, 0, 0, 3, 4, source.dc, 0, 0, SRCCOPY)?;
            }
            assert_eq!(smooth_scanline(destination.pixels()?, 3, 4, 0), colors);
        }
        Ok(())
    }
}
