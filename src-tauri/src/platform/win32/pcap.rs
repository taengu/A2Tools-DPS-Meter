//! Which packet-capture library to load. The API is libpcap's on every OS;
//! on Windows it comes from Npcap.

use std::path::Path;

/// The library `capture::pcap_capturer` loads at runtime (the first of these
/// that loads).
pub const LIBRARIES: &[&str] = &["wpcap.dll"];

/// What to tell the player when it will not load.
pub const MISSING_HELP: &str = "Is Npcap installed? Download from https://npcap.com";

/// The meter can fetch and run Npcap's installer for the player.
pub const OFFERS_INSTALL: bool = true;

/// Whether a read returns at its timeout on a device with no traffic: Npcap's
/// does.
pub const READ_TIMES_OUT_WHEN_IDLE: bool = true;

/// Whether a filter can change while capturing without losing what the
/// kernel has queued. Not here: Npcap empties its buffer on a new filter
/// (BIOCSETF), so a lock that narrowed the filter would cut the game's
/// stream right where it was found. The filter is set once, when the device
/// opens, and kept (`pcap_capturer`).
pub const FILTER_CHANGE_KEEPS_QUEUE: bool = false;

/// Loading uses explicit paths and flags, without changing the process's DLL
/// search path.
pub fn prepare() {}

/// The System32 folder, from Windows.
fn system32() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_System, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: the string Windows allocates is read once, then freed.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_System, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(std::path::PathBuf::from)
    }
}

/// Load the capture library `name` by its full path: from `System32\Npcap`,
/// else System32 itself (Npcap's WinPcap-compatible mode), never by the
/// usual search, which goes on into PATH when Npcap isn't installed, and
/// PATH has folders the user can write (audit L1). Dependencies (Packet.dll)
/// may come only from that DLL's directory or System32.
///
/// # Safety
/// Loading a library runs its initialisation, as `libloading::Library::new`.
pub unsafe fn load(name: &str) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // Never fall back to an ambient search if Windows cannot name its folder.
    let system = system32().ok_or(libloading::Error::LoadLibraryExWUnknown)?;
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
    let npcap = system.join("Npcap").join(name);
    match unsafe { Library::load_with_flags(&npcap, flags) } {
        Ok(lib) => Ok(lib.into()),
        Err(_) => unsafe { Library::load_with_flags(system.join(name), flags) }.map(Into::into),
    }
}

/// Whether the capture library can be loaded at all.
pub fn library_available() -> bool {
    // SAFETY: loading Npcap runs no initialisation we depend on not running.
    LIBRARIES.iter().any(|name| unsafe { load(name).is_ok() })
}

/// Run Npcap's installer and wait until it closes. It needs administrator
/// rights, which the meter may not have, so Windows is asked to elevate it
/// (the UAC prompt names Npcap's publisher); that prompt being refused is an
/// error.
///
/// Only one Nmap Software signed, checked through a handle that keeps the
/// file from being changed until the installer has closed (#131).
pub fn run_installer(installer: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ alone: others may read it (the installer), not write
    // or delete it.
    let held = std::fs::OpenOptions::new().read(true).share_mode(0x1).open(installer).map_err(|e| e.to_string())?;
    let signer = authenticode_signer(installer, &held)?;
    if signer != NPCAP_SIGNER {
        return Err(format!("the installer is signed by {signer}, not {NPCAP_SIGNER}"));
    }
    let result = elevate(installer);
    drop(held);
    result
}

/// Who signs Npcap's installers.
const NPCAP_SIGNER: &str = "Nmap Software LLC";

/// The signer a file's Authenticode signature names, once Windows trusts the
/// signature; an error for an unsigned file or one it does not trust.
fn authenticode_signer(path: &Path, file: &std::fs::File) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
    use windows::Win32::Security::WinTrust::*;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE(file.as_raw_handle()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data`, `info` and the path outlive both calls; the state the
    // first opens is read in between and closed by the second.
    unsafe {
        let status = WinVerifyTrust(HWND(-1isize as _), &mut action, &mut data as *mut _ as *mut _);
        let signer = (status == 0)
            .then(|| WTHelperProvDataFromStateData(data.hWVTStateData))
            .filter(|p| !p.is_null())
            .map(|p| WTHelperGetProvSignerFromChain(p, 0, false, 0))
            .filter(|s| !s.is_null() && (**s).csCertChain > 0)
            .map(|s| {
                let cert = (*(*s).pasCertChain).pCert;
                let mut name = [0u16; 256];
                let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut name));
                String::from_utf16_lossy(&name[..(n as usize).saturating_sub(1)])
            });
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(HWND(-1isize as _), &mut action, &mut data as *mut _ as *mut _);
        match signer {
            Some(name) => Ok(name),
            None if status != 0 => Err(format!("the installer's signature is not trusted ({status:#x})")),
            None => Err("the installer names no signer".into()),
        }
    }
}

/// Run it elevated and wait for it to close.
fn elevate(installer: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file: Vec<u16> = installer.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the path it points to live across the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| e.to_string())?;
    if !info.hProcess.is_invalid() {
        // SAFETY: a process handle ShellExecuteExW opened for us; closed once.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

/// Devices not worth opening on this OS. None here.
pub fn skip_device(_name: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    /// An unsigned program names no signer; Npcap's installer, when one is
    /// given (A2_NPCAP_INSTALLER, from npcap.com/dist), names Nmap.
    #[test]
    fn only_a_trusted_signature_names_a_signer() {
        let me = std::env::current_exe().unwrap();
        assert!(super::authenticode_signer(&me, &std::fs::File::open(&me).unwrap()).is_err());
        if let Some(path) = std::env::var_os("A2_NPCAP_INSTALLER").map(std::path::PathBuf::from) {
            let signer = super::authenticode_signer(&path, &std::fs::File::open(&path).unwrap());
            assert_eq!(signer.as_deref(), Ok(super::NPCAP_SIGNER));
        }
    }
}
