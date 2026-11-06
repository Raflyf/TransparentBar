use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::E_INVALIDARG;

pub const BRIDGE_CLSID: GUID = GUID::from_u128(0x8a4b5ed1_1e59_4e76_a38f_1cae66acd0a1);
#[allow(dead_code)]
pub type InitializeXaml =
    unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, GUID, PCWSTR) -> HRESULT;

pub fn event_name(host: u32, shell: u32, generation: u32, kind: &str) -> Vec<u16> {
    format!(r"Local\TransparentBar.{host}.{shell}.{generation}.{kind}")
        .encode_utf16()
        .chain([0])
        .collect()
}

#[allow(dead_code)]
pub fn parse_init(text: &str) -> Result<(u32, u32, u32), HRESULT> {
    let mut parts = text.split(':');
    let mut values = [0; 3];
    for value in &mut values {
        *value = parts
            .next()
            .and_then(|part| part.parse().ok())
            .filter(|id| *id != 0)
            .ok_or(E_INVALIDARG)?;
    }
    if parts.next().is_some() {
        return Err(E_INVALIDARG);
    }
    Ok((values[0], values[1], values[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_bridge_initialization() {
        assert_eq!(parse_init("123:456:1"), Ok((123, 456, 1)));
        for text in [
            "",
            "0:2:1",
            "1:0:1",
            "1:2:0",
            "1:2",
            "1:2:3:4",
            "1:2:abc",
            "4294967296:2:1",
        ] {
            assert!(parse_init(text).is_err(), "{text}");
        }
        assert!(event_name(1, 2, 1, "Ready") != event_name(1, 2, 2, "Ready"));
    }
}
