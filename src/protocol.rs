use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use windows::{
    core::{Error, Result, GUID, HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            Cryptography::{BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
            GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            TOKEN_QUERY, TOKEN_USER,
        },
        System::{
            Memory::*,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
};

#[allow(dead_code)]
pub const BRIDGE_FILE: &str = concat!("transparentbar_bridge-", env!("CARGO_PKG_VERSION"), ".dll");
pub const BRIDGE_CLSID: GUID = GUID::from_u128(0x54cd5c4a_0d6d_4ef0_9c4b_94e2828d2351);
const STATUS_MAGIC: u32 = 0x54425231;
pub const SESSION_TOKEN_HEX_LEN: usize = 32;

#[allow(dead_code)]
pub type InitializeXaml =
    unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, GUID, PCWSTR) -> HRESULT;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitData {
    pub host: u32,
    pub shell: u32,
    pub generation: u32,
    pub inspect: bool,
    pub token: String,
}

pub fn is_valid_token(token: &str) -> bool {
    token.len() == SESSION_TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[allow(dead_code)]
pub fn generate_session_token() -> Result<String> {
    let mut bytes = [0u8; SESSION_TOKEN_HEX_LEN / 2];
    unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut bytes,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
        .ok()?;
    }
    if bytes.iter().all(|&b| b == 0) {
        return Err(Error::from(E_FAIL));
    }
    let mut hex = String::with_capacity(SESSION_TOKEN_HEX_LEN);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        hex.push(HEX[(b >> 4) as usize] as char);
        hex.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok(hex)
}

#[allow(dead_code)]
pub fn format_init_data(
    host: u32,
    shell: u32,
    generation: u32,
    inspect: bool,
    token: &str,
) -> Result<String> {
    if host == 0 || shell == 0 || generation == 0 || !is_valid_token(token) {
        return Err(Error::from(E_INVALIDARG));
    }
    if inspect {
        Ok(format!("{host}:{shell}:{generation}:{token};inspect"))
    } else {
        Ok(format!("{host}:{shell}:{generation}:{token}"))
    }
}

#[allow(dead_code)]
pub fn parse_init_data(raw: &str) -> Result<InitData> {
    let trimmed = raw.trim_end_matches('\0');
    let (core, inspect) = match trimmed.split_once(';') {
        Some((head, "inspect")) => (head, true),
        Some(_) => return Err(Error::from(E_INVALIDARG)),
        None => (trimmed, false),
    };
    let mut parts = core.split(':');
    let host: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::from(E_INVALIDARG))?;
    let shell: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::from(E_INVALIDARG))?;
    let generation: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::from(E_INVALIDARG))?;
    let token = parts.next().ok_or_else(|| Error::from(E_INVALIDARG))?;
    if parts.next().is_some()
        || host == 0
        || shell == 0
        || generation == 0
        || !is_valid_token(token)
    {
        return Err(Error::from(E_INVALIDARG));
    }
    Ok(InitData {
        host,
        shell,
        generation,
        inspect,
        token: token.to_string(),
    })
}

pub fn event_name(token: &str, kind: &str) -> Vec<u16> {
    format!(
        r"Local\TransparentBar.{}.{token}.{kind}",
        env!("CARGO_PKG_VERSION")
    )
    .encode_utf16()
    .chain([0])
    .collect()
}

fn status_name(token: &str) -> Vec<u16> {
    format!(
        r"Local\TransparentBar.{}.{token}.Status",
        env!("CARGO_PKG_VERSION")
    )
    .encode_utf16()
    .chain([0])
    .collect()
}

struct TokenHandle(HANDLE);
impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Returns the current process token user buffer so callers can access `TOKEN_USER.User.Sid`.
#[allow(dead_code)]
pub fn current_user_token_buffer() -> Result<Vec<u8>> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = TokenHandle(token);
        let mut needed = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return Err(Error::from_win32());
        }
        let mut buffer = vec![0u8; needed as usize];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )?;
        Ok(buffer)
    }
}

#[allow(dead_code)]
pub fn token_user_sid(buffer: &[u8]) -> Result<PSID> {
    if buffer.len() < std::mem::size_of::<TOKEN_USER>() {
        return Err(Error::from(E_INVALIDARG));
    }
    let user = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
    if user.User.Sid.is_invalid() {
        return Err(Error::from(E_FAIL));
    }
    Ok(user.User.Sid)
}

/// Explicit protected DACL granting access strictly to the current user SID.
#[allow(dead_code)]
pub struct OwnerSecurityAttributes {
    sd: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

impl OwnerSecurityAttributes {
    #[allow(dead_code)]
    pub fn current_user() -> Result<Self> {
        unsafe {
            let token_buf = current_user_token_buffer()?;
            let sid = token_user_sid(&token_buf)?;
            let mut sid_pwstr = PWSTR::null();
            ConvertSidToStringSidW(sid, &mut sid_pwstr)?;
            let sid_string = sid_pwstr.to_string();
            let _ = LocalFree(HLOCAL(sid_pwstr.0.cast()));
            let sid_string = sid_string.map_err(|_| Error::from(E_FAIL))?;

            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid_string})")
                .encode_utf16()
                .chain([0])
                .collect();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )?;
            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: FALSE,
            };
            Ok(Self { sd, attrs })
        }
    }

    #[allow(dead_code)]
    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

impl Drop for OwnerSecurityAttributes {
    fn drop(&mut self) {
        if !self.sd.0.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.sd.0));
            }
        }
    }
}

#[repr(C)]
pub struct StartupStatus {
    magic: AtomicU32,
    host: AtomicU32,
    shell: AtomicU32,
    generation: AtomicU32,
    inspect: AtomicU32,
    pub stage: AtomicU32,
    pub error: AtomicI32,
    pub callbacks: AtomicU32,
    pub frames: AtomicU32,
    pub fills: AtomicU32,
    pub borders: AtomicU32,
}

pub struct StatusChannel {
    mapping: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// The mapped object contains atomic fields only.
unsafe impl Send for StatusChannel {}
unsafe impl Sync for StatusChannel {}

impl StatusChannel {
    #[allow(dead_code)]
    pub fn create(
        host: u32,
        shell: u32,
        generation: u32,
        inspect: bool,
        token: &str,
    ) -> Result<Self> {
        if host == 0 || shell == 0 || generation == 0 || !is_valid_token(token) {
            return Err(Error::from(E_INVALIDARG));
        }
        unsafe {
            let security = OwnerSecurityAttributes::current_user()?;
            let name = status_name(token);
            let mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(security.as_ptr()),
                PAGE_READWRITE,
                0,
                std::mem::size_of::<StartupStatus>() as u32,
                PCWSTR(name.as_ptr()),
            )?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mapping);
                return Err(Error::new(
                    E_FAIL,
                    "The previous bridge session has not finished yet. Close the existing instance before reopening.",
                ));
            }
            let channel = Self::map(mapping)?;
            let data = channel.data();
            data.host.store(host, Ordering::Relaxed);
            data.shell.store(shell, Ordering::Relaxed);
            data.generation.store(generation, Ordering::Relaxed);
            data.inspect.store(u32::from(inspect), Ordering::Relaxed);
            data.stage.store(0, Ordering::Relaxed);
            data.error.store(0, Ordering::Relaxed);
            data.callbacks.store(0, Ordering::Relaxed);
            data.frames.store(0, Ordering::Relaxed);
            data.fills.store(0, Ordering::Relaxed);
            data.borders.store(0, Ordering::Relaxed);
            data.magic.store(STATUS_MAGIC, Ordering::Release);
            Ok(channel)
        }
    }

    #[allow(dead_code)]
    pub fn open(token: &str, shell: u32) -> Result<Self> {
        if shell == 0 || !is_valid_token(token) {
            return Err(Error::from(E_INVALIDARG));
        }
        let name = status_name(token);
        let mapping =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr()))? };
        let channel = Self::map(mapping)?;
        let data = channel.data();
        if data.magic.load(Ordering::Acquire) != STATUS_MAGIC
            || data.shell.load(Ordering::Acquire) != shell
        {
            return Err(Error::from(E_INVALIDARG));
        }
        Ok(channel)
    }

    fn map(mapping: HANDLE) -> Result<Self> {
        let view = unsafe {
            MapViewOfFile(
                mapping,
                FILE_MAP_ALL_ACCESS,
                0,
                0,
                std::mem::size_of::<StartupStatus>(),
            )
        };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(Error::from_win32());
        }
        Ok(Self { mapping, view })
    }

    pub fn data(&self) -> &StartupStatus {
        unsafe { &*self.view.Value.cast::<StartupStatus>() }
    }

    #[allow(dead_code)]
    pub fn initialization(&self) -> Result<(u32, u32, u32, bool)> {
        let data = self.data();
        if data.magic.load(Ordering::Acquire) != STATUS_MAGIC {
            return Err(Error::from(E_INVALIDARG));
        }
        let host = data.host.load(Ordering::Acquire);
        let shell = data.shell.load(Ordering::Acquire);
        let generation = data.generation.load(Ordering::Acquire);
        let inspect = data.inspect.load(Ordering::Acquire);
        if host == 0 || shell == 0 || generation == 0 || inspect > 1 {
            return Err(Error::from(E_INVALIDARG));
        }
        Ok((host, shell, generation, inspect != 0))
    }

    #[allow(dead_code)]
    pub fn stage(&self, stage: u32) {
        self.data().stage.fetch_max(stage, Ordering::AcqRel);
    }

    #[allow(dead_code)]
    pub fn fail(&self, error: HRESULT) {
        let _ = self
            .data()
            .error
            .compare_exchange(0, error.0, Ordering::AcqRel, Ordering::Acquire);
    }

    #[allow(dead_code)]
    pub fn describe(&self) -> String {
        let data = self.data();
        let inspect = data.inspect.load(Ordering::Acquire) != 0;
        let stage = match data.stage.load(Ordering::Acquire) {
            0 => "DLL/COM not responding yet",
            1 => "COM factory",
            2 => "SetSite",
            3 => "XAML interface",
            4 => "UI dispatcher",
            5 => "Session channel",
            6 => "Callback registration",
            7 => "Callback received",
            8 => "Taskbar frame",
            9 => "BackgroundFill",
            10 if inspect => "Elements found (read-only diagnostic)",
            10 => "Transparency confirmed",
            11 => "Restoration",
            12 => "Restoration complete",
            _ => "Unknown stage",
        };
        format!(
            "{stage}; callbacks={}, frames={}, fill={}, border={}",
            data.callbacks.load(Ordering::Acquire),
            data.frames.load(Ordering::Acquire),
            data.fills.load(Ordering::Acquire),
            data.borders.load(Ordering::Acquire)
        )
    }
}

impl Drop for StatusChannel {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.mapping);
        }
    }
}

const MAX_LOG_BYTES: u64 = 64 * 1024;

#[cold]
#[inline(never)]
#[allow(dead_code)]
pub fn trim_log_if_oversized() {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let path = std::path::PathBuf::from(local)
        .join("TransparentBar")
        .join("startup.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::write(&path, b"");
        }
    }
}

#[cold]
#[inline(never)]
pub fn trace(stage: &str) {
    eprintln!("{stage}");
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let directory = std::path::PathBuf::from(local).join("TransparentBar");
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("startup.log"))
        {
            let _ = writeln!(file, "{stage}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_names_and_layout_are_stable() {
        let token_a = "0123456789abcdef0123456789abcdef";
        let token_b = "fedcba9876543210fedcba9876543210";
        assert_ne!(event_name(token_a, "Ready"), event_name(token_b, "Ready"));
        assert_ne!(event_name(token_a, "Ready"), event_name(token_a, "Stop"));
        assert_ne!(status_name(token_a), status_name(token_b));
        assert_eq!(std::mem::size_of::<StartupStatus>(), 44);
        assert_eq!(std::mem::offset_of!(StartupStatus, error), 24);
    }

    #[test]
    fn session_token_generation_is_unique_and_valid() {
        let first = generate_session_token().unwrap();
        let second = generate_session_token().unwrap();
        assert!(is_valid_token(&first));
        assert!(is_valid_token(&second));
        assert_ne!(first, second);
        assert!(!is_valid_token("short"));
        assert!(!is_valid_token("0123456789ABCDEF0123456789ABCDEF"));
        assert!(!is_valid_token("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn init_data_roundtrips_and_rejects_malformed_inputs() {
        let token = generate_session_token().unwrap();
        let encoded = format_init_data(1234, 5678, 3, false, &token).unwrap();
        let parsed = parse_init_data(&encoded).unwrap();
        assert_eq!(
            parsed,
            InitData {
                host: 1234,
                shell: 5678,
                generation: 3,
                inspect: false,
                token: token.clone(),
            }
        );

        let encoded_inspect = format_init_data(1234, 5678, 3, true, &token).unwrap();
        let parsed_inspect = parse_init_data(&format!("{encoded_inspect}\0")).unwrap();
        assert!(parsed_inspect.inspect);
        assert_eq!(parsed_inspect.token, token);

        for invalid in [
            "",
            "0:5678:1:0123456789abcdef0123456789abcdef",
            "1234:0:1:0123456789abcdef0123456789abcdef",
            "1234:5678:0:0123456789abcdef0123456789abcdef",
            "1234:5678:1:not-a-hex-token",
            "1234:5678:1:0123456789abcdef0123456789abcdef;unknown",
            "1234:5678:1:0123456789abcdef0123456789abcdef:extra",
        ] {
            assert!(
                parse_init_data(invalid).is_err(),
                "accepted invalid init data: {invalid}"
            );
        }
    }

    #[test]
    fn atomic_status_channel_roundtrips_with_owner_dacl() {
        let token = generate_session_token().unwrap();
        let host_ch = StatusChannel::create(1111, 2222, 7, true, &token).unwrap();
        assert!(StatusChannel::create(1111, 2222, 7, true, &token).is_err());

        let bridge_ch = StatusChannel::open(&token, 2222).unwrap();
        assert_eq!(bridge_ch.initialization().unwrap(), (1111, 2222, 7, true));
        assert!(StatusChannel::open(&token, 9999).is_err());

        bridge_ch.stage(10);
        bridge_ch.data().callbacks.fetch_add(4, Ordering::AcqRel);
        bridge_ch.data().frames.fetch_add(1, Ordering::AcqRel);
        bridge_ch.data().fills.fetch_add(1, Ordering::AcqRel);
        assert!(host_ch
            .describe()
            .contains("Elements found (read-only diagnostic); callbacks=4, frames=1, fill=1"));

        bridge_ch.fail(E_ACCESSDENIED);
        assert_eq!(
            host_ch.data().error.load(Ordering::Acquire),
            E_ACCESSDENIED.0
        );
    }
}
