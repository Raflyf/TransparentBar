#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
};
use windows::{
    core::{s, w, Error, Result, PCSTR, PCWSTR},
    Win32::{
        Foundation::*,
        System::{Com::*, LibraryLoader::*, Registry::*, Threading::*},
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
};

const NAME: PCWSTR = w!("TransparentBar");
const WINDOW_CLASS: PCWSTR = w!("TransparentBar.Window");
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const SETTINGS_KEY: PCWSTR = w!(r"Software\TransparentBar");
const CONNECTOR_VALUE: PCWSTR = w!("TaskbarConnector");
const TRAY_MESSAGE: u32 = WM_APP + 1;
const RECONNECT_MESSAGE: u32 = WM_APP + 2;
const REFRESH_MESSAGE: u32 = WM_APP + 3;
const COMMAND_MESSAGE: u32 = WM_APP + 5;
const MENU_MESSAGE: u32 = WM_APP + 4;
const UPDATE_MESSAGE: u32 = WM_APP + 6;
const STARTUP: usize = 1;
const ABOUT: usize = 2;
const EXIT: usize = 3;
const CONNECTOR: usize = 4;
const UPDATE: usize = 5;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const REFRESH_TIMER_ID: usize = 1;
const CONNECTOR_TIMER_ID: usize = 2;
const CONNECTOR_REFRESH_MS: u32 = 120;
const CONNECTOR_IDLE_MS: u32 = 1000;
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

mod connector;
mod icon;
mod protocol;
mod updater;
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
                        format!("{BRIDGE_FILE} must be in the application folder."),
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
                    format!("Bridge failed: {}", self.status.describe()),
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
                    "Taskbar restoration was not confirmed. Explorer may have stopped responding.",
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
                            "Windows rejected legacy taskbar transparency.",
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
                        "Windows rejected legacy taskbar restoration.",
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
                "The application path is too long for Windows startup. Move the folder to a shorter path.",
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

fn settings_flag(value: PCWSTR, fallback: bool) -> Result<bool> {
    let mut data = [0u16; 16];
    let mut bytes = std::mem::size_of_val(&data) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(fallback);
    }
    status.ok()?;
    Ok(unsafe { *(data.as_ptr().cast::<u32>()) } != 0)
}

fn set_settings_flag(value: PCWSTR, enabled: bool) -> Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let word: u32 = if enabled { 1 } else { 0 };
        let bytes = std::slice::from_raw_parts((&word as *const u32).cast::<u8>(), 4);
        let result = RegSetValueExW(key, value, 0, REG_DWORD, Some(bytes));
        let _ = RegCloseKey(key);
        result.ok()
    }
}

fn apply_menu_theme() {
    unsafe {
        let mut data = 1u32;
        let mut bytes = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut bytes),
        );
        let dark = status.is_ok() && data == 0;
        if let Ok(uxtheme) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) {
            if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135usize as *const u8)) {
                let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
                let _ = set_mode(if dark { 2 } else { 3 });
            }
            if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136usize as *const u8)) {
                let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
                flush();
            }
            let _ = FreeLibrary(uxtheme);
        }
    }
}

fn show_error(error: &Error) {
    eprintln!("{error}");
    let text = wide(std::ffi::OsStr::new(&format!(
        "{error}

Share this error message when reporting an issue. Startup log: %LOCALAPPDATA%\\TransparentBar\\startup.log"
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
        let tip = wide(std::ffi::OsStr::new("TransparentBar - transparent taskbar"));
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        if !Shell_NotifyIconW(NIM_ADD, &icon).as_bool() {
            return Err(Error::new(E_FAIL, "Explorer has not accepted the tray icon yet. Wait until the taskbar is ready, then reopen the application."));
        }
        icon.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // Unsupported v4 keeps legacy notifications; GetLastError is not part of this API contract.
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &icon);
    }
    Ok(())
}

fn menu(hwnd: HWND, point: POINT) -> Result<usize> {
    apply_menu_theme();
    let enabled = startup_enabled()?;
    let connector = settings_flag(CONNECTOR_VALUE, true)?;
    unsafe {
        let popup = CreatePopupMenu()?;
        let result = (|| {
            AppendMenuW(
                popup,
                MF_STRING | if connector { MF_CHECKED } else { MF_UNCHECKED },
                CONNECTOR,
                w!("&Taskbar connector"),
            )?;
            AppendMenuW(
                popup,
                MF_STRING | if enabled { MF_CHECKED } else { MF_UNCHECKED },
                STARTUP,
                w!("Run at &sign-in"),
            )?;
            AppendMenuW(popup, MF_SEPARATOR, 0, None)?;
            if let Some(tag) = updater::available_tag() {
                let label = wide(std::ffi::OsStr::new(&format!("&Update available ({tag})")));
                AppendMenuW(popup, MF_STRING, UPDATE, PCWSTR(label.as_ptr()))?;
            } else {
                AppendMenuW(popup, MF_STRING, UPDATE, w!("&Check for updates"))?;
            }
            AppendMenuW(popup, MF_STRING, ABOUT, w!("&About / license"))?;
            AppendMenuW(popup, MF_STRING, EXIT, w!("E&xit"))?;
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

fn is_window_timer(hwnd: HWND, message: &MSG, id: usize) -> bool {
    message.message == WM_TIMER
        && message.hwnd == hwnd
        && message.wParam.0 == id
        && message.lParam.0 == 0
}

fn connector_interval(enabled: bool, active: bool) -> Option<u32> {
    enabled.then_some(if active {
        CONNECTOR_REFRESH_MS
    } else {
        CONNECTOR_IDLE_MS
    })
}

fn update_connector(
    hwnd: HWND,
    strip: &mut Option<connector::Connector>,
    enabled: bool,
    interval: &mut Option<u32>,
) {
    let active = strip
        .as_mut()
        .is_some_and(|strip| strip.update(enabled).unwrap_or(false));
    let next = connector_interval(enabled && strip.is_some(), active);
    if next != *interval {
        unsafe {
            let _ = KillTimer(hwnd, CONNECTOR_TIMER_ID);
            *interval = next.filter(|&ms| SetTimer(hwnd, CONNECTOR_TIMER_ID, ms, None) != 0);
            if next.is_some() && interval.is_none() {
                if let Some(strip) = strip {
                    strip.hide();
                }
                protocol::trace("Connector timer could not be created; strip hidden.");
            }
        }
    }
}

fn schedule(hwnd: HWND, delay_ms: u32) -> Result<()> {
    if unsafe { SetTimer(hwnd, REFRESH_TIMER_ID, delay_ms, None) } == 0 {
        Err(Error::from_win32())
    } else {
        Ok(())
    }
}

fn command(hwnd: HWND, id: usize) -> Result<bool> {
    match id {
        CONNECTOR => set_settings_flag(CONNECTOR_VALUE, !settings_flag(CONNECTOR_VALUE, true)?)?,
        STARTUP => set_startup(!startup_enabled()?)?,
        UPDATE => {
            if updater::available_tag().is_some() {
                updater::open_release_page(hwnd);
            } else {
                updater::check_in_background(hwnd, true, UPDATE_MESSAGE);
            }
        }
        ABOUT => unsafe {
            MessageBoxW(
                None,
                w!("TransparentBar 0.1.8
Native Rust, Windows 10 and 11 x64.
Copyright 2026 Rafly Firmansyah.
MIT License; may be redistributed under license terms. No warranty.
Source: github.com/Raflyf/TransparentBar

Startup settings apply to the current Windows user account only."),
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
        TRAY_MESSAGE if ((lp.0 as u32) & 0xffff) == NIN_BALLOONUSERCLICK => {
            let _ = PostMessageW(hwnd, TRAY_MESSAGE, wp, lp);
            LRESULT(0)
        }
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
    event: u32,
    target: HWND,
    id_object: i32,
    id_child: i32,
    _: u32,
    _: u32,
) {
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (id_object != OBJID_WINDOW.0 || id_child != 0 || target != GetForegroundWindow())
    {
        return;
    }
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
                EVENT_SYSTEM_MINIMIZEEND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let location_hook = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let mut service: Option<TaskbarService> = None;
            let mut connector = connector::Connector::create().ok();
            let mut connector_timer = None;
            let result = (|| {
                let mut connector_enabled = settings_flag(CONNECTOR_VALUE, true)?;
                let mut tray_ready = false;
                let mut update_checked = false;
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
                            connector_enabled =
                                settings_flag(CONNECTOR_VALUE, true).unwrap_or(false);
                            schedule(hwnd, 500)?;
                        }
                        WM_TIMER if is_window_timer(hwnd, &msg, CONNECTOR_TIMER_ID) => {
                            if connector_timer.is_some() {
                                update_connector(
                                    hwnd,
                                    &mut connector,
                                    connector_enabled,
                                    &mut connector_timer,
                                );
                            }
                        }
                        WM_TIMER if is_window_timer(hwnd, &msg, REFRESH_TIMER_ID) => {
                            // KillTimer leaves queued ticks behind; an already-removed timer is harmless.
                            let _ = KillTimer(hwnd, REFRESH_TIMER_ID);
                            update_connector(
                                hwnd,
                                &mut connector,
                                connector_enabled,
                                &mut connector_timer,
                            );
                            if RESTART.swap(false, Ordering::AcqRel) {
                                service = None;
                                generation += 1;
                                readiness = 20;
                                tray_ready = false;
                                retries = 3;
                            }
                            if !tray_ready {
                                match add_tray(hwnd, application_icon.0) {
                                    Ok(()) => {
                                        tray_ready = true;
                                        if !update_checked {
                                            update_checked = true;
                                            updater::check_in_background(
                                                hwnd,
                                                false,
                                                UPDATE_MESSAGE,
                                            );
                                        }
                                    }
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
                                        return Err(Error::new(E_FAIL, format!("Taskbar is not ready yet: {detail}. Log: %LOCALAPPDATA%\\TransparentBar\\startup.log")));
                                    }
                                    readiness -= 1;
                                    schedule(hwnd, 500)?;
                                }
                            }
                        }
                        TRAY_MESSAGE | MENU_MESSAGE => {
                            let notification = (msg.lParam.0 as u32) & 0xffff;
                            if msg.message == TRAY_MESSAGE
                                && notification == NIN_BALLOONUSERCLICK
                                && updater::available_tag().is_some()
                            {
                                updater::open_release_page(hwnd);
                            } else if msg.message == MENU_MESSAGE || is_tray_menu(notification) {
                                let mut point = POINT::default();
                                GetCursorPos(&mut point)?;
                                match menu(hwnd, point) {
                                    Ok(selected) => match command(hwnd, selected) {
                                        Ok(true) => PostQuitMessage(0),
                                        Ok(false) => {
                                            if selected == CONNECTOR {
                                                connector_enabled =
                                                    settings_flag(CONNECTOR_VALUE, true)
                                                        .unwrap_or(false);
                                                update_connector(
                                                    hwnd,
                                                    &mut connector,
                                                    connector_enabled,
                                                    &mut connector_timer,
                                                );
                                            }
                                        }
                                        Err(error) => show_error(&error),
                                    },
                                    Err(error) => show_error(&error),
                                }
                            }
                        }
                        UPDATE_MESSAGE => {
                            let manual = msg.wParam.0 != 0;
                            match msg.lParam.0 {
                                1 => {
                                    if let Some(tag) = updater::available_tag() {
                                        if manual {
                                            let text = wide(std::ffi::OsStr::new(&format!(
                                                "A new release ({tag}) is available on GitHub.\n\nCurrent version: v{}\n\nOpen the release download page now?",
                                                updater::CURRENT_VERSION
                                            )));
                                            if MessageBoxW(
                                                None,
                                                PCWSTR(text.as_ptr()),
                                                NAME,
                                                MB_YESNO | MB_ICONINFORMATION,
                                            ) == IDYES
                                            {
                                                updater::open_release_page(hwnd);
                                            }
                                        } else {
                                            updater::show_balloon(
                                                hwnd,
                                                &format!("Update available: {tag}"),
                                                "Click this notification or open the tray menu to download the latest release.",
                                            );
                                        }
                                    }
                                }
                                0 if manual => {
                                    let text = wide(std::ffi::OsStr::new(&format!(
                                        "You are using the latest version of TransparentBar (v{}).",
                                        updater::CURRENT_VERSION
                                    )));
                                    MessageBoxW(
                                        None,
                                        PCWSTR(text.as_ptr()),
                                        NAME,
                                        MB_OK | MB_ICONINFORMATION,
                                    );
                                }
                                _ if manual => {
                                    MessageBoxW(
                                        None,
                                        w!("Could not check for updates. Check your internet connection and try again."),
                                        NAME,
                                        MB_OK | MB_ICONWARNING,
                                    );
                                }
                                _ => (),
                            }
                        }
                        WM_COMMAND | COMMAND_MESSAGE => {
                            let selected = msg.wParam.0 & 0xffff;
                            match command(hwnd, selected) {
                                Ok(true) => break,
                                Ok(false) => {
                                    if selected == CONNECTOR {
                                        connector_enabled =
                                            settings_flag(CONNECTOR_VALUE, true).unwrap_or(false);
                                        update_connector(
                                            hwnd,
                                            &mut connector,
                                            connector_enabled,
                                            &mut connector_timer,
                                        );
                                    }
                                }
                                Err(error) => show_error(&error),
                            }
                        }
                        _ => {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                Ok(())
            })();
            let _ = KillTimer(hwnd, CONNECTOR_TIMER_ID);
            let _ = KillTimer(hwnd, REFRESH_TIMER_ID);
            drop(connector.take());
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
            for hook in [foreground_hook, location_hook] {
                if !hook.is_invalid() {
                    let _ = UnhookWinEvent(hook);
                }
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
        assert!(is_window_timer(window, &own, REFRESH_TIMER_ID));
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
                !is_window_timer(window, &foreign, REFRESH_TIMER_ID),
                "foreign timer was consumed"
            );
        }
    }

    #[test]
    fn connector_timer_is_owned_and_stops_when_disabled() {
        let window = HWND(0x100usize as *mut c_void);
        let tick = MSG {
            hwnd: window,
            message: WM_TIMER,
            wParam: WPARAM(CONNECTOR_TIMER_ID),
            ..Default::default()
        };
        assert!(is_window_timer(window, &tick, CONNECTOR_TIMER_ID));
        assert!(!is_window_timer(window, &tick, REFRESH_TIMER_ID));
        for foreign in [
            MSG {
                hwnd: HWND::default(),
                ..tick
            },
            MSG {
                wParam: WPARAM(CONNECTOR_TIMER_ID + 1),
                ..tick
            },
            MSG {
                lParam: LPARAM(1),
                ..tick
            },
        ] {
            assert!(!is_window_timer(window, &foreign, CONNECTOR_TIMER_ID));
        }
        assert_eq!(connector_interval(true, true), Some(120));
        assert_eq!(connector_interval(true, false), Some(1000));
        assert_eq!(connector_interval(false, true), None);
        assert_eq!(connector_interval(false, false), None);
    }

    #[test]
    fn connector_scanline_preserves_separate_colors_and_changes_with_content() {
        let row = [[30, 20, 10, 255], [200, 180, 160, 255], [60, 80, 100, 255]].concat();
        let capture = [row.clone(), row.clone()].concat();
        let colors = connector::smooth_scanline(&capture, 3, 2, 0);
        assert_eq!(colors, [(10, 20, 30), (160, 180, 200), (100, 80, 60)]);
        let mut pixels = vec![0; 3 * 4 * 4];
        connector::fill_gradient_columns(&mut pixels, 3, 4, &colors);
        assert_ne!(&pixels[0..4], &pixels[4..8]);
        assert_ne!(&pixels[4..8], &pixels[8..12]);
        assert!(pixels[3 * 3 * 4..].iter().all(|&byte| byte == 0));
        let mut scrolled = capture;
        scrolled[6] = 0;
        assert_ne!(connector::smooth_scanline(&scrolled, 3, 2, 0), colors);
        assert_eq!(
            connector::smooth_scanline(&row, 3, 1, 1),
            [(85, 100, 115), (90, 93, 96), (130, 130, 130)]
        );
        assert!(connector::smooth_scanline(&[], 0, 0, 8).is_empty());
    }

    #[test]
    fn startup_quotes_path_and_preserves_unicode() {
        let command = startup_command(Path::new(r"C:\My Apps\café\transparentbar.exe"));
        assert_eq!(command.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&command[..command.len() - 1]).unwrap(),
            r#""C:\My Apps\café\transparentbar.exe""#
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

    #[test]
    fn connector_gradient_fades_and_premultiplies_alpha() {
        let mut pixels = vec![0u8; 4 * 5 * 4];
        connector::fill_gradient_columns(&mut pixels, 4, 5, &[(200, 100, 50); 4]);
        let top_alpha = pixels[3];
        assert!(top_alpha > 150);
        assert_eq!(pixels[0], ((50u32 * u32::from(top_alpha)) / 255) as u8);
        assert_eq!(pixels[1], ((100u32 * u32::from(top_alpha)) / 255) as u8);
        assert_eq!(pixels[2], ((200u32 * u32::from(top_alpha)) / 255) as u8);
        let last = (4 * 4) * 4;
        assert_eq!(&pixels[last..last + 4], &[0, 0, 0, 0]);
        assert!(connector::is_shell_class("Shell_TrayWnd"));
        assert!(!connector::is_shell_class("Chrome_WidgetWin_1"));
        let bar = RECT {
            left: 0,
            top: 1032,
            right: 1920,
            bottom: 1080,
        };
        let maximised = RECT {
            left: -8,
            top: -8,
            right: 1928,
            bottom: 1040,
        };
        let fullscreen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert!(connector::is_docked_above_taskbar(&maximised, &bar));
        assert!(!connector::is_docked_above_taskbar(&fullscreen, &bar));
    }

    #[test]
    fn updater_parses_release_tags_and_compares_versions() {
        assert_eq!(
            updater::extract_tag_from_location(
                "https://github.com/Raflyf/TransparentBar/releases/tag/v0.1.6"
            ),
            Some("v0.1.6".to_string())
        );
        assert_eq!(
            updater::extract_tag_from_json(r#"{"tag_name": "v0.2.0", "draft": false}"#),
            Some("v0.2.0".to_string())
        );
        assert_eq!(
            updater::extract_tag_from_location(
                "https://github.com/Raflyf/TransparentBar/releases/tag/v0.1.7\0\0\0"
            ),
            Some("v0.1.7".to_string())
        );
        assert!(updater::is_newer("v0.1.6", "0.1.5"));
        assert!(updater::is_newer("v0.2.0", "0.1.5"));
        assert!(!updater::is_newer("v0.1.5", "0.1.5"));
        assert!(!updater::is_newer("v0.1.4", "0.1.5"));
    }
}
