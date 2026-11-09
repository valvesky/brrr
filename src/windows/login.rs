//! Login registration needs no elevation: it belongs to the current user.

use super::wide;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn enabled() -> bool {
    let mut size = 0;
    // SAFETY: the key/value strings and size output are valid; null queries size.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide("Omapower").as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
            && size > 2
    }
}

pub fn set(enabled: bool) -> Result<(), String> {
    let mut key = null_mut();
    // SAFETY: the key output is writable and null security uses the user's ACL.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "open login registration: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ));
    }
    let result = if enabled {
        std::env::current_exe()
            .map_err(|error| error.to_string())
            .and_then(|path| {
                let command = quoted_path(path.as_os_str().encode_wide());
                // SAFETY: command is a terminated UTF-16 REG_SZ; size is in bytes.
                let status = unsafe {
                    RegSetValueExW(
                        key,
                        wide("Omapower").as_ptr(),
                        0,
                        REG_SZ,
                        command.as_ptr().cast(),
                        (command.len() * 2) as u32,
                    )
                };
                registry_result(status)
            })
    } else {
        // SAFETY: key is open for writing and the value name is terminated.
        let status = unsafe { RegDeleteValueW(key, wide("Omapower").as_ptr()) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            registry_result(status)
        }
    };
    // SAFETY: the key was successfully opened above and is closed exactly once.
    unsafe { RegCloseKey(key) };
    result
}

fn registry_result(status: u32) -> Result<(), String> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "update login registration: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ))
    }
}

fn quoted_path(path: impl IntoIterator<Item = u16>) -> Vec<u16> {
    std::iter::once(b'"' as u16)
        .chain(path)
        .chain([b'"' as u16, 0])
        .collect()
}

pub fn change(enabled: bool) -> i32 {
    match set(enabled) {
        Ok(()) => {
            eprintln!(
                "omapower: {} start at login",
                if enabled { "enabled" } else { "disabled" }
            );
            0
        }
        Err(error) => {
            eprintln!("omapower: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_executable_path() {
        assert_eq!(
            String::from_utf16(&quoted_path(
                r"C:\Program Files\Omapower\omapower.exe".encode_utf16()
            ))
            .unwrap(),
            "\"C:\\Program Files\\Omapower\\omapower.exe\"\0"
        );
    }
}
