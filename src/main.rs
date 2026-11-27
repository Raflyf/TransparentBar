#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use windows::{
    core::{s, w, Error, Result, PCSTR, PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            EqualSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        },
        Storage::FileSystem::{
            CreateFileW, GetDriveTypeW, GetFileInformationByHandle, GetFileVersionInfoSizeW,
            GetFileVersionInfoW, GetFinalPathNameByHandleW, VerQueryValueW,
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            READ_CONTROL, VOLUME_NAME_DOS,
        },
        System::{
            Com::*,
            LibraryLoader::*,
            Memory::{SetProcessWorkingSetSizeEx, SETPROCESSWORKINGSETSIZEEX_FLAGS},
            Registry::*,
            RemoteDesktop::*,
            SystemInformation::GetWindowsDirectoryW,
            SystemServices::*,
            Threading::*,
            WindowsProgramming::DRIVE_REMOTE,
        },
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
};

const NAME: PCWSTR = w!("TransparentBar");
const SUPPORT_URL: PCWSTR = w!("https://yapp.ink/raflyf");
const SAWERIA_URL: PCWSTR = w!("https://saweria.co/BuyMatcha");
const WINDOW_CLASS: PCWSTR = w!("TransparentBar.Window");
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const SETTINGS_KEY: PCWSTR = w!(r"Software\TransparentBar");
const CONNECTOR_VALUE: PCWSTR = w!("TaskbarConnector");
const PATTERN_VALUE: PCWSTR = w!("TaskbarPattern");
const REDUCE_MOTION_VALUE: PCWSTR = w!("ReduceMotion");
const AUTO_UPDATE_VALUE: PCWSTR = w!("CheckUpdatesAtStartup");
const TRAY_MESSAGE: u32 = WM_APP + 1;
const MENU_MESSAGE: u32 = WM_APP + 4;
const UPDATE_MESSAGE: u32 = WM_APP + 6;
const POWER_PAUSE_MESSAGE: u32 = WM_APP + 7;
const POWER_RESUME_MESSAGE: u32 = WM_APP + 8;
const CONNECTOR_FAST_MS: u32 = 16;
const CONNECTOR_SETTLED_MS: u32 = 64;
const CONNECTOR_QUIESCENT_MS: u32 = 250;
const STARTUP: usize = 1;
const ABOUT: usize = 2;
const EXIT: usize = 3;
const CONNECTOR: usize = 4;
const UPDATE: usize = 5;
const PATTERN: usize = 6;
const REDUCE_MOTION: usize = 7;
const SUPPORT: usize = 8;
const SAWERIA: usize = 9;
const AUTO_UPDATE: usize = 10;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const REFRESH_TIMER_ID: usize = 1;
const CONNECTOR_TIMER_ID: usize = 2;
const CONNECTOR_IDLE_MS: u32 = 1000;
const RECONNECT_COOLDOWN: Duration = Duration::from_secs(3);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
static CONNECTOR_ENABLED: AtomicBool = AtomicBool::new(false);
static CONNECTOR_VISIBLE: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static NEEDS_LEGACY_REFRESH: AtomicBool = AtomicBool::new(true);
static CONNECTOR_TIMER_MS: AtomicU32 = AtomicU32::new(0);

mod connector;
mod icon;
mod protocol;
mod updater;
use protocol::{
    event_name, format_init_data, generate_session_token, InitializeXaml, OwnerSecurityAttributes,
    StatusChannel, BRIDGE_CLSID, BRIDGE_FILE,
};
static RESTART: AtomicBool = AtomicBool::new(false);

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct OwnedSd(PSECURITY_DESCRIPTOR);
impl Drop for OwnedSd {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.0 .0));
            }
        }
    }
}

fn harden_process() {
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
        let _ = SetDllDirectoryW(w!(""));

        let mut ext_policy = PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY::default();
        // Bit 0: DisableExtensionPoints = 1
        ext_policy.Anonymous.Flags = 1;
        let _ = SetProcessMitigationPolicy(
            ProcessExtensionPointDisablePolicy,
            (&ext_policy as *const PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY).cast(),
            std::mem::size_of::<PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY>(),
        );

        let mut img_policy = PROCESS_MITIGATION_IMAGE_LOAD_POLICY::default();
        // Bit 0: NoRemoteImages = 1, Bit 1: NoLowMandatoryLabelImages = 1
        img_policy.Anonymous.Flags = 0b11;
        let _ = SetProcessMitigationPolicy(
            ProcessImageLoadPolicy,
            (&img_policy as *const PROCESS_MITIGATION_IMAGE_LOAD_POLICY).cast(),
            std::mem::size_of::<PROCESS_MITIGATION_IMAGE_LOAD_POLICY>(),
        );
    }
}

fn normalize_win32_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('\0');
    let stripped = trimmed
        .strip_prefix(r"\\?\")
        .unwrap_or(trimmed)
        .replace('/', "\\");
    stripped.to_lowercase()
}

fn strip_extended_prefix(path: &str) -> PathBuf {
    PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path))
}

fn is_expected_explorer_path(actual: &str, windows_dir: &str) -> bool {
    let base = normalize_win32_path(windows_dir);
    let base = base.trim_end_matches('\\');
    if base.is_empty() {
        return false;
    }
    let expected = format!(r"{base}\explorer.exe");
    normalize_win32_path(actual) == expected
}

fn verify_explorer_process(pid: u32) -> Result<()> {
    if pid == 0 {
        return Err(Error::from(E_ACCESSDENIED));
    }
    unsafe {
        let process = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?);
        let mut image_buf = vec![0u16; 1024];
        let mut image_len = image_buf.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_FORMAT(0),
            PWSTR(image_buf.as_mut_ptr()),
            &mut image_len,
        )?;
        let actual = String::from_utf16_lossy(&image_buf[..image_len as usize]);

        let mut win_buf = [0u16; 512];
        let win_len = GetWindowsDirectoryW(Some(&mut win_buf)) as usize;
        if win_len == 0 || win_len >= win_buf.len() {
            return Err(Error::from_win32());
        }
        let windows_dir = String::from_utf16_lossy(&win_buf[..win_len]);
        if !is_expected_explorer_path(&actual, &windows_dir) {
            return Err(Error::new(
                E_ACCESSDENIED,
                "Target taskbar window does not belong to Windows Explorer.",
            ));
        }
    }
    Ok(())
}

fn final_path_by_handle(handle: HANDLE) -> Result<String> {
    unsafe {
        let mut buf = vec![0u16; 1024];
        let len = GetFinalPathNameByHandleW(handle, &mut buf, VOLUME_NAME_DOS) as usize;
        if len == 0 {
            return Err(Error::from_win32());
        }
        if len >= buf.len() {
            buf.resize(len + 1, 0);
            let len = GetFinalPathNameByHandleW(handle, &mut buf, VOLUME_NAME_DOS) as usize;
            if len == 0 || len >= buf.len() {
                return Err(Error::from_win32());
            }
            return Ok(String::from_utf16_lossy(&buf[..len]));
        }
        Ok(String::from_utf16_lossy(&buf[..len]))
    }
}

fn reject_remote_or_unc_path(canonical_path: &str) -> Result<()> {
    let stripped = canonical_path
        .strip_prefix(r"\\?\")
        .unwrap_or(canonical_path);
    if stripped.starts_with(r"\\") || stripped.to_ascii_uppercase().starts_with(r"UNC\") {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL cannot be loaded from a network or UNC location.",
        ));
    }
    let bytes = stripped.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL must reside on a local drive.",
        ));
    }
    let root: Vec<u16> = format!("{}:\\", bytes[0] as char)
        .encode_utf16()
        .chain([0])
        .collect();
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if drive_type == DRIVE_REMOTE || drive_type <= 1 {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL cannot be loaded from a remote drive.",
        ));
    }
    Ok(())
}

fn file_owner_sid(handle: HANDLE) -> Result<(OwnedSd, PSID)> {
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut sd),
        );
        if err != ERROR_SUCCESS {
            return Err(Error::from(err.to_hresult()));
        }
        let sd = OwnedSd(sd);
        if owner.is_invalid() {
            return Err(Error::from(E_ACCESSDENIED));
        }
        Ok((sd, owner))
    }
}

#[cfg(target_env = "msvc")]
fn query_version_string(block: &[u8], sub_block: PCWSTR) -> Option<String> {
    unsafe {
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(block.as_ptr().cast(), sub_block, &mut ptr, &mut len).as_bool()
            || ptr.is_null()
            || len == 0
        {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize);
        let nul = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        Some(String::from_utf16_lossy(&slice[..nul]))
    }
}

#[cfg(target_env = "msvc")]
fn verify_bridge_version_resource(path: &Path) -> Result<()> {
    let path_wide = wide(path.as_os_str());
    unsafe {
        let mut handle = 0u32;
        let size = GetFileVersionInfoSizeW(PCWSTR(path_wide.as_ptr()), Some(&mut handle));
        if size == 0 {
            return Err(Error::new(
                E_ACCESSDENIED,
                "Bridge DLL is missing required PE version metadata.",
            ));
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(path_wide.as_ptr()),
            handle,
            size,
            block.as_mut_ptr().cast(),
        )?;
        let product = query_version_string(&block, w!(r"\StringFileInfo\040904b0\ProductName"));
        let internal = query_version_string(&block, w!(r"\StringFileInfo\040904b0\InternalName"));
        let version = query_version_string(&block, w!(r"\StringFileInfo\040904b0\FileVersion"));
        if product.as_deref() != Some("TransparentBar")
            || internal.as_deref() != Some("transparentbar_bridge")
            || version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
        {
            return Err(Error::new(
                E_ACCESSDENIED,
                "Bridge DLL version stamp does not match this TransparentBar build.",
            ));
        }
    }
    Ok(())
}

fn verify_bridge_dll() -> Result<(PathBuf, OwnedHandle)> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| Error::new(E_FAIL, "Could not resolve application directory."))?;
    let exe_dir_wide = wide(exe_dir.as_os_str());
    let dir_handle = OwnedHandle(unsafe {
        CreateFileW(
            PCWSTR(exe_dir_wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    });
    let mut dir_info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(dir_handle.0, &mut dir_info)?;
    }
    if (dir_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0
        || (dir_info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) == 0
    {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Application directory cannot be a symlink or reparse point.",
        ));
    }
    let canonical_dir = final_path_by_handle(dir_handle.0)?;
    reject_remote_or_unc_path(&canonical_dir)?;

    let exe_wide = wide(exe_path.as_os_str());
    let exe_handle = OwnedHandle(unsafe {
        CreateFileW(
            PCWSTR(exe_wide.as_ptr()),
            READ_CONTROL.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    });
    let (_exe_sd, exe_owner) = file_owner_sid(exe_handle.0)?;

    let bridge_candidate = exe_dir.join(BRIDGE_FILE);
    let bridge_wide = wide(bridge_candidate.as_os_str());
    // Hold FILE_SHARE_READ only (no write/delete share) across verification and injection to eliminate TOCTOU.
    let bridge_handle = OwnedHandle(unsafe {
        CreateFileW(
            PCWSTR(bridge_wide.as_ptr()),
            FILE_READ_DATA.0 | FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
        .map_err(|_| {
            Error::new(
                E_FAIL,
                format!("{BRIDGE_FILE} must be in the application folder."),
            )
        })?
    });

    let mut bridge_info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(bridge_handle.0, &mut bridge_info)?;
    }
    if (bridge_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0
        || (bridge_info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0
        || (bridge_info.nFileSizeHigh == 0 && bridge_info.nFileSizeLow == 0)
    {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL is invalid or uses a disallowed reparse point.",
        ));
    }

    let canonical_bridge = final_path_by_handle(bridge_handle.0)?;
    reject_remote_or_unc_path(&canonical_bridge)?;
    let bridge_clean = strip_extended_prefix(&canonical_bridge);
    let dir_clean = strip_extended_prefix(&canonical_dir);
    let same_dir = bridge_clean.parent().is_some_and(|parent| {
        normalize_win32_path(&parent.to_string_lossy())
            == normalize_win32_path(&dir_clean.to_string_lossy())
    });
    let same_name = bridge_clean
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(BRIDGE_FILE));
    if !same_dir || !same_name {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL resolved outside the application folder.",
        ));
    }

    let (_bridge_sd, bridge_owner) = file_owner_sid(bridge_handle.0)?;
    let token_buf = protocol::current_user_token_buffer()?;
    let current_sid = protocol::token_user_sid(&token_buf)?;
    let owner_ok = unsafe {
        EqualSid(bridge_owner, current_sid).is_ok() || EqualSid(bridge_owner, exe_owner).is_ok()
    };
    if !owner_ok {
        return Err(Error::new(
            E_ACCESSDENIED,
            "Bridge DLL owner does not match the current user or application binary.",
        ));
    }

    #[cfg(target_env = "msvc")]
    verify_bridge_version_resource(&bridge_clean)?;

    Ok((bridge_clean, bridge_handle))
}

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
            verify_explorer_process(shell)?;

            let host = GetCurrentProcessId();
            let inspect = std::env::args().any(|arg| arg == "--diagnose");
            let token = generate_session_token()?;
            let security = OwnerSecurityAttributes::current_user()?;
            let create = |kind: &str| -> Result<HANDLE> {
                let name = event_name(&token, kind);
                CreateEventW(Some(security.as_ptr()), true, false, PCWSTR(name.as_ptr()))
            };
            let mut session = Self {
                ready: HANDLE::default(),
                failed: HANDLE::default(),
                stop: HANDLE::default(),
                done: HANDLE::default(),
                status: StatusChannel::create(host, shell, generation, inspect, &token)?,
            };
            session.ready = create("Ready")?;
            session.failed = create("Failed")?;
            session.stop = create("Stop")?;
            session.done = create("Done")?;

            let (bridge_path, _bridge_lock) = verify_bridge_dll()?;
            let module = LoadLibraryExW(
                w!("Windows.UI.Xaml.dll"),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )?;
            let result = (|| -> Result<()> {
                let address = GetProcAddress(module, s!("InitializeXamlDiagnosticsEx"))
                    .ok_or_else(Error::from_win32)?;
                let initialize: InitializeXaml = std::mem::transmute(address);
                let bridge = wide(bridge_path.as_os_str());
                let init_str = format_init_data(host, shell, generation, inspect, &token)?;
                let init = wide(std::ffi::OsStr::new(&init_str));
                let seed = u32::from_str_radix(&token[..4], 16).unwrap_or(generation);
                let mut last_hr = E_FAIL;
                for attempt in 0..16u32 {
                    let slot = if attempt == 0 {
                        generation
                    } else {
                        seed.wrapping_add(attempt).max(1)
                    };
                    let endpoint =
                        wide(std::ffi::OsStr::new(&format!("VisualDiagConnection{slot}")));
                    let hr = initialize(
                        PCWSTR(endpoint.as_ptr()),
                        shell,
                        PCWSTR::null(),
                        PCWSTR(bridge.as_ptr()),
                        BRIDGE_CLSID,
                        PCWSTR(init.as_ptr()),
                    );
                    if hr.is_ok() {
                        protocol::trace("Host.Initialize S_OK");
                        return Ok(());
                    }
                    last_hr = hr;
                    if hr != ERROR_ALREADY_EXISTS.to_hresult() {
                        break;
                    }
                }
                last_hr.ok()
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
    let mut data: u32 = 0;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(fallback);
    }
    status.ok()?;
    Ok(data != 0)
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

#[cold]
#[inline(never)]
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

/// Hint Windows to release cold one-time startup/WinHTTP pages from the active working set.
#[cold]
#[inline(never)]
fn trim_working_set() {
    // SAFETY: Passing (usize::MAX, usize::MAX, 0) to SetProcessWorkingSetSizeEx is the documented
    // Win32 call to trim unused pages from the current process working set.
    unsafe {
        let _ = SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
            SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
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
        let tip = wide(std::ffi::OsStr::new(concat!(
            "TransparentBar v",
            env!("CARGO_PKG_VERSION")
        )));
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
    let pattern = settings_flag(PATTERN_VALUE, true)?;
    let reduce_motion = settings_flag(REDUCE_MOTION_VALUE, false)?;
    let auto_update = settings_flag(AUTO_UPDATE_VALUE, true)?;
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
            AppendMenuW(
                popup,
                MF_STRING | if pattern { MF_CHECKED } else { MF_UNCHECKED },
                PATTERN,
                w!("Connector &pattern"),
            )?;
            AppendMenuW(
                popup,
                MF_STRING
                    | if reduce_motion {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    },
                REDUCE_MOTION,
                w!("&Reduce connector animation"),
            )?;
            AppendMenuW(
                popup,
                MF_STRING
                    | if auto_update {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    },
                AUTO_UPDATE,
                w!("Check updates at &launch"),
            )?;
            AppendMenuW(popup, MF_SEPARATOR, 0, None)?;
            if let Some(tag) = updater::available_tag() {
                let label = wide(std::ffi::OsStr::new(&format!("&Update available ({tag})")));
                AppendMenuW(popup, MF_STRING, UPDATE, PCWSTR(label.as_ptr()))?;
            } else {
                AppendMenuW(popup, MF_STRING, UPDATE, w!("&Check for updates"))?;
            }
            AppendMenuW(popup, MF_STRING, ABOUT, w!("&About / license"))?;
            AppendMenuW(popup, MF_STRING, SUPPORT, w!("&Buy me a coffee"))?;
            AppendMenuW(popup, MF_STRING, SAWERIA, w!("Support via Sa&weria"))?;
            AppendMenuW(popup, MF_STRING, EXIT, w!("E&xit"))?;
            let _ = SetForegroundWindow(hwnd);
            // TPM_RETURNCMD returns the clicked ID directly without dispatching spoofable WM_COMMAND messages.
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

fn connector_interval(enabled: bool, state: connector::ConnectorState) -> Option<u32> {
    if !enabled {
        return None;
    }
    match state {
        connector::ConnectorState::Active => Some(CONNECTOR_FAST_MS),
        connector::ConnectorState::Settled => Some(CONNECTOR_SETTLED_MS),
        connector::ConnectorState::Quiescent => Some(CONNECTOR_QUIESCENT_MS),
        connector::ConnectorState::Hidden => Some(CONNECTOR_IDLE_MS),
    }
}

fn connector_style() -> connector::Style {
    connector::Style {
        pattern: settings_flag(PATTERN_VALUE, true).unwrap_or(true),
        reduce_motion: settings_flag(REDUCE_MOTION_VALUE, false).unwrap_or(false),
    }
}

fn sync_connector_timer(hwnd: HWND, next: Option<u32>, timer_ms: &AtomicU32) -> bool {
    let desired = next.unwrap_or(0);
    if timer_ms.load(Ordering::Relaxed) == desired {
        return true;
    }
    unsafe {
        let _ = KillTimer(hwnd, CONNECTOR_TIMER_ID);
        if desired == 0 {
            timer_ms.store(0, Ordering::Relaxed);
            return true;
        }
        if SetTimer(hwnd, CONNECTOR_TIMER_ID, desired, None) != 0 {
            timer_ms.store(desired, Ordering::Relaxed);
            true
        } else {
            timer_ms.store(0, Ordering::Relaxed);
            false
        }
    }
}

fn update_connector(
    hwnd: HWND,
    strip: &mut Option<connector::Connector>,
    enabled: bool,
    paused: bool,
    style: connector::Style,
) {
    let was_visible = CONNECTOR_VISIBLE.load(Ordering::Relaxed);
    if paused || !enabled {
        if let Some(s) = strip.as_mut() {
            s.hide();
        }
        if !enabled {
            // Release the layered window and GDI/USER resources when connector is turned off.
            drop(strip.take());
        }
        CONNECTOR_VISIBLE.store(false, Ordering::Relaxed);
        let _ = sync_connector_timer(hwnd, None, &CONNECTOR_TIMER_MS);
        if was_visible || paused {
            trim_working_set();
        }
        return;
    }
    if strip.is_none() {
        if !connector::has_eligible_foreground() {
            CONNECTOR_VISIBLE.store(false, Ordering::Relaxed);
            let next = connector_interval(true, connector::ConnectorState::Hidden);
            let _ = sync_connector_timer(hwnd, next, &CONNECTOR_TIMER_MS);
            return;
        }
        *strip = connector::Connector::create().ok();
    }
    let state = strip
        .as_mut()
        .and_then(|s| s.update(true, style).ok())
        .unwrap_or(connector::ConnectorState::Hidden);
    let now_visible = state != connector::ConnectorState::Hidden;
    CONNECTOR_VISIBLE.store(now_visible, Ordering::Relaxed);
    let next = connector_interval(strip.is_some(), state);
    if !sync_connector_timer(hwnd, next, &CONNECTOR_TIMER_MS) {
        if let Some(s) = strip.as_mut() {
            s.hide();
        }
        CONNECTOR_VISIBLE.store(false, Ordering::Relaxed);
        protocol::trace("Connector timer could not be created; strip hidden.");
    }
    if was_visible && !now_visible {
        trim_working_set();
    }
}

fn schedule(hwnd: HWND, delay_ms: u32) -> Result<()> {
    if unsafe { SetTimer(hwnd, REFRESH_TIMER_ID, delay_ms, None) } == 0 {
        Err(Error::from_win32())
    } else {
        Ok(())
    }
}

fn reconnect_wait_ms(last_connect: Option<Instant>, now: Instant) -> Option<u32> {
    let elapsed = now.saturating_duration_since(last_connect?);
    if elapsed >= RECONNECT_COOLDOWN {
        None
    } else {
        let remaining = RECONNECT_COOLDOWN - elapsed;
        Some(remaining.as_millis().clamp(100, u32::MAX as u128) as u32)
    }
}

unsafe fn open_url(hwnd: HWND, url: PCWSTR) -> Result<()> {
    let mut request = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        hwnd,
        lpVerb: w!("open"),
        lpFile: url,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    ShellExecuteExW(&mut request)
}

fn command(hwnd: HWND, id: usize) -> Result<bool> {
    match id {
        CONNECTOR => set_settings_flag(CONNECTOR_VALUE, !settings_flag(CONNECTOR_VALUE, true)?)?,
        PATTERN => set_settings_flag(PATTERN_VALUE, !settings_flag(PATTERN_VALUE, true)?)?,
        REDUCE_MOTION => set_settings_flag(
            REDUCE_MOTION_VALUE,
            !settings_flag(REDUCE_MOTION_VALUE, false)?,
        )?,
        AUTO_UPDATE => {
            set_settings_flag(AUTO_UPDATE_VALUE, !settings_flag(AUTO_UPDATE_VALUE, true)?)?
        }
        STARTUP => set_startup(!startup_enabled()?)?,
        UPDATE => {
            if updater::available_tag().is_some() {
                updater::open_release_page(hwnd);
            } else {
                updater::check_in_background(hwnd, true, UPDATE_MESSAGE);
            }
        }
        SUPPORT => unsafe { open_url(hwnd, SUPPORT_URL)? },
        SAWERIA => unsafe { open_url(hwnd, SAWERIA_URL)? },
        ABOUT => unsafe {
            MessageBoxW(
                None,
                w!("TransparentBar 0.2.7
Native Rust, Windows 10 and 11 x64.
Copyright 2026 Rafly Firmansyah.
MIT License; may be redistributed under license terms. No warranty.
Source: github.com/Raflyf/TransparentBar
Support (optional): https://yapp.ink/raflyf
Saweria (optional): https://saweria.co/BuyMatcha

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
        WM_POWERBROADCAST => {
            let event = wp.0 as u32;
            if event == PBT_APMSUSPEND {
                let _ = PostMessageW(hwnd, POWER_PAUSE_MESSAGE, WPARAM(0), LPARAM(0));
            } else if event == PBT_APMRESUMEAUTOMATIC || event == PBT_APMRESUMESUSPEND {
                let _ = PostMessageW(hwnd, POWER_RESUME_MESSAGE, WPARAM(0), LPARAM(0));
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            let event = wp.0 as u32;
            if event == WTS_SESSION_LOCK {
                let _ = PostMessageW(hwnd, POWER_PAUSE_MESSAGE, WPARAM(0), LPARAM(0));
            } else if event == WTS_SESSION_UNLOCK {
                let _ = PostMessageW(hwnd, POWER_RESUME_MESSAGE, WPARAM(0), LPARAM(0));
            }
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
    if PAUSED.load(Ordering::Relaxed) {
        return;
    }
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (id_object != OBJID_WINDOW.0 || id_child != 0 || target != GetForegroundWindow())
    {
        return;
    }
    let raw = MAIN_HWND.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut c_void);
    if NEEDS_LEGACY_REFRESH.load(Ordering::Relaxed) {
        SetTimer(hwnd, REFRESH_TIMER_ID, 100, None);
    }
    if CONNECTOR_ENABLED.load(Ordering::Relaxed) {
        // Skip waking on normal non-maximized window drags while the strip is already hidden.
        let wake = event != EVENT_OBJECT_LOCATIONCHANGE
            || CONNECTOR_VISIBLE.load(Ordering::Relaxed)
            || IsZoomed(target).as_bool();
        if wake && CONNECTOR_TIMER_MS.load(Ordering::Relaxed) != CONNECTOR_FAST_MS {
            // Record the real OS timer period so update_connector relaxes back to idle/settled on the next tick.
            if SetTimer(hwnd, CONNECTOR_TIMER_ID, CONNECTOR_FAST_MS, None) != 0 {
                CONNECTOR_TIMER_MS.store(CONNECTOR_FAST_MS, Ordering::Relaxed);
            }
        }
    }
}

fn sync_location_hook(hook: &mut HWINEVENTHOOK, enabled: bool) {
    unsafe {
        if enabled {
            if hook.is_invalid() {
                *hook = SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(foreground_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
            }
        } else if !hook.is_invalid() {
            let _ = UnhookWinEvent(*hook);
            *hook = HWINEVENTHOOK::default();
        }
    }
}

fn cleanup_stale_bridges_in(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let is_bridge_artifact = name.starts_with("transparentbar_bridge-")
            && (name.ends_with(".dll") || name.ends_with(".dll.old"));
        if is_bridge_artifact && !name.eq_ignore_ascii_case(BRIDGE_FILE) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn cleanup_stale_bridges() {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        cleanup_stale_bridges_in(&dir);
    }
}

fn run() -> Result<()> {
    unsafe {
        let security = OwnerSecurityAttributes::current_user()?;
        let mutex = CreateMutexW(
            Some(security.as_ptr()),
            false,
            w!(r"Local\TransparentBar.SingleInstance"),
        )?;
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let result = (|| {
            if exists {
                if let Ok(hwnd) = FindWindowW(WINDOW_CLASS, None) {
                    PostMessageW(hwnd, MENU_MESSAGE, WPARAM(0), LPARAM(0))?;
                }
                return Ok(());
            }
            cleanup_stale_bridges();
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
            MAIN_HWND.store(hwnd.0 as usize, Ordering::Release);
            let foreground_hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_MINIMIZEEND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let mut location_hook = HWINEVENTHOOK::default();
            let mut service: Option<TaskbarService> = None;
            let mut connector: Option<connector::Connector> = None;
            let session_registered =
                WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();
            let mut paused = false;
            PAUSED.store(false, Ordering::Relaxed);
            NEEDS_LEGACY_REFRESH.store(true, Ordering::Relaxed);
            CONNECTOR_VISIBLE.store(false, Ordering::Relaxed);
            CONNECTOR_TIMER_MS.store(0, Ordering::Relaxed);
            let result = (|| {
                let mut connector_enabled = settings_flag(CONNECTOR_VALUE, true)?;
                let mut cached_style = connector_style();
                CONNECTOR_ENABLED.store(connector_enabled, Ordering::Relaxed);
                sync_location_hook(&mut location_hook, connector_enabled);
                let mut tray_ready = false;
                let mut update_checked = false;
                let mut trimmed_after_startup = false;
                let mut retries = 3u8;
                let mut generation = 1u32;
                let mut readiness = 20u8;
                let mut last_connect: Option<Instant> = None;
                schedule(hwnd, 500)?;
                trim_working_set();
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
                        POWER_PAUSE_MESSAGE => {
                            paused = true;
                            PAUSED.store(true, Ordering::Relaxed);
                            update_connector(
                                hwnd,
                                &mut connector,
                                connector_enabled,
                                paused,
                                cached_style,
                            );
                        }
                        POWER_RESUME_MESSAGE => {
                            paused = false;
                            PAUSED.store(false, Ordering::Relaxed);
                            update_connector(
                                hwnd,
                                &mut connector,
                                connector_enabled,
                                paused,
                                cached_style,
                            );
                            schedule(hwnd, 500)?;
                        }
                        WM_TIMER if is_window_timer(hwnd, &msg, CONNECTOR_TIMER_ID) => {
                            if CONNECTOR_TIMER_MS.load(Ordering::Relaxed) != 0 {
                                update_connector(
                                    hwnd,
                                    &mut connector,
                                    connector_enabled,
                                    paused,
                                    cached_style,
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
                                paused,
                                cached_style,
                            );
                            if RESTART.swap(false, Ordering::AcqRel) {
                                service = None;
                                generation = generation.wrapping_add(1).max(1);
                                readiness = 20;
                                tray_ready = false;
                                trimmed_after_startup = false;
                                NEEDS_LEGACY_REFRESH.store(true, Ordering::Relaxed);
                                retries = 3;
                            }
                            if !tray_ready {
                                match add_tray(hwnd, application_icon.0) {
                                    Ok(()) => {
                                        tray_ready = true;
                                        if !update_checked {
                                            update_checked = true;
                                            if settings_flag(AUTO_UPDATE_VALUE, true)
                                                .unwrap_or(true)
                                            {
                                                updater::check_in_background(
                                                    hwnd,
                                                    false,
                                                    UPDATE_MESSAGE,
                                                );
                                            }
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
                                let now = Instant::now();
                                if let Some(wait_ms) = reconnect_wait_ms(last_connect, now) {
                                    schedule(hwnd, wait_ms)?;
                                    continue;
                                }
                                last_connect = Some(now);
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
                                } else {
                                    // XAML bridge stays active via Explorer callbacks; only legacy Win10 needs foreground refreshes.
                                    NEEDS_LEGACY_REFRESH
                                        .store(service.xaml.is_none(), Ordering::Relaxed);
                                    if !trimmed_after_startup {
                                        trimmed_after_startup = true;
                                        trim_working_set();
                                    }
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
                                                CONNECTOR_ENABLED
                                                    .store(connector_enabled, Ordering::Relaxed);
                                                sync_location_hook(
                                                    &mut location_hook,
                                                    connector_enabled,
                                                );
                                                update_connector(
                                                    hwnd,
                                                    &mut connector,
                                                    connector_enabled,
                                                    paused,
                                                    cached_style,
                                                );
                                            }
                                            if selected == PATTERN || selected == REDUCE_MOTION {
                                                cached_style = connector_style();
                                                update_connector(
                                                    hwnd,
                                                    &mut connector,
                                                    connector_enabled,
                                                    paused,
                                                    cached_style,
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
                            trim_working_set();
                        }
                        _ => {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                Ok(())
            })();
            MAIN_HWND.store(0, Ordering::Release);
            CONNECTOR_ENABLED.store(false, Ordering::Relaxed);
            CONNECTOR_VISIBLE.store(false, Ordering::Relaxed);
            CONNECTOR_TIMER_MS.store(0, Ordering::Relaxed);
            let _ = KillTimer(hwnd, CONNECTOR_TIMER_ID);
            let _ = KillTimer(hwnd, REFRESH_TIMER_ID);
            if session_registered {
                let _ = WTSUnRegisterSessionNotification(hwnd);
            }
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

/// Per-monitor DPI awareness keeps window geometry and screen capture in the same device pixels
/// at any scaling. Without it a scaled display samples rows far above the real window edge.
/// SetProcessDpiAwarenessContext needs Windows 10 1607, so it is resolved at runtime.
fn enable_dpi_awareness() -> bool {
    unsafe {
        if let Ok(user32) = GetModuleHandleW(w!("user32.dll")) {
            if let Some(address) = GetProcAddress(user32, s!("SetProcessDpiAwarenessContext")) {
                let set: unsafe extern "system" fn(isize) -> BOOL = std::mem::transmute(address);
                if set(-4).as_bool() {
                    return true; // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
                }
            }
        }
        SetProcessDPIAware().as_bool()
    }
}

fn diagnose() -> Result<()> {
    protocol::trace("Host.Diagnose start");
    let session = XamlSession::connect(2)?;
    let start = Instant::now();
    let mut ready = false;
    unsafe {
        while start.elapsed() < Duration::from_secs(8) {
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
    harden_process();
    protocol::trim_log_if_oversized();
    protocol::trace("Host.Main");
    enable_dpi_awareness();
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
    fn process_requests_per_monitor_v2_dpi_awareness() {
        type Current = unsafe extern "system" fn() -> isize;
        type Same = unsafe extern "system" fn(isize, isize) -> BOOL;
        unsafe {
            enable_dpi_awareness();
            let user32 = GetModuleHandleW(w!("user32.dll")).unwrap();
            let current: Current =
                GetProcAddress(user32, s!("GetThreadDpiAwarenessContext")).unwrap();
            let same: Same = std::mem::transmute(
                GetProcAddress(user32, s!("AreDpiAwarenessContextsEqual")).unwrap(),
            );
            assert!(
                same(current(), -4).as_bool(),
                "thread is not per-monitor aware v2"
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
        assert_eq!(
            connector_interval(true, connector::ConnectorState::Active),
            Some(16)
        );
        assert_eq!(
            connector_interval(true, connector::ConnectorState::Settled),
            Some(64)
        );
        assert_eq!(
            connector_interval(true, connector::ConnectorState::Quiescent),
            Some(250)
        );
        assert_eq!(
            connector_interval(true, connector::ConnectorState::Hidden),
            Some(1000)
        );
        assert_eq!(
            connector_interval(false, connector::ConnectorState::Active),
            None
        );
        assert_eq!(
            connector_interval(false, connector::ConnectorState::Settled),
            None
        );
        assert_eq!(
            connector_interval(false, connector::ConnectorState::Quiescent),
            None
        );
        assert_eq!(
            connector_interval(false, connector::ConnectorState::Hidden),
            None
        );
    }

    #[test]
    fn connector_scanline_preserves_separate_colors_and_changes_with_content() {
        let row = [[30, 20, 10, 255], [200, 180, 160, 255], [60, 80, 100, 255]].concat();
        let capture = [row.clone(), row.clone()].concat();
        let colors = connector::smooth_scanline(&capture, 3, 2, 0);
        assert_eq!(colors, [(10, 20, 30), (160, 180, 200), (100, 80, 60)]);
        let mut pixels = vec![0; 3 * 4 * 4];
        connector::fill_gradient_columns(
            &mut pixels,
            3,
            4,
            &colors,
            &connector::Style {
                pattern: false,
                reduce_motion: false,
            },
        );
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
    fn support_destination_is_https_and_consistent() {
        let url = unsafe { SUPPORT_URL.to_string().unwrap() };
        let saweria = unsafe { SAWERIA_URL.to_string().unwrap() };
        assert_eq!(url, "https://yapp.ink/raflyf");
        assert_eq!(saweria, "https://saweria.co/BuyMatcha");
        assert_eq!(
            include_str!("../.github/FUNDING.yml").trim(),
            format!("custom: ['{url}', '{saweria}']")
        );
        let readme = include_str!("../README.md");
        assert!(readme.contains(&format!("[buy me a coffee]({url})")));
        assert!(readme.contains(&format!("[Saweria]({saweria})")));
        let commands = [
            STARTUP,
            ABOUT,
            EXIT,
            CONNECTOR,
            UPDATE,
            PATTERN,
            REDUCE_MOTION,
            SUPPORT,
            SAWERIA,
            AUTO_UPDATE,
        ];
        for (index, id) in commands.iter().enumerate() {
            assert!(!commands[..index].contains(id), "duplicate menu command ID");
        }
        assert!(
            !command(HWND::default(), 0).unwrap(),
            "cancel must not exit"
        );
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
        connector::fill_gradient_columns(
            &mut pixels,
            4,
            5,
            &[(200, 100, 50); 4],
            &connector::Style {
                pattern: false,
                reduce_motion: false,
            },
        );
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
        assert!(connector::is_docked_above_taskbar(
            &maximised,
            &bar,
            &connector::metrics(96)
        ));
        assert!(!connector::is_docked_above_taskbar(
            &fullscreen,
            &bar,
            &connector::metrics(96)
        ));
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
            updater::extract_tag_from_location("https://evil.example/releases/tag/v9.9.9"),
            None
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

    #[test]
    fn explorer_path_verification_matches_only_system_root_explorer() {
        assert!(is_expected_explorer_path(
            r"C:\Windows\explorer.exe",
            r"C:\Windows"
        ));
        assert!(is_expected_explorer_path(
            r"\\?\c:\WINDOWS\EXPLORER.EXE",
            r"C:\Windows\"
        ));
        assert!(!is_expected_explorer_path(
            r"C:\Users\Public\explorer.exe",
            r"C:\Windows"
        ));
        assert!(!is_expected_explorer_path(
            r"C:\Windows\System32\explorer.exe",
            r"C:\Windows"
        ));
        assert!(!is_expected_explorer_path(
            r"C:\Windows\notepad.exe",
            r"C:\Windows"
        ));
    }

    #[test]
    fn remote_and_unc_paths_are_rejected_for_bridge_dll() {
        assert!(reject_remote_or_unc_path(r"\\server\share\transparentbar").is_err());
        assert!(reject_remote_or_unc_path(r"\\?\UNC\server\share\transparentbar").is_err());
        assert!(reject_remote_or_unc_path(r"relative\path").is_err());
    }

    #[test]
    fn reconnect_cooldown_rate_limits_reattachment() {
        let base = Instant::now();
        assert_eq!(reconnect_wait_ms(None, base), None);
        assert!(reconnect_wait_ms(Some(base), base + Duration::from_millis(500)).is_some());
        assert_eq!(
            reconnect_wait_ms(Some(base), base + RECONNECT_COOLDOWN),
            None
        );
    }

    #[test]
    fn stale_bridge_cleanup_removes_only_older_bridge_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "transparentbar-bridge-cleanup-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let current = root.join(BRIDGE_FILE);
        let old_dll = root.join("transparentbar_bridge-0.2.4.dll");
        let old_bak = root.join("transparentbar_bridge-0.2.4.dll.old");
        let unrelated = root.join("README.md");
        for file in [&current, &old_dll, &old_bak, &unrelated] {
            std::fs::write(file, b"x").unwrap();
        }
        cleanup_stale_bridges_in(&root);
        assert!(current.exists());
        assert!(!old_dll.exists());
        assert!(!old_bak.exists());
        assert!(unrelated.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn connector_timer_sync_reprograms_when_foreground_wake_overrides_period() {
        let tracked = AtomicU32::new(CONNECTOR_IDLE_MS);
        // Simulate foreground_changed accelerating the OS timer to 16 ms.
        tracked.store(CONNECTOR_FAST_MS, Ordering::Relaxed);
        // Next idle tick must observe 16 != 1000 so it reprograms back to 1000 ms (not stuck at 16 ms).
        let desired = connector_interval(true, connector::ConnectorState::Hidden).unwrap();
        assert_ne!(tracked.load(Ordering::Relaxed), desired);
        assert_eq!(desired, CONNECTOR_IDLE_MS);
    }
}
