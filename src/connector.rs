// ponytail: primary bottom taskbar only; add per-monitor strips when secondary taskbar connectors are requested.

use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{s, w, Error, Result, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

const CLASS: PCWSTR = w!("TransparentBar.Connector");
// Base sizes are device pixels at 96 DPI; Metrics scales them per monitor.
const BASE_SEAM_OVERLAP: i32 = 2;
const BASE_SAMPLE_ROWS: i32 = 4;
const BASE_BLUR_RADIUS: i32 = 8;
const BASE_STRIP_HEIGHT: i32 = 42;
const BASE_FRAME_OVERHANG: i32 = 14;
const BASE_EDGE_SLACK: i32 = 4;
const TOP_ALPHA: f32 = 0.90;
const FALLOFF: f32 = 1.65;
static CLASS_READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Metrics {
    pub strip: i32,
    pub seam: i32,
    pub sample_rows: i32,
    pub blur: usize,
    pub overhang: i32,
    pub slack: i32,
}

pub(crate) fn metrics(dpi: u32) -> Metrics {
    let dpi: i32 = if dpi == 0 { 96 } else { dpi.min(1920) as i32 };
    let scale = |base: i32| (base * dpi + 48) / 96;
    Metrics {
        strip: scale(BASE_STRIP_HEIGHT),
        seam: scale(BASE_SEAM_OVERLAP),
        sample_rows: scale(BASE_SAMPLE_ROWS).max(1),
        blur: scale(BASE_BLUR_RADIUS).max(1) as usize,
        overhang: scale(BASE_FRAME_OVERHANG),
        slack: scale(BASE_EDGE_SLACK),
    }
}

/// Monitor DPI of a window; GetDpiForWindow needs Windows 10 1607, so it is resolved at runtime.
fn dpi_for_window(hwnd: HWND) -> u32 {
    unsafe {
        if let Ok(user32) = GetModuleHandleW(w!("user32.dll")) {
            if let Some(address) = GetProcAddress(user32, s!("GetDpiForWindow")) {
                let get: unsafe extern "system" fn(HWND) -> u32 = std::mem::transmute(address);
                let dpi = get(hwnd);
                if dpi != 0 {
                    return dpi;
                }
            }
        }
        let screen = GetDC(None);
        let dpi = GetDeviceCaps(screen, LOGPIXELSX);
        let _ = ReleaseDC(None, screen);
        u32::try_from(dpi).unwrap_or(96)
    }
}

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
        let m = metrics(dpi_for_window(taskbar));
        let height = m.strip.min(bar.bottom - bar.top + m.seam);
        if width <= 0 || height < 8 || maximised_above(taskbar, &bar, &m).is_none() {
            self.hide();
            return Ok(false);
        }
        let rect = RECT {
            left: bar.left,
            top: bar.top - m.seam,
            right: bar.right,
            bottom: bar.top - m.seam + height,
        };
        if self.last_rect.is_some_and(|previous| previous != rect) {
            self.hide(); // The old strip must not overlap a relocated capture row.
        }
        let sample = Dib::resize(&mut self.sample, width, m.sample_rows)?;
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
                m.sample_rows,
                screen,
                rect.left,
                sample_top(rect.top, m.sample_rows),
                SRCCOPY,
            )
        };
        let _ = unsafe { ReleaseDC(None, screen) };
        capture?;
        let columns = smooth_scanline(
            sample.pixels()?,
            width as usize,
            m.sample_rows as usize,
            m.blur,
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

fn sample_top(overlay_top: i32, rows: i32) -> i32 {
    overlay_top - rows
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

pub(crate) fn is_docked_above_taskbar(window: &RECT, bar: &RECT, m: &Metrics) -> bool {
    let bar_height = bar.bottom - bar.top;
    if bar_height <= 0 || window.left >= bar.right || window.right <= bar.left {
        return false;
    }
    // A maximized window's invisible resize frame grows with DPI, so the tolerance scales too.
    let overhang = m.overhang.min(bar_height / 2);
    window.bottom >= bar.top - m.slack && window.bottom <= bar.top + overhang
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, message, wp, lp)
}

fn maximised_above(taskbar: HWND, bar: &RECT, m: &Metrics) -> Option<RECT> {
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
            || info.rcWork.bottom > bar.top + m.overhang
            || sample_top(bar.top - m.seam, m.sample_rows) < info.rcMonitor.top
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
        is_docked_above_taskbar(&rect, bar, m).then_some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_stays_above_overlay_including_negative_coordinates() {
        for top in [1030, 30, -52] {
            for dpi in [96, 120, 144, 192] {
                let rows = metrics(dpi).sample_rows;
                assert_eq!(sample_top(top, rows) + rows, top);
                assert!(sample_top(top, rows) < top);
            }
        }
    }

    #[test]
    fn metrics_follow_display_scaling() {
        assert_eq!(metrics(0), metrics(96));
        let base = metrics(96);
        assert_eq!(
            (base.strip, base.seam, base.sample_rows, base.blur),
            (BASE_STRIP_HEIGHT, BASE_SEAM_OVERLAP, BASE_SAMPLE_ROWS, 8)
        );
        let mut previous = base;
        for dpi in [120, 144, 192, 288] {
            let scaled = metrics(dpi);
            assert!(scaled.strip > previous.strip && scaled.overhang >= previous.overhang);
            assert!(scaled.sample_rows >= previous.sample_rows);
            previous = scaled;
        }
        assert_eq!(metrics(192).strip, BASE_STRIP_HEIGHT * 2);
    }

    #[test]
    fn maximized_frame_is_docked_at_every_scale_and_fullscreen_is_not() {
        for dpi in [96u32, 120, 144, 168, 192, 288] {
            let m = metrics(dpi);
            let px = |base: i32| base * dpi as i32 / 96;
            let bar = RECT {
                left: 0,
                top: 1000,
                right: 2560,
                bottom: 1000 + px(48),
            };
            // Windows draws maximized frames about 8 px (at 96 DPI) past the work area.
            let maximized = RECT {
                left: -px(8),
                top: -px(8),
                right: 2560 + px(8),
                bottom: bar.top + px(8),
            };
            let fullscreen = RECT {
                bottom: bar.bottom,
                ..maximized
            };
            let floating = RECT {
                bottom: bar.top - px(60),
                ..maximized
            };
            assert!(is_docked_above_taskbar(&maximized, &bar, &m), "dpi {dpi}");
            assert!(!is_docked_above_taskbar(&fullscreen, &bar, &m), "dpi {dpi}");
            assert!(!is_docked_above_taskbar(&floating, &bar, &m), "dpi {dpi}");
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
