#[cfg(all(target_env = "msvc", not(test)))]
use windows::{
    core::{Error, Result, PCWSTR},
    Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{DestroyIcon, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR},
    },
};

#[cfg(any(test, not(target_env = "msvc")))]
use windows::{
    core::{Error, Result},
    Win32::{Foundation::E_INVALIDARG, UI::WindowsAndMessaging::*},
};

#[cfg(any(test, not(target_env = "msvc")))]
const ICON: &[u8] = include_bytes!("../assets/transparentbar.ico");

#[cfg(any(test, not(target_env = "msvc")))]
fn icon_image() -> Result<&'static [u8]> {
    if ICON.len() < 6 || ICON[..4] != [0, 0, 1, 0] {
        return Err(Error::from(E_INVALIDARG));
    }
    let count = u16::from_le_bytes([ICON[4], ICON[5]]) as usize;
    let mut selected = None;
    for index in 0..count {
        let start = 6 + 16 * index;
        let entry = ICON
            .get(start..start + 16)
            .ok_or_else(|| Error::from(E_INVALIDARG))?;
        let width = if entry[0] == 0 { 256 } else { entry[0] as i32 };
        let length = u32::from_le_bytes(
            entry[8..12]
                .try_into()
                .map_err(|_| Error::from(E_INVALIDARG))?,
        ) as usize;
        let offset = u32::from_le_bytes(
            entry[12..16]
                .try_into()
                .map_err(|_| Error::from(E_INVALIDARG))?,
        ) as usize;
        let score = (width - 32).abs();
        if selected.as_ref().is_none_or(|(best, _, _)| score < *best) {
            selected = Some((score, offset, length));
        }
    }
    let (_, offset, length) = selected.ok_or_else(|| Error::from(E_INVALIDARG))?;
    ICON.get(
        offset
            ..offset
                .checked_add(length)
                .ok_or_else(|| Error::from(E_INVALIDARG))?,
    )
    .ok_or_else(|| Error::from(E_INVALIDARG))
}

pub struct AppIcon(pub HICON);
impl AppIcon {
    #[cfg(all(target_env = "msvc", not(test)))]
    pub fn load() -> Result<Self> {
        // Load directly from the embedded PE resource (1 ICON in assets/app.rc) to avoid duplicating
        // the 89 KB icon payload inside .rdata.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let handle = LoadImageW(
                instance,
                PCWSTR(std::ptr::without_provenance(1)),
                IMAGE_ICON,
                32,
                32,
                LR_DEFAULTCOLOR,
            )?;
            if handle.is_invalid() {
                return Err(Error::from_win32());
            }
            Ok(Self(HICON(handle.0)))
        }
    }

    #[cfg(any(test, not(target_env = "msvc")))]
    pub fn load() -> Result<Self> {
        let image = icon_image()?;
        unsafe {
            CreateIconFromResourceEx(image, true, 0x00030000, 32, 32, LR_DEFAULTCOLOR).map(Self)
        }
    }
}
impl Drop for AppIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_payload_is_valid_and_contains_multiple_sizes() {
        assert_eq!(u16::from_le_bytes([ICON[4], ICON[5]]), 8);
        assert!(icon_image().unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
