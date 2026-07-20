//! Minimal registry reads.
//!
//! Only enough to fetch a couple of `REG_SZ` values under `HKEY_LOCAL_MACHINE`; pulling in a
//! registry crate for that would be more dependency than the job deserves.

use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::HSTRING;

/// Reads a string value, or `None` if the key, the value, or the permission is missing.
pub fn read_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);

    // Ask for the size first: model name strings have no fixed length.
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status.is_err() || size == 0 {
        return None;
    }

    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }

    // The returned size counts bytes and includes the terminating NUL.
    let chars = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..chars]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_value_that_exists_on_every_windows_install() {
        let name = read_string(
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        );
        let name = name.expect("every Windows machine has a processor name");
        assert!(!name.trim().is_empty());
        // No trailing NUL should survive into the string.
        assert!(
            !name.contains('\0'),
            "string was not trimmed to its real length: {name:?}"
        );
    }

    #[test]
    fn missing_keys_and_values_yield_none_rather_than_panicking() {
        assert_eq!(
            read_string(r"HARDWARE\DESCRIPTION\System", "NoSuchValue"),
            None
        );
        assert_eq!(read_string(r"No\Such\Key\At\All", "Whatever"), None);
    }
}
