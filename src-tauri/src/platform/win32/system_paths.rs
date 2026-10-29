//! Paths supplied by Windows, rather than inherited environment variables or PATH.

use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

pub(super) fn system_directory() -> Result<PathBuf, String> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: the API receives a writable buffer and its actual length.
        let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 {
            return Err(format!("GetSystemDirectoryW: {}", std::io::Error::last_os_error()));
        }
        if length < buffer.len() {
            let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
            return path.is_absolute().then_some(path).ok_or_else(|| "Windows returned a relative system directory".into());
        }
        if length > 32_768 {
            return Err("Windows system directory is too long".into());
        }
        buffer.resize(length + 1, 0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_system_directory_is_absolute_and_contains_windows_installer() {
        let path = super::system_directory().unwrap();
        assert!(path.is_absolute());
        assert!(path.join("msiexec.exe").is_file());
    }
}
