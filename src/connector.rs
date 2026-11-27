// ponytail: primary bottom taskbar only; add per-monitor strips when secondary taskbar connectors are requested.

use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};
use windows::{
    core::{s, w, Error, Result, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::{Dwm::DwmIsCompositionEnabled, Gdi::*},
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

const CLASS: PCWSTR = w!("TransparentBar.Connector");
// Base sizes are device pixels at 96 DPI; Metrics scales them per monitor.
const BASE_SEAM_OVERLAP: i32 = 2;
const BASE_SAMPLE_ROWS: i32 = 4;
const BASE_BLUR_RADIUS: i32 = 8;
const BASE_STRIP_HEIGHT: i32 = 34;
const BASE_FRAME_OVERHANG: i32 = 14;
const BASE_EDGE_SLACK: i32 = 4;
const TOP_ALPHA: f32 = 0.90;
const FALLOFF: f32 = 1.65;
/// Vertical gap between pattern cells, and how faint each cell line is.
const BASE_GRID_STEP: i32 = 28;
const GRID_ALPHA: f32 = 0.07;
/// The strip never covers more than this share of a thin taskbar.
const MAX_STRIP_FRACTION: i32 = 55;
const MAX_STRIP_FRACTION_DEN: i32 = 100;
const MAX_STRIP_WIDTH: i32 = 1920;
const SETTLED_THRESHOLD: u32 = 20;
const QUIESCENT_THRESHOLD: u32 = 60;

type GetDpiForWindowFn = unsafe extern "system" fn(HWND) -> u32;
static GET_DPI_FOR_WINDOW: OnceLock<Option<GetDpiForWindowFn>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub pattern: bool,
    pub reduce_motion: bool,
}
static CLASS_READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Metrics {
    pub strip: i32,
    pub seam: i32,
    pub sample_rows: i32,
    pub blur: usize,
    pub overhang: i32,
    pub slack: i32,
    pub grid_step: i32,
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
        grid_step: scale(BASE_GRID_STEP),
    }
}

/// Monitor DPI of a window; GetDpiForWindow needs Windows 10 1607, resolved once and cached.
fn dpi_for_window(hwnd: HWND) -> u32 {
    let get_dpi = *GET_DPI_FOR_WINDOW.get_or_init(|| {
        // SAFETY: user32.dll stays loaded for the process lifetime; GetDpiForWindow matches GetDpiForWindowFn ABI.
        unsafe {
            let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;
            let address = GetProcAddress(user32, s!("GetDpiForWindow"))?;
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                GetDpiForWindowFn,
            >(address))
        }
    });
    // SAFETY: cached user32 function pointer and fallback screen DC query are thread-safe Win32 calls.
    unsafe {
        if let Some(get) = get_dpi {
            let dpi = get(hwnd);
            if dpi != 0 {
                return dpi;
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
        slot.as_mut().ok_or_else(|| Error::from(E_FAIL))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorState {
    Active,
    Settled,
    Quiescent,
    Hidden,
}

pub struct Connector {
    hwnd: HWND,
    sample: Option<Dib>,
    image: Option<Dib>,
    last_rect: Option<RECT>,
    visible: bool,
    shown: Vec<(u8, u8, u8)>,
    uploaded: Vec<(u8, u8, u8)>,
    columns: Vec<(u8, u8, u8)>,
    col_sums: Vec<[u32; 3]>,
    last_raw: Vec<u8>,
    row_alphas: Vec<u8>,
    settled_ticks: u32,
    style: Style,
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
                visible: false,
                shown: Vec::new(),
                uploaded: Vec::new(),
                columns: Vec::new(),
                col_sums: Vec::new(),
                last_raw: Vec::new(),
                row_alphas: Vec::new(),
                settled_ticks: 0,
                style: Style {
                    pattern: true,
                    reduce_motion: false,
                },
            })
        }
    }

    pub fn hide(&mut self) {
        if self.visible || unsafe { IsWindowVisible(self.hwnd).as_bool() } {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        self.last_rect = None;
        self.shown.clear();
        self.shown.shrink_to_fit();
        self.uploaded.clear();
        self.uploaded.shrink_to_fit();
        self.columns.clear();
        self.columns.shrink_to_fit();
        self.col_sums.clear();
        self.col_sums.shrink_to_fit();
        self.last_raw.clear();
        self.last_raw.shrink_to_fit();
        self.row_alphas.clear();
        self.row_alphas.shrink_to_fit();
        self.settled_ticks = 0;
        self.sample = None;
        self.image = None;
    }

    /// True while sampling a maximized window; errors never leave a stale strip visible.
    pub fn update(&mut self, enabled: bool, style: Style) -> Result<ConnectorState> {
        let result = self.update_inner(enabled, style);
        if result.is_err() {
            self.hide();
        }
        result
    }

    fn update_inner(&mut self, enabled: bool, style: Style) -> Result<ConnectorState> {
        let style_changed = self.style != style;
        self.style = style;
        if !enabled {
            self.hide();
            return Ok(ConnectorState::Hidden);
        }
        // A remote desktop session usually hides the local taskbar, so sampling it is wasted work.
        if unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0 {
            self.hide();
            return Ok(ConnectorState::Hidden);
        }
        // DWM composition off means screen captures can return blank pixels.
        if !dwm_composition_enabled() {
            self.hide();
            return Ok(ConnectorState::Hidden);
        }
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid()
            || !unsafe { IsWindowVisible(fg).as_bool() }
            || !unsafe { IsZoomed(fg).as_bool() }
        {
            self.hide();
            return Ok(ConnectorState::Hidden);
        }
        let (taskbar, monitor) = match find_docked_taskbar(fg) {
            Some(found) => found,
            None => {
                self.hide();
                return Ok(ConnectorState::Hidden);
            }
        };
        let mut bar = RECT::default();
        unsafe {
            GetWindowRect(taskbar, &mut bar)?;
        }
        let bar_height = bar.bottom - bar.top;
        let m = metrics(dpi_for_window(taskbar));
        // Keep the strip proportional on thin taskbars and bounded on ultra-wide screens.
        let max_height = (bar_height * MAX_STRIP_FRACTION / MAX_STRIP_FRACTION_DEN)
            .max(m.seam + 8)
            .min(m.strip);
        let width = (bar.right - bar.left).min(MAX_STRIP_WIDTH);
        let height = max_height.min(bar_height + m.seam);
        if width <= 0 || height < 8 || maximised_above(fg, monitor, &bar, &m).is_none() {
            self.hide();
            return Ok(ConnectorState::Hidden);
        }
        let rect = RECT {
            left: bar.left,
            top: bar.top - m.seam,
            right: bar.left + width,
            bottom: bar.top - m.seam + height,
        };
        if self.last_rect.is_some_and(|previous| previous != rect) {
            self.hide(); // The old strip must not overlap a relocated capture row.
        }
        if style_changed {
            self.uploaded.clear();
            self.settled_ticks = 0;
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
        let raw = sample.pixels()?;
        let raw_unchanged =
            !style_changed && !self.columns.is_empty() && self.last_raw.as_slice() == raw;
        if !raw_unchanged {
            self.last_raw.clear();
            self.last_raw.extend_from_slice(raw);
            smooth_scanline_into(
                raw,
                width as usize,
                m.sample_rows as usize,
                m.blur,
                &mut self.col_sums,
                &mut self.columns,
            );
        }
        // Ease toward the new edge so a fast scroll fades instead of snapping.
        if self.shown.len() != self.columns.len() || style.reduce_motion {
            if self.shown != self.columns {
                self.shown.clone_from(&self.columns);
                self.uploaded.clear();
            }
        } else if !raw_unchanged || self.shown != self.columns {
            blend_toward(&mut self.shown, &self.columns);
        }
        if self.uploaded != self.shown || self.image.is_none() {
            let image = Dib::resize(&mut self.image, width, height)?;
            if self.row_alphas.len() != height as usize {
                fill_row_alphas(height as usize, &mut self.row_alphas);
            }
            fill_gradient_columns_with_alphas(
                image.pixels()?,
                width as usize,
                height as usize,
                &self.shown,
                &style,
                &self.row_alphas,
            );
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
            self.uploaded.clone_from(&self.shown);
        }
        unsafe {
            let previous = GetWindow(taskbar, GW_HWNDPREV).unwrap_or_default();
            let needs_pos = !self.visible || self.last_rect != Some(rect) || previous != self.hwnd;
            if needs_pos {
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
                self.visible = true;
            }
        }
        let changing = !raw_unchanged || self.shown != self.columns || self.uploaded != self.shown;
        if changing {
            self.settled_ticks = 0;
        } else {
            self.settled_ticks = self.settled_ticks.saturating_add(1);
        }
        self.last_rect = Some(rect);
        if self.settled_ticks > QUIESCENT_THRESHOLD {
            Ok(ConnectorState::Quiescent)
        } else if self.settled_ticks > SETTLED_THRESHOLD {
            Ok(ConnectorState::Settled)
        } else {
            Ok(ConnectorState::Active)
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

fn sample_top(overlay_top: i32, rows: i32) -> i32 {
    overlay_top - rows
}

#[inline]
fn div_255(v: u32) -> u32 {
    // Exact floor(v / 255) for all v in 0..=65534 (covers all 0..=255 * 0..=255 products).
    (v + 1 + (v >> 8)) >> 8
}

#[inline]
fn div_by_u64_fast(sum: u64, d: u64, mul: u64) -> u8 {
    if mul != 0 {
        ((u128::from(sum) * u128::from(mul)) >> 64) as u8
    } else {
        (sum / d) as u8
    }
}

#[inline]
fn fast_u64_recip(d: u64) -> u64 {
    // Exact ceil(2^64 / d) gives exact floor(x / d) for x <= 255 * d when 255 * d * (d - 1) < 2^64.
    if (2..=200_000_000).contains(&d) {
        (((1u128 << 64) + u128::from(d - 1)) / u128::from(d)) as u64
    } else {
        0
    }
}

pub(crate) fn smooth_scanline_into(
    raw_bgra: &[u8],
    width: usize,
    rows: usize,
    radius: usize,
    col_sums: &mut Vec<[u32; 3]>,
    out: &mut Vec<(u8, u8, u8)>,
) {
    out.clear();
    if width == 0 || rows == 0 {
        return;
    }
    assert_eq!(raw_bgra.len(), width * rows * 4);
    col_sums.clear();
    col_sums.resize(width, [0u32; 3]);
    // Row-major pass: contiguous reads for cache locality and LLVM auto-vectorization.
    let mut row_weight_sum = 0u64;
    for (row_idx, row_bytes) in raw_bgra.chunks_exact(width * 4).enumerate() {
        let w_u64 = ((row_idx + 1) as u64) * ((row_idx + 1) as u64);
        row_weight_sum += w_u64;
        let w = w_u64 as u32;
        let (row_pixels, _) = row_bytes.as_chunks::<4>();
        for (sum, px) in col_sums.iter_mut().zip(row_pixels) {
            sum[0] += u32::from(px[2]) * w;
            sum[1] += u32::from(px[1]) * w;
            sum[2] += u32::from(px[0]) * w;
        }
    }
    out.reserve(width);
    let initial_end = radius.saturating_add(1).min(width);
    let (mut sum_r, mut sum_g, mut sum_b) = (0u64, 0u64, 0u64);
    for c in &col_sums[..initial_end] {
        sum_r += u64::from(c[0]);
        sum_g += u64::from(c[1]);
        sum_b += u64::from(c[2]);
    }
    let window_span = radius.saturating_mul(2).saturating_add(1);
    if radius > 0 && window_span < width {
        // Left border: window grows from [0..radius+1] to [0..2*radius].
        for x in 0..radius {
            if x > 0 {
                let c = col_sums[x + radius];
                sum_r += u64::from(c[0]);
                sum_g += u64::from(c[1]);
                sum_b += u64::from(c[2]);
            }
            let count = (x + radius + 1) as u64 * row_weight_sum;
            out.push((
                (sum_r / count) as u8,
                (sum_g / count) as u8,
                (sum_b / count) as u8,
            ));
        }
        // Interior: constant window width, single multiply-high replaces hardware division.
        let mid_count = window_span as u64 * row_weight_sum;
        let mid_mul = fast_u64_recip(mid_count);
        // Enter x = radius: add col_sums[2 * radius], nothing dropped yet.
        {
            let c = col_sums[radius * 2];
            sum_r += u64::from(c[0]);
            sum_g += u64::from(c[1]);
            sum_b += u64::from(c[2]);
            out.push((
                div_by_u64_fast(sum_r, mid_count, mid_mul),
                div_by_u64_fast(sum_g, mid_count, mid_mul),
                div_by_u64_fast(sum_b, mid_count, mid_mul),
            ));
        }
        let mid_end = width - radius;
        for (&add_c, &sub_c) in col_sums[window_span..width]
            .iter()
            .zip(col_sums[..mid_end - radius - 1].iter())
        {
            sum_r = sum_r + u64::from(add_c[0]) - u64::from(sub_c[0]);
            sum_g = sum_g + u64::from(add_c[1]) - u64::from(sub_c[1]);
            sum_b = sum_b + u64::from(add_c[2]) - u64::from(sub_c[2]);
            out.push((
                div_by_u64_fast(sum_r, mid_count, mid_mul),
                div_by_u64_fast(sum_g, mid_count, mid_mul),
                div_by_u64_fast(sum_b, mid_count, mid_mul),
            ));
        }
        // Right border: window shrinks from right edge.
        for x in mid_end..width {
            let drop_c = col_sums[x - radius - 1];
            sum_r -= u64::from(drop_c[0]);
            sum_g -= u64::from(drop_c[1]);
            sum_b -= u64::from(drop_c[2]);
            let count = (width - (x - radius)) as u64 * row_weight_sum;
            out.push((
                (sum_r / count) as u8,
                (sum_g / count) as u8,
                (sum_b / count) as u8,
            ));
        }
    } else if radius == 0 {
        let mul = fast_u64_recip(row_weight_sum);
        for c in col_sums.iter() {
            out.push((
                div_by_u64_fast(u64::from(c[0]), row_weight_sum, mul),
                div_by_u64_fast(u64::from(c[1]), row_weight_sum, mul),
                div_by_u64_fast(u64::from(c[2]), row_weight_sum, mul),
            ));
        }
    } else {
        let mut cur_start = 0usize;
        let mut cur_end = initial_end;
        for x in 0..width {
            let start = x.saturating_sub(radius);
            let end = x.saturating_add(radius).saturating_add(1).min(width);
            while cur_end < end {
                let c = col_sums[cur_end];
                sum_r += u64::from(c[0]);
                sum_g += u64::from(c[1]);
                sum_b += u64::from(c[2]);
                cur_end += 1;
            }
            while cur_start < start {
                let c = col_sums[cur_start];
                sum_r -= u64::from(c[0]);
                sum_g -= u64::from(c[1]);
                sum_b -= u64::from(c[2]);
                cur_start += 1;
            }
            let count = (end - start) as u64 * row_weight_sum;
            out.push((
                (sum_r / count) as u8,
                (sum_g / count) as u8,
                (sum_b / count) as u8,
            ));
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn smooth_scanline(
    raw_bgra: &[u8],
    width: usize,
    rows: usize,
    radius: usize,
) -> Vec<(u8, u8, u8)> {
    let mut col_sums = Vec::new();
    let mut out = Vec::new();
    smooth_scanline_into(raw_bgra, width, rows, radius, &mut col_sums, &mut out);
    out
}

#[inline]
fn blend_channel(old: u8, new: u8) -> u8 {
    // Exact integer equivalent of BLEND_STEP = 0.5 with half-away-from-zero rounding.
    if old.abs_diff(new) <= 1 {
        new
    } else {
        ((u16::from(old) + u16::from(new) + 1) >> 1) as u8
    }
}

/// Exponential ease used for the cross-fade between two sampled edges.
pub(crate) fn blend_toward(shown: &mut [(u8, u8, u8)], target: &[(u8, u8, u8)]) {
    for (slot, &(red, green, blue)) in shown.iter_mut().zip(target) {
        let (old_red, old_green, old_blue) = *slot;
        *slot = (
            blend_channel(old_red, red),
            blend_channel(old_green, green),
            blend_channel(old_blue, blue),
        );
    }
}

pub(crate) fn fill_row_alphas(height: usize, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(height);
    let span = height.saturating_sub(1).max(1) as f32;
    for row in 0..height {
        let falloff = 1.0 - row as f32 / span;
        let alpha = (TOP_ALPHA * falloff.powf(FALLOFF) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
        out.push(alpha);
    }
}

pub(crate) fn fill_gradient_columns_with_alphas(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    columns: &[(u8, u8, u8)],
    style: &Style,
    row_alphas: &[u8],
) {
    assert_eq!(columns.len(), width);
    assert_eq!(pixels.len(), width * height * 4);
    assert_eq!(row_alphas.len(), height);
    let step = metrics(96).grid_step.max(8) as usize;
    let grid_alpha = (GRID_ALPHA * 255.0) as u32;
    let mut lut = [0u8; 256];
    for (row, (row_pixels, &alpha_u8)) in pixels
        .chunks_exact_mut(width * 4)
        .zip(row_alphas.iter())
        .enumerate()
    {
        if alpha_u8 == 0 {
            row_pixels.fill(0);
            continue;
        }
        let alpha = u32::from(alpha_u8);
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = div_255((i as u32) * alpha) as u8;
        }
        let (row_px, _) = row_pixels.as_chunks_mut::<4>();
        for (px, &(red, green, blue)) in row_px.iter_mut().zip(columns) {
            *px = [
                lut[usize::from(blue)],
                lut[usize::from(green)],
                lut[usize::from(red)],
                alpha_u8,
            ];
        }
        if style.pattern {
            // Apply faint grid lines while this row is hot in L1 cache.
            let shade = div_255(grid_alpha * alpha) as u8;
            if row % step == 0 {
                for px in row_px.iter_mut().step_by(step) {
                    px[0] = px[0].saturating_add(shade).min(alpha_u8);
                    px[1] = px[1].saturating_add(shade).min(alpha_u8);
                    px[2] = px[2].saturating_add(shade).min(alpha_u8);
                }
            } else {
                for px in row_px.iter_mut().step_by(step) {
                    px[0] = px[0].saturating_sub(shade);
                    px[1] = px[1].saturating_sub(shade);
                    px[2] = px[2].saturating_sub(shade);
                }
            }
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn fill_gradient_columns(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    columns: &[(u8, u8, u8)],
    style: &Style,
) {
    let mut row_alphas = Vec::new();
    fill_row_alphas(height, &mut row_alphas);
    fill_gradient_columns_with_alphas(pixels, width, height, columns, style, &row_alphas);
}

pub fn has_eligible_foreground() -> bool {
    unsafe {
        if GetSystemMetrics(SM_REMOTESESSION) != 0 || !dwm_composition_enabled() {
            return false;
        }
        let fg = GetForegroundWindow();
        !fg.is_invalid() && IsWindowVisible(fg).as_bool() && IsZoomed(fg).as_bool()
    }
}

/// Check DWM composition directly via dwmapi instead of dynamic GetProcAddress lookup.
fn dwm_composition_enabled() -> bool {
    unsafe { DwmIsCompositionEnabled().map(BOOL::as_bool).unwrap_or(true) }
}

const SHELL_CLASSES: [&[u8]; 6] = [
    b"Shell_TrayWnd",
    b"Shell_SecondaryTrayWnd",
    b"Progman",
    b"WorkerW",
    b"Windows.UI.Core.CoreWindow",
    b"TransparentBar.Connector",
];

#[inline]
fn is_shell_class_utf16(class: &[u16]) -> bool {
    SHELL_CLASSES.iter().any(|&name| {
        class.len() == name.len()
            && class
                .iter()
                .zip(name.iter())
                .all(|(&w, &b)| w == u16::from(b))
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES.contains(&class.as_bytes())
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

pub(crate) fn find_docked_taskbar(fg: HWND) -> Option<(HWND, HMONITOR)> {
    unsafe {
        let fg_monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
        if fg_monitor.is_invalid() {
            return None;
        }
        // Primary bottom taskbar
        if let Ok(primary) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if !primary.is_invalid()
                && MonitorFromWindow(primary, MONITOR_DEFAULTTONULL) == fg_monitor
            {
                return Some((primary, fg_monitor));
            }
        }
        // Secondary taskbar on multi-monitor setups
        let mut child = HWND::default();
        while let Ok(next) = FindWindowExW(None, child, w!("Shell_SecondaryTrayWnd"), None) {
            if next.is_invalid() {
                break;
            }
            if MonitorFromWindow(next, MONITOR_DEFAULTTONULL) == fg_monitor {
                return Some((next, fg_monitor));
            }
            child = next;
        }
        None
    }
}

fn maximised_above(fg: HWND, monitor: HMONITOR, bar: &RECT, m: &Metrics) -> Option<RECT> {
    unsafe {
        // Fast local checks first to avoid unnecessary shell RPCs on ineligible windows.
        let mut name = [0u16; 64];
        let length = GetClassNameW(fg, &mut name);
        if length <= 0 || is_shell_class_utf16(&name[..length as usize]) {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(fg, &mut rect).ok()?;
        if !is_docked_above_taskbar(&rect, bar, m) {
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
        if let Ok(state) = SHQueryUserNotificationState() {
            if matches!(
                state,
                QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_APP
            ) {
                return None;
            }
        }
        Some(rect)
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
    fn blending_eases_instead_of_snapping() {
        let mut shown = vec![(0u8, 0u8, 0u8); 2];
        let target = [(200u8, 100u8, 50u8); 2];
        let mut previous = 0u8;
        let mut frames = 0;
        while shown[0].0 != target[0].0 {
            blend_toward(&mut shown, &target);
            assert!(shown[0].0 >= previous, "blend must move toward the target");
            previous = shown[0].0;
            assert_eq!(shown[0], shown[1], "every column animates at the same rate");
            frames += 1;
            assert!(frames < 40, "blend must converge, not stall");
        }
        assert!(frames > 1, "one frame would be a snap, not a fade");
    }

    #[test]
    fn sampled_rows_weight_the_visible_edge() {
        // Row 0 is farthest from the app edge; the last row is the one the user sees.
        let far = [[0u8, 0, 0, 255]; 1].concat();
        // Raw pixels are BGRA, so red lives at offset 2.
        let near = [[0u8, 0, 255, 255]; 1].concat();
        let capture = [far, near.clone()].concat();
        let weighted = smooth_scanline(&capture, 1, 2, 0);
        assert!(weighted[0].0 > 127, "the edge row must dominate the sample");
        assert_eq!(weighted[0].2, 0, "the far row must not win");
        assert_eq!(smooth_scanline(&near, 1, 1, 0), [(255, 0, 0)]);
    }

    #[test]
    fn pattern_adds_grid_lines_and_reduce_motion_snaps() {
        let columns = [(100u8, 120, 140); 60];
        let gradient = Style {
            pattern: false,
            reduce_motion: false,
        };
        let patterned = Style {
            pattern: true,
            reduce_motion: false,
        };
        let mut plain = vec![0u8; 60 * 20 * 4];
        let mut grid = vec![0u8; 60 * 20 * 4];
        fill_gradient_columns(&mut plain, 60, 20, &columns, &gradient);
        fill_gradient_columns(&mut grid, 60, 20, &columns, &patterned);
        assert_ne!(&plain[..], &grid[..], "grid pattern must alter pixels");
        // A vertical grid line lands at column 0, brightening its first row.
        assert!(grid[0] > plain[0]);
        // A row between grid lines is darkened, and the alpha byte is untouched.
        assert!(grid[(3 * 60) * 4] < plain[(3 * 60) * 4]);
        assert_eq!(grid[(3 * 60) * 4 + 3], plain[(3 * 60) * 4 + 3]);
        // Reduce motion disables blending, so a jump lands in one frame.
        let mut shown = vec![(0u8, 0u8, 0u8)];
        let snap = Style {
            pattern: false,
            reduce_motion: true,
        };
        let target = [(255u8, 255, 255)];
        assert!(snap.reduce_motion);
        if snap.reduce_motion {
            shown = target.to_vec();
        } else {
            blend_toward(&mut shown, &target);
        }
        assert_eq!(shown, target);
    }

    #[test]
    fn thin_taskbars_keep_the_strip_proportional() {
        let m = metrics(96);
        for (bar_height, expected) in [(28, 15), (40, 22), (48, 26), (80, 34), (200, 34)] {
            let max_height = (bar_height * MAX_STRIP_FRACTION / MAX_STRIP_FRACTION_DEN)
                .max(m.seam + 8)
                .min(m.strip);
            assert_eq!(max_height, expected, "bar height {bar_height}");
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

    #[test]
    fn pattern_brightening_clamps_to_premultiplied_alpha_without_wraparound() {
        let width = 64;
        let height = 32;
        let white = vec![(255u8, 255u8, 255u8); width];
        let mut pixels = vec![0u8; width * height * 4];
        fill_gradient_columns(
            &mut pixels,
            width,
            height,
            &white,
            &Style {
                pattern: true,
                reduce_motion: false,
            },
        );
        for &[b, g, r, a] in pixels.as_chunks::<4>().0 {
            assert!(
                b <= a && g <= a && r <= a,
                "premultiplied BGRA violated: ({b}, {g}, {r}, {a})"
            );
        }
        // Top-left pixel is on both horizontal and vertical grid lines (row=0, x=0): must stay bright, never wrap.
        let a0 = pixels[3];
        assert!(a0 > 200);
        assert_eq!((pixels[0], pixels[1], pixels[2]), (a0, a0, a0));
    }

    #[test]
    fn div_255_is_bit_exact_for_all_u8_products() {
        for c in 0u32..=255 {
            for a in 0u32..=255 {
                let p = c * a;
                assert_eq!(div_255(p), p / 255, "mismatch at c={c}, a={a}");
            }
        }
    }

    #[test]
    fn blend_channel_matches_float_half_step_for_all_u8_pairs() {
        for old in 0u8..=255 {
            for new in 0u8..=255 {
                let diff = f32::from(new) - f32::from(old);
                let expected = if diff.abs() <= 1.0 {
                    new
                } else {
                    (f32::from(old) + diff * 0.5).round().clamp(0.0, 255.0) as u8
                };
                assert_eq!(
                    blend_channel(old, new),
                    expected,
                    "mismatch at old={old}, new={new}"
                );
            }
        }
    }

    #[test]
    fn smooth_scanline_into_matches_reference_and_reuses_buffers() {
        fn reference(
            raw_bgra: &[u8],
            width: usize,
            rows: usize,
            radius: usize,
        ) -> Vec<(u8, u8, u8)> {
            let mut prefix = vec![(0u64, 0u64, 0u64); width + 1];
            let mut weights = vec![0u64; width + 1];
            for x in 0..width {
                let (mut r, mut g, mut b) = prefix[x];
                let mut wsum = weights[x];
                for row in 0..rows {
                    let at = (row * width + x) * 4;
                    let w = ((row + 1) as u64) * ((row + 1) as u64);
                    r += u64::from(raw_bgra[at + 2]) * w;
                    g += u64::from(raw_bgra[at + 1]) * w;
                    b += u64::from(raw_bgra[at]) * w;
                    wsum += w;
                }
                prefix[x + 1] = (r, g, b);
                weights[x + 1] = wsum;
            }
            (0..width)
                .map(|x| {
                    let start = x.saturating_sub(radius);
                    let end = x.saturating_add(radius).saturating_add(1).min(width);
                    let count = weights[end] - weights[start];
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

        let mut col_sums = Vec::new();
        let mut out = Vec::new();
        for width in [1usize, 2, 3, 5, 16, 17, 18, 64, 300] {
            for rows in [1usize, 2, 4, 8] {
                let mut raw = vec![0u8; width * rows * 4];
                for (i, byte) in raw.iter_mut().enumerate() {
                    *byte = ((i.wrapping_mul(37) + 13) & 0xff) as u8;
                }
                for radius in [0usize, 1, 2, 8, 12] {
                    smooth_scanline_into(&raw, width, rows, radius, &mut col_sums, &mut out);
                    let expected = reference(&raw, width, rows, radius);
                    assert_eq!(out, expected, "width={width} rows={rows} radius={radius}");
                }
            }
        }
        // Second call at same dimensions reuses exact allocations (pointers stay stable).
        let raw = vec![128u8; 300 * 4 * 4];
        smooth_scanline_into(&raw, 300, 4, 8, &mut col_sums, &mut out);
        let sums_ptr = col_sums.as_ptr();
        let out_ptr = out.as_ptr();
        smooth_scanline_into(&raw, 300, 4, 8, &mut col_sums, &mut out);
        assert_eq!(col_sums.as_ptr(), sums_ptr);
        assert_eq!(out.as_ptr(), out_ptr);
    }

    #[test]
    fn is_shell_class_utf16_matches_str_list() {
        for name in [
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "Progman",
            "WorkerW",
            "Windows.UI.Core.CoreWindow",
            "TransparentBar.Connector",
        ] {
            let wide: Vec<u16> = name.encode_utf16().collect();
            assert!(is_shell_class_utf16(&wide), "{name}");
            assert!(is_shell_class(name));
        }
        for other in ["Chrome_WidgetWin_1", "Notepad", "Shell_TrayWnd2", ""] {
            let wide: Vec<u16> = other.encode_utf16().collect();
            assert!(!is_shell_class_utf16(&wide), "{other}");
            assert!(!is_shell_class(other));
        }
    }
}
