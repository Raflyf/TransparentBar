// ponytail: GitHub release tag check via WinHTTP; add signed self-updater when code-signing certificate is provisioned.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::*,
        Networking::WinHttp::*,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: PCWSTR = w!("https://github.com/Raflyf/TransparentBar/releases/latest");
static CHECKING: AtomicBool = AtomicBool::new(false);
static AVAILABLE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

struct WinHttpHandle(*mut core::ffi::c_void);
impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

pub fn available_tag() -> Option<String> {
    AVAILABLE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

fn set_available_tag(tag: String) {
    if let Ok(mut guard) = AVAILABLE.get_or_init(|| Mutex::new(None)).lock() {
        *guard = Some(tag);
    }
}

pub fn open_release_page(hwnd: HWND) {
    unsafe {
        let _ = ShellExecuteW(hwnd, w!("open"), RELEASES_URL, None, None, SW_SHOWNORMAL);
    }
}

pub fn check_in_background(hwnd: HWND, manual: bool, notify_message: u32) {
    if CHECKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let hwnd_raw = hwnd.0 as usize;
    std::thread::spawn(move || {
        let outcome = fetch_latest_tag();
        CHECKING.store(false, Ordering::Release);
        let target = HWND(hwnd_raw as *mut core::ffi::c_void);
        match outcome {
            Ok(tag) if is_newer(&tag, CURRENT_VERSION) => {
                set_available_tag(tag);
                unsafe {
                    let _ = PostMessageW(
                        target,
                        notify_message,
                        WPARAM(usize::from(manual)),
                        LPARAM(1),
                    );
                }
            }
            Ok(tag) if manual => {
                if is_newer(&tag, CURRENT_VERSION) {
                    set_available_tag(tag);
                }
                unsafe {
                    let _ = PostMessageW(target, notify_message, WPARAM(1), LPARAM(0));
                }
            }
            Err(_) if manual => unsafe {
                let _ = PostMessageW(target, notify_message, WPARAM(1), LPARAM(-1));
            },
            _ => (),
        }
    });
}

pub fn show_balloon(hwnd: HWND, title: &str, body: &str) {
    unsafe {
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_INFO,
            ..Default::default()
        };
        copy_wide(&mut icon.szInfoTitle, title);
        copy_wide(&mut icon.szInfo, body);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &icon);
    }
}

fn copy_wide(dest: &mut [u16], text: &str) {
    let limit = dest.len().saturating_sub(1);
    for (slot, ch) in dest.iter_mut().take(limit).zip(text.encode_utf16()) {
        *slot = ch;
    }
}

fn fetch_latest_tag() -> Result<String> {
    fetch_tag_from_redirect().or_else(|_| fetch_tag_from_api())
}

fn fetch_tag_from_redirect() -> Result<String> {
    unsafe {
        let session = WinHttpHandle(WinHttpOpen(
            w!("TransparentBar"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(Error::from_win32());
        }
        WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 5000)?;
        let connect = WinHttpHandle(WinHttpConnect(
            session.0,
            w!("github.com"),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connect.0.is_null() {
            return Err(Error::from_win32());
        }
        let request = WinHttpHandle(WinHttpOpenRequest(
            connect.0,
            w!("HEAD"),
            w!("/Raflyf/TransparentBar/releases/latest"),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(Error::from_win32());
        }
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&policy.to_ne_bytes()),
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
        let mut buffer = [0u16; 512];
        let mut bytes = std::mem::size_of_val(&buffer) as u32;
        let mut index = 0u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr().cast()),
            &mut bytes,
            &mut index,
        )?;
        let nul = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let location = String::from_utf16_lossy(&buffer[..nul]);
        extract_tag_from_location(&location)
            .ok_or_else(|| Error::new(E_FAIL, "Release tag was not found in redirect location."))
    }
}

fn fetch_tag_from_api() -> Result<String> {
    unsafe {
        let session = WinHttpHandle(WinHttpOpen(
            w!("TransparentBar"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(Error::from_win32());
        }
        WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 5000)?;
        let connect = WinHttpHandle(WinHttpConnect(
            session.0,
            w!("api.github.com"),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connect.0.is_null() {
            return Err(Error::from_win32());
        }
        let request = WinHttpHandle(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            w!("/repos/Raflyf/TransparentBar/releases/latest"),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(Error::from_win32());
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
        let mut body = Vec::new();
        let mut chunk = [0u8; 2048];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )?;
            if read == 0 || body.len() > 65536 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
        let text = String::from_utf8_lossy(&body);
        extract_tag_from_json(&text)
            .ok_or_else(|| Error::new(E_FAIL, "Release tag was not found in API response."))
    }
}

pub(crate) fn extract_tag_from_location(location: &str) -> Option<String> {
    let (_, tail) = location.rsplit_once("/releases/tag/")?;
    let tag = tail
        .trim_end_matches('\0')
        .split(['?', '#', '/'])
        .next()?
        .trim();
    if parse_semver(tag).is_some() {
        Some(tag.to_string())
    } else {
        None
    }
}

pub(crate) fn extract_tag_from_json(json: &str) -> Option<String> {
    let (_, rest) = json.split_once("\"tag_name\"")?;
    let (_, after_colon) = rest.split_once(':')?;
    let trimmed = after_colon.trim_start();
    let inner = trimmed.strip_prefix('"')?;
    let (tag, _) = inner.split_once('"')?;
    if parse_semver(tag).is_some() {
        Some(tag.to_string())
    } else {
        None
    }
}

pub(crate) fn parse_semver(version: &str) -> Option<(u32, u32, u32)> {
    let clean = version
        .trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .trim_start_matches(['v', 'V']);
    let core = clean.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub(crate) fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_semver(candidate), parse_semver(current)) {
        (Some(cand), Some(curr)) => cand > curr,
        _ => false,
    }
}
