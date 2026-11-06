use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use windows::{
    core::{Error, Result, GUID, HRESULT, PCWSTR},
    Win32::{Foundation::*, System::Memory::*},
};

#[allow(dead_code)]
pub const BRIDGE_FILE: &str = "transparentbar_bridge-0.1.2.dll";
pub const BRIDGE_CLSID: GUID = GUID::from_u128(0xf4fd55c8_1b61_4a99_943f_03bafc54a188);
#[allow(dead_code)]
pub type InitializeXaml =
    unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, GUID, PCWSTR) -> HRESULT;

pub fn event_name(host: u32, shell: u32, generation: u32, kind: &str) -> Vec<u16> {
    format!(r"Local\TransparentBar.{host}.{shell}.{generation}.{kind}")
        .encode_utf16()
        .chain([0])
        .collect()
}
fn status_name(shell: u32) -> Vec<u16> {
    format!(r"Local\TransparentBar.0.1.2.Status.{shell}")
        .encode_utf16()
        .chain([0])
        .collect()
}

#[repr(C)]
pub struct StartupStatus {
    magic: u32,
    host: u32,
    shell: u32,
    generation: u32,
    inspect: u32,
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
// The mapped object contains immutable header fields and atomic counters only.
unsafe impl Send for StatusChannel {}
unsafe impl Sync for StatusChannel {}
impl StatusChannel {
    #[allow(dead_code)]
    pub fn create(host: u32, shell: u32, generation: u32, inspect: bool) -> Result<Self> {
        unsafe {
            let name = status_name(shell);
            let mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                std::mem::size_of::<StartupStatus>() as u32,
                PCWSTR(name.as_ptr()),
            )?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mapping);
                return Err(Error::new(E_FAIL, "Sesi bridge sebelumnya belum selesai. Tutup instance lama sebelum membuka ulang."));
            }
            let channel = Self::map(mapping)?;
            channel
                .view
                .Value
                .cast::<StartupStatus>()
                .write(StartupStatus {
                    magic: 0x54425231,
                    host,
                    shell,
                    generation,
                    inspect: u32::from(inspect),
                    stage: AtomicU32::new(0),
                    error: AtomicI32::new(0),
                    callbacks: AtomicU32::new(0),
                    frames: AtomicU32::new(0),
                    fills: AtomicU32::new(0),
                    borders: AtomicU32::new(0),
                });
            Ok(channel)
        }
    }
    #[allow(dead_code)]
    pub fn open(shell: u32) -> Result<Self> {
        let name = status_name(shell);
        let mapping =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr()))? };
        let channel = Self::map(mapping)?;
        if channel.data().magic != 0x54425231 || channel.data().shell != shell {
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
        if data.host == 0 || data.shell == 0 || data.generation == 0 || data.inspect > 1 {
            return Err(Error::from(E_INVALIDARG));
        }
        Ok((data.host, data.shell, data.generation, data.inspect != 0))
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
        let stage = match data.stage.load(Ordering::Acquire) {
            0 => "DLL/COM belum merespons",
            1 => "Factory COM",
            2 => "SetSite",
            3 => "Antarmuka XAML",
            4 => "Dispatcher UI",
            5 => "Kanal sesi",
            6 => "Registrasi callback",
            7 => "Callback diterima",
            8 => "Frame taskbar",
            9 => "BackgroundFill",
            10 if data.inspect != 0 => "Elemen ditemukan (diagnostic read-only)",
            10 => "Transparansi terkonfirmasi",
            11 => "Pemulihan",
            12 => "Pemulihan selesai",
            _ => "Tahap tidak dikenal",
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
        assert_ne!(event_name(1, 2, 1, "Ready"), event_name(1, 2, 2, "Ready"));
        assert_ne!(status_name(2), status_name(3));
        assert_eq!(std::mem::size_of::<StartupStatus>(), 44);
        assert_eq!(std::mem::offset_of!(StartupStatus, error), 24);
    }
}
