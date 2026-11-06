#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
};
use windows::{
    core::{s, w, Error, Result, PCWSTR},
    Win32::{
        Foundation::*,
        System::{Com::*, LibraryLoader::*, Registry::*, Threading::*},
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
};

const NAME: PCWSTR = w!("TransparentBar");
const WINDOW_CLASS: PCWSTR = w!("TransparentBar.Window");
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const TRAY_MESSAGE: u32 = WM_APP + 1;
const RECONNECT_MESSAGE: u32 = WM_APP + 2;
const REFRESH_MESSAGE: u32 = WM_APP + 3;
const COMMAND_MESSAGE: u32 = WM_APP + 5;
const MENU_MESSAGE: u32 = WM_APP + 4;
const STARTUP: usize = 1;
const ABOUT: usize = 2;
const EXIT: usize = 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const REFRESH_TIMER_ID: usize = 1;
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

mod icon;
mod protocol;
use protocol::{event_name, InitializeXaml, StatusChannel, BRIDGE_CLSID, BRIDGE_FILE};
static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    color: u32,
    animation: i32,
}
#[repr(C)]
struct CompositionData {
    attribute: i32,
    data: *mut c_void,
    size: usize,
}
type Composition = unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;

struct XamlSession {
    ready: HANDLE,
    failed: HANDLE,
    stop: HANDLE,
    done: HANDLE,
    status: StatusChannel,
}
impl XamlSession {
    fn connect(generation: u32) -> Result<Self> {
        unsafe {
            let hwnd = FindWindowW(w!("Shell_TrayWnd"), None)?;
            let mut shell = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut shell));
            if shell == 0 {
                return Err(Error::from(E_FAIL));
            }
            let host = GetCurrentProcessId();
            let create = |kind: &str| -> Result<HANDLE> {
                let name = event_name(host, shell, generation, kind);
                CreateEventW(None, true, false, PCWSTR(name.as_ptr()))
            };
            let mut session = Self {
                ready: HANDLE::default(),
                failed: HANDLE::default(),
                stop: HANDLE::default(),
                done: HANDLE::default(),
                status: StatusChannel::create(
                    host,
                    shell,
                    generation,
                    std::env::args().any(|arg| arg == "--diagnose"),
                )?,
            };
            session.ready = create("Ready")?;
            session.failed = create("Failed")?;
            session.stop = create("Stop")?;
            session.done = create("Done")?;
            let module = LoadLibraryExW(
                w!("Windows.UI.Xaml.dll"),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )?;
            let result = (|| -> Result<()> {
                let address = GetProcAddress(module, s!("InitializeXamlDiagnosticsEx"))
                    .ok_or_else(Error::from_win32)?;
                let initialize: InitializeXaml = std::mem::transmute(address);
                let bridge = std::env::current_exe()?.with_file_name(BRIDGE_FILE);
                if !bridge.is_file() {
                    return Err(Error::new(
                        E_FAIL,
                        format!("{BRIDGE_FILE} harus di folder aplikasi."),
                    ));
                }
                let bridge = wide(bridge.as_os_str());
                let init = wide(std::ffi::OsStr::new(&format!(
                    "{host}:{shell}:{generation}{}",
                    if std::env::args().any(|arg| arg == "--diagnose") {
                        ";inspect"
                    } else {
                        ""
                    }
                )));
                let endpoint = wide(std::ffi::OsStr::new(&format!(
                    "VisualDiagConnection{generation}"
                )));
                initialize(
                    PCWSTR(endpoint.as_ptr()),
                    shell,
                    PCWSTR::null(),
                    PCWSTR(bridge.as_ptr()),
                    BRIDGE_CLSID,
                    PCWSTR(init.as_ptr()),
                )
                .ok()
                .inspect(|_| protocol::trace("Host.Initialize S_OK"))
            })();
            let _ = FreeLibrary(module);
            result?;
            Ok(session)
        }
    }
    fn ready(&self) -> Result<bool> {
        unsafe {
            let code = self.status.data().error.load(Ordering::Acquire);
            if code < 0 || WaitForSingleObject(self.failed, 0) == WAIT_OBJECT_0 {
                let code = if code < 0 {
                    windows::core::HRESULT(code)
                } else {
                    E_FAIL
                };
                return Err(Error::new(
                    code,
                    format!("Bridge gagal: {}", self.status.describe()),
                ));
            }
            Ok(WaitForSingleObject(self.ready, 0) == WAIT_OBJECT_0)
        }
    }
    fn restore(&self) -> Result<()> {
        unsafe {
            SetEvent(self.stop)?;
            if WaitForSingleObject(self.done, 3000) != WAIT_OBJECT_0 {
                return Err(Error::new(
                    E_FAIL,
                    "Pemulihan taskbar belum terkonfirmasi. Explorer mungkin sedang berhenti.",
                ));
            }
        }
        Ok(())
    }
}
impl Drop for XamlSession {
    fn drop(&mut self) {
        unsafe {
            if !self.stop.is_invalid() {
                let _ = SetEvent(self.stop);
            }
            for handle in [self.ready, self.failed, self.stop, self.done] {
                if !handle.is_invalid() {
                    let _ = CloseHandle(handle);
                }
            }
        }
    }
}

struct TaskbarService {
    xaml: Option<XamlSession>,
    saved: Vec<(HWND, AccentPolicy)>,
}
impl TaskbarService {
    fn connect(generation: u32) -> Result<Self> {
        let hwnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), None)? };
        let xaml = unsafe {
            FindWindowExW(
                hwnd,
                None,
                w!("Windows.UI.Composition.DesktopWindowContentBridge"),
                None,
            )
            .is_ok()
        };
        Ok(Self {
            xaml: if xaml {
                Some(XamlSession::connect(generation)?)
            } else {
                None
            },
            saved: Vec::new(),
        })
    }
    fn apply(&mut self) -> Result<bool> {
        if let Some(session) = &self.xaml {
            return session.ready();
        }
        unsafe {
            let user32 = GetModuleHandleW(w!("user32.dll"))?;
            let address = GetProcAddress(user32, s!("SetWindowCompositionAttribute"))
                .ok_or_else(Error::from_win32)?;
            let set: Composition = std::mem::transmute(address);
            let get = GetProcAddress(user32, s!("GetWindowCompositionAttribute")).map(|address| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, Composition>(address)
            });
            for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
                let mut previous = HWND::default();
                while let Ok(hwnd) = FindWindowExW(None, previous, class, None) {
                    if !self.saved.iter().any(|(saved, _)| *saved == hwnd) {
                        let mut original = AccentPolicy::default();
                        if let Some(get) = get {
                            let mut data = CompositionData {
                                attribute: 19,
                                data: (&mut original as *mut AccentPolicy).cast(),
                                size: std::mem::size_of::<AccentPolicy>(),
                            };
                            let _ = get(hwnd, &mut data);
                        }
                        self.saved.push((hwnd, original));
                    }
                    let mut clear = AccentPolicy {
                        state: 2,
                        flags: 0,
                        color: 0,
                        animation: 0,
                    };
                    let mut data = CompositionData {
                        attribute: 19,
                        data: (&mut clear as *mut AccentPolicy).cast(),
                        size: std::mem::size_of::<AccentPolicy>(),
                    };
                    if !set(hwnd, &mut data).as_bool() {
                        return Err(Error::new(
                            E_FAIL,
                            "Windows tidak menerima transparansi taskbar legacy.",
                        ));
                    }
                    previous = hwnd;
                }
            }
        }
        Ok(true)
    }
    fn restore(&self) -> Result<()> {
        if let Some(session) = &self.xaml {
            return session.restore();
        }
        unsafe {
            let user32 = GetModuleHandleW(w!("user32.dll"))?;
            let address = GetProcAddress(user32, s!("SetWindowCompositionAttribute"))
                .ok_or_else(Error::from_win32)?;
            let set: Composition = std::mem::transmute(address);
            for (hwnd, original) in &self.saved {
                if !IsWindow(*hwnd).as_bool() {
                    continue;
                }
                let mut original = *original;
                let mut data = CompositionData {
                    attribute: 19,
                    data: (&mut original as *mut AccentPolicy).cast(),
                    size: std::mem::size_of::<AccentPolicy>(),
                };
                if !set(*hwnd, &mut data).as_bool() {
                    return Err(Error::new(
                        E_FAIL,
                        "Windows menolak pemulihan taskbar legacy.",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn startup_command(path: &Path) -> Vec<u16> {
    std::iter::once(b'"' as u16)
        .chain(path.as_os_str().encode_wide())
        .chain([b'"' as u16, 0])
        .collect()
}

fn startup_enabled() -> Result<bool> {
    let mut data = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&data) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            NAME,
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    status.ok()?;
    Ok(bytes > 2 && data[0] != 0)
}

fn set_startup(enabled: bool) -> Result<()> {
    unsafe {
        let command = if enabled {
            startup_command(&std::env::current_exe()?)
        } else {
            Vec::new()
        };
        if command.len() > 261 {
            return Err(Error::new(
                E_INVALIDARG,
                "Path terlalu panjang untuk startup Windows. Pindahkan folder aplikasi.",
            ));
        }
        let mut key = HKEY::default();
        if enabled {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
            .ok()?;
        } else {
            let status = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_SET_VALUE, &mut key);
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            status.ok()?;
        }
        let result = if enabled {
            let bytes =
                std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2);
            RegSetValueExW(key, NAME, 0, REG_SZ, Some(bytes))
        } else {
            RegDeleteValueW(key, NAME)
        };
        let _ = RegCloseKey(key);
        if !enabled && result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        result.ok()
    }
}

fn show_error(error: &Error) {
    eprintln!("{error}");
    let text = wide(std::ffi::OsStr::new(&format!(
        "{error}

Kirim pesan error ini untuk diagnosis. Detail startup: %LOCALAPPDATA%\\TransparentBar\\startup.log"
    )));
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), NAME, MB_OK | MB_ICONERROR);
    }
}

fn add_tray(hwnd: HWND, application_icon: HICON) -> Result<()> {
    unsafe {
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: application_icon,
            ..Default::default()
        };
        let tip = wide(std::ffi::OsStr::new("TransparentBar - taskbar transparan"));
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        if !Shell_NotifyIconW(NIM_ADD, &icon).as_bool() {
            return Err(Error::new(E_FAIL, "Explorer belum menerima ikon tray. Tunggu sampai taskbar siap, lalu buka ulang aplikasi."));
        }
        icon.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // Unsupported v4 keeps legacy notifications; GetLastError is not part of this API contract.
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &icon);
    }
    Ok(())
}

fn menu(hwnd: HWND, point: POINT) -> Result<usize> {
    let enabled = startup_enabled()?;
    unsafe {
        let popup = CreatePopupMenu()?;
        let result = (|| {
            AppendMenuW(
                popup,
                MF_STRING | if enabled { MF_CHECKED } else { MF_UNCHECKED },
                STARTUP,
                w!("Jalankan saat &login"),
            )?;
            AppendMenuW(popup, MF_SEPARATOR, 0, None)?;
            AppendMenuW(popup, MF_STRING, ABOUT, w!("&Tentang / lisensi"))?;
            AppendMenuW(popup, MF_STRING, EXIT, w!("&Keluar"))?;
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                popup,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                None,
            )
            .0;
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            Ok(selected as usize)
        })();
        DestroyMenu(popup)?;
        result
    }
}

fn is_tray_menu(notification: u32) -> bool {
    matches!(
        notification,
        WM_CONTEXTMENU | WM_LBUTTONUP | WM_RBUTTONUP | NIN_SELECT | NIN_KEYSELECT
    )
}

fn is_refresh_timer(hwnd: HWND, message: &MSG) -> bool {
    message.message == WM_TIMER
        && message.hwnd == hwnd
        && message.wParam.0 == REFRESH_TIMER_ID
        && message.lParam.0 == 0
}

fn schedule(hwnd: HWND, delay_ms: u32) -> Result<()> {
    if unsafe { SetTimer(hwnd, REFRESH_TIMER_ID, delay_ms, None) } == 0 {
        Err(Error::from_win32())
    } else {
        Ok(())
    }
}

fn command(id: usize) -> Result<bool> {
    match id {
        STARTUP => set_startup(!startup_enabled()?)?,
        ABOUT => unsafe {
            MessageBoxW(
                None,
                w!("TransparentBar 0.1.4
Native Rust, Windows 10 dan 11 x64.
Copyright 2026 Rafly Firmansyah.
MIT License; boleh dibagikan sesuai lisensi. Tanpa garansi.
Source: github.com/Raflyf/transparentbar

Pengaturan startup berlaku untuk akun Windows ini saja."),
                NAME,
                MB_OK,
            );
        },
        EXIT => return Ok(true),
        _ => (),
    }
    Ok(false)
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == TASKBAR_CREATED.load(Ordering::Relaxed) && message != 0 {
        RESTART.store(true, Ordering::Release);
        SetTimer(hwnd, REFRESH_TIMER_ID, 1000, None);
        return LRESULT(0);
    }
    match message {
        TRAY_MESSAGE if is_tray_menu((lp.0 as u32) & 0xffff) => {
            let _ = PostMessageW(hwnd, MENU_MESSAGE, wp, lp);
            LRESULT(0)
        }
        WM_COMMAND => {
            let _ = PostMessageW(hwnd, COMMAND_MESSAGE, wp, lp);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_THEMECHANGED => {
            SetTimer(hwnd, REFRESH_TIMER_ID, 500, None);
            LRESULT(0)
        }
        WM_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION if wp.0 != 0 => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wp, lp),
    }
}

unsafe extern "system" fn foreground_changed(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if let Ok(hwnd) = FindWindowW(WINDOW_CLASS, None) {
        SetTimer(hwnd, REFRESH_TIMER_ID, 100, None);
    }
}

fn run() -> Result<()> {
    unsafe {
        let mutex = CreateMutexW(None, false, w!(r"Local\TransparentBar.SingleInstance"))?;
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let result = (|| {
            if exists {
                if let Ok(hwnd) = FindWindowW(WINDOW_CLASS, None) {
                    PostMessageW(hwnd, MENU_MESSAGE, WPARAM(0), LPARAM(0))?;
                }
                return Ok(());
            }
            let application_icon = icon::AppIcon::load()?;
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hIcon: application_icon.0,
                lpszClassName: WINDOW_CLASS,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(Error::from_win32());
            }
            TASKBAR_CREATED.store(
                RegisterWindowMessageW(w!("TaskbarCreated")),
                Ordering::Relaxed,
            );
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                WINDOW_CLASS,
                NAME,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                instance,
                None,
            )?;
            let foreground_hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let mut service: Option<TaskbarService> = None;
            let result = (|| {
                let mut tray_ready = false;
                let mut retries = 3u8;
                let mut generation = 1u32;
                let mut readiness = 20u8;
                schedule(hwnd, 500)?;
                loop {
                    let mut msg = MSG::default();
                    let status = GetMessageW(&mut msg, None, 0, 0).0;
                    if status == -1 {
                        return Err(Error::from_win32());
                    }
                    if status == 0 {
                        break;
                    }
                    match msg.message {
                        RECONNECT_MESSAGE => {
                            service = None;
                            generation += 1;
                            readiness = 20;
                            tray_ready = false;
                            retries = 3;
                            schedule(hwnd, 1000)?;
                        }
                        REFRESH_MESSAGE => {
                            schedule(hwnd, 500)?;
                        }
                        WM_TIMER if is_refresh_timer(hwnd, &msg) => {
                            // KillTimer leaves queued ticks behind; an already-removed timer is harmless.
                            let _ = KillTimer(hwnd, REFRESH_TIMER_ID);
                            if RESTART.swap(false, Ordering::AcqRel) {
                                service = None;
                                generation += 1;
                                readiness = 20;
                                tray_ready = false;
                                retries = 3;
                            }
                            if !tray_ready {
                                match add_tray(hwnd, application_icon.0) {
                                    Ok(()) => tray_ready = true,
                                    Err(_) if retries > 0 => {
                                        retries -= 1;
                                        schedule(hwnd, 2000)?;
                                        continue;
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            if service.is_none() {
                                match TaskbarService::connect(generation) {
                                    Ok(connected) => {
                                        service = Some(connected);
                                        schedule(hwnd, 750)?;
                                    }
                                    Err(error) if retries > 0 && error.code() != E_NOTIMPL => {
                                        retries -= 1;
                                        schedule(hwnd, 2000)?;
                                        continue;
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            if let Some(service) = &mut service {
                                if !service.apply()? {
                                    if readiness == 0 {
                                        let detail = service
                                            .xaml
                                            .as_ref()
                                            .map(|session| session.status.describe())
                                            .unwrap_or_default();
                                        return Err(Error::new(E_FAIL, format!("Taskbar belum siap: {detail}. Log: %LOCALAPPDATA%\\TransparentBar\\startup.log")));
                                    }
                                    readiness -= 1;
                                    schedule(hwnd, 500)?;
                                }
                            }
                        }
                        TRAY_MESSAGE | MENU_MESSAGE => {
                            let notification = (msg.lParam.0 as u32) & 0xffff;
                            if msg.message == MENU_MESSAGE || is_tray_menu(notification) {
                                let mut point = POINT::default();
                                GetCursorPos(&mut point)?;
                                if let Err(error) =
                                    menu(hwnd, point).and_then(command).map(|quit| {
                                        if quit {
                                            PostQuitMessage(0);
                                        }
                                    })
                                {
                                    show_error(&error);
                                }
                            }
                        }
                        WM_COMMAND | COMMAND_MESSAGE => match command(msg.wParam.0 & 0xffff) {
                            Ok(true) => break,
                            Ok(false) => (),
                            Err(error) => show_error(&error),
                        },
                        _ => {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                Ok(())
            })();
            let restore = if let Some(service) = service.take() {
                service.restore()
            } else {
                Ok(())
            };
            let _ = Shell_NotifyIconW(
                NIM_DELETE,
                &NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: 1,
                    ..Default::default()
                },
            );
            if !foreground_hook.is_invalid() {
                let _ = UnhookWinEvent(foreground_hook);
            }
            let _ = DestroyWindow(hwnd);
            result.and(restore)
        })();
        let _ = CloseHandle(mutex);
        result
    }
}

fn diagnose() -> Result<()> {
    protocol::trace("Host.Diagnose start");
    let session = XamlSession::connect(2)?;
    let start = std::time::Instant::now();
    let mut ready = false;
    unsafe {
        while start.elapsed() < std::time::Duration::from_secs(8) {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if session.ready()? {
                protocol::trace("Host.Diagnose Ready (read-only)");
                ready = true;
                break;
            }
            let _ = MsgWaitForMultipleObjects(None, false, 100, QS_ALLINPUT);
        }
    }
    let detail = session.status.describe();
    protocol::trace(&format!("Host.Diagnose stop: {detail}"));
    let restore = session.restore();
    if !ready {
        return Err(Error::new(E_FAIL, format!("Diagnostic timeout: {detail}")));
    }
    restore
}

fn main() {
    protocol::trace("Host.Main");
    unsafe {
        if let Err(error) = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() {
            show_error(&error);
            std::process::exit(1);
        }
        let diagnostic = std::env::args().any(|arg| arg == "--diagnose");
        let result = if diagnostic { diagnose() } else { run() };
        CoUninitialize();
        if let Err(error) = result {
            protocol::trace(&format!("Host.Error {error}"));
            if !diagnostic {
                show_error(&error);
            }
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_timer_does_not_consume_menu_or_callback_timers() {
        let window = HWND(0x100usize as *mut c_void);
        let own = MSG {
            hwnd: window,
            message: WM_TIMER,
            wParam: WPARAM(REFRESH_TIMER_ID),
            ..Default::default()
        };
        assert!(is_refresh_timer(window, &own));
        for foreign in [
            MSG {
                hwnd: HWND::default(),
                ..own
            },
            MSG {
                hwnd: HWND(0x200usize as *mut c_void),
                ..own
            },
            MSG {
                wParam: WPARAM(REFRESH_TIMER_ID + 1),
                ..own
            },
            MSG {
                lParam: LPARAM(1),
                ..own
            },
            MSG {
                message: WM_COMMAND,
                ..own
            },
        ] {
            assert!(
                !is_refresh_timer(window, &foreign),
                "foreign timer was consumed"
            );
        }
    }

    #[test]
    fn startup_quotes_path_and_preserves_unicode() {
        let command = startup_command(Path::new(r"C:\Aplikasi Saya\café\transparentbar.exe"));
        assert_eq!(command.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&command[..command.len() - 1]).unwrap(),
            r#""C:\Aplikasi Saya\café\transparentbar.exe""#
        );
    }

    #[test]
    fn tray_menu_supports_legacy_and_keyboard_events() {
        for notification in [
            WM_CONTEXTMENU,
            WM_LBUTTONUP,
            WM_RBUTTONUP,
            NIN_SELECT,
            NIN_KEYSELECT,
        ] {
            assert!(is_tray_menu(notification));
        }
        assert!(!is_tray_menu(WM_MOUSEMOVE));
        assert!(!is_tray_menu(NIN_POPUPOPEN));
    }

    #[test]
    fn legacy_accent_layout_matches_x64() {
        assert_eq!(std::mem::size_of::<AccentPolicy>(), 16);
        assert_eq!(std::mem::offset_of!(CompositionData, data), 8);
        assert_eq!(std::mem::size_of::<CompositionData>(), 24);
    }
}
