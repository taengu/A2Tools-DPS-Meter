//! Verifying a downloaded executable without releasing the file being checked.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Security::Cryptography::{
    CertGetNameStringW, CERT_CONTEXT, CERT_NAME_ATTR_TYPE, szOID_COMMON_NAME, szOID_ORGANIZATION_NAME,
};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
    WTHelperProvDataFromStateData, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA,
    WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_WHOLECHAIN,
    WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

/// Keeps both the file and its path from being changed until the installer
/// process has finished. Directory handles deny renames of its ancestors too.
pub(super) struct LockedFile {
    file: File,
    _directories: Vec<File>,
    path: PathBuf,
}

impl LockedFile {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("the installer needs an absolute path without parent components".into());
        }
        let parent = path.parent().ok_or("the installer has no parent directory")?;
        let mut directories = Vec::new();
        for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(directory)
                .map_err(|e| format!("cannot protect installer directory {}: {e}", directory.display()))?;
            let metadata = handle.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return Err("the installer path contains a redirected directory".into());
            }
            directories.push(handle);
        }
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .map_err(|e| format!("cannot protect installer file: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err("the installer must be an ordinary file".into());
        }
        Ok(Self { file, _directories: directories, path: path.to_owned() })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn sha256(&self) -> Result<String, String> {
        use std::io::{Read, Seek, SeekFrom};
        use sha2::{Digest, Sha256};
        let mut file = self.file.try_clone().map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let length = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if length == 0 { break; }
            hash.update(&buffer[..length]);
        }
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
}

struct TrustState(WINTRUST_DATA);

impl Drop for TrustState {
    fn drop(&mut self) {
        self.0.dwStateAction = WTD_STATEACTION_CLOSE;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: this is the state returned by WinVerifyTrust, closed once.
        unsafe { WinVerifyTrust(HWND::default(), &mut action, (&mut self.0 as *mut WINTRUST_DATA).cast()); }
    }
}

fn certificate_attribute(certificate: *const CERT_CONTEXT, oid: PCSTR) -> Result<String, String> {
    // SAFETY: the certificate is owned by the live WinVerifyTrust state; the
    // attribute OID is a static, NUL-terminated string from the Windows bindings.
    let length = unsafe { CertGetNameStringW(certificate, CERT_NAME_ATTR_TYPE, 0, Some(oid.0.cast()), None) };
    if !(2..=1024).contains(&length) {
        return Err("the installer signer has no usable publisher name".into());
    }
    let mut buffer = vec![0u16; length as usize];
    let copied = unsafe {
        CertGetNameStringW(certificate, CERT_NAME_ATTR_TYPE, 0, Some(oid.0.cast()), Some(&mut buffer))
    };
    if copied != length {
        return Err("could not read the installer publisher".into());
    }
    String::from_utf16(&buffer[..buffer.len() - 1]).map_err(|_| "the installer publisher is not UTF-16".into())
}

fn expected_publisher(common_name: &str, organization: &str) -> bool {
    // Npcap 1.90 from https://npcap.com/dist/npcap-1.90.exe is signed by
    // CN=Nmap Software LLC, O=Nmap Software LLC. Check identity, not a leaf
    // thumbprint, so normal certificate renewal does not break installations.
    common_name == "Nmap Software LLC" && organization == "Nmap Software LLC"
}

/// Verify the Authenticode signature, chain and revocation policy, then the
/// publisher on that same validated signer. The caller holds `file` through
/// ShellExecuteExW and until the installer exits.
pub(super) fn verify_npcap(file: &LockedFile) -> Result<(), String> {
    let mut wide: Vec<u16> = file.path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err("the installer path contains a NUL".into());
    }
    wide.push(0);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE(file.file.as_raw_handle()),
        ..Default::default()
    };
    let mut state = TrustState(WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    });
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: all structures and their referenced path/file remain alive for
    // the verification and state inspection. The held handle denies changes.
    let status = unsafe { WinVerifyTrust(HWND::default(), &mut action, (&mut state.0 as *mut WINTRUST_DATA).cast()) };
    if status != 0 {
        return Err(format!("Npcap's Authenticode signature is not trusted (0x{:08x})", status as u32));
    }
    let provider = unsafe { WTHelperProvDataFromStateData(state.0.hWVTStateData) };
    if provider.is_null() {
        return Err("the trusted installer has no signature provider state".into());
    }
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
    if signer.is_null() || unsafe { (*signer).dwError != 0 } {
        return Err("the trusted installer has no validated signer".into());
    }
    let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
    if certificate.is_null() || unsafe { (*certificate).pCert.is_null() } {
        return Err("the trusted installer has no publisher certificate".into());
    }
    let leaf = unsafe { (*certificate).pCert };
    let common_name = certificate_attribute(leaf, szOID_COMMON_NAME)?;
    let organization = certificate_attribute(leaf, szOID_ORGANIZATION_NAME)?;
    if !expected_publisher(&common_name, &organization) {
        return Err("the installer is not signed by Nmap Software LLC".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (PathBuf, PathBuf) {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).unwrap();
        let tag: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let directory = std::env::temp_dir().join(format!("a2tools-installer-test-{tag}"));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("installer.exe");
        std::fs::write(&path, b"MZ unsigned synthetic installer").unwrap();
        (directory, path)
    }

    #[test]
    fn a_held_installer_cannot_be_written_deleted_or_moved_with_its_directory() {
        let (directory, path) = sample();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(LockedFile::open(&path).is_err(), "an existing writer prevents verification");
        drop(writer);
        let file = LockedFile::open(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&path, directory.join("other.exe")).is_err());
        assert!(std::fs::rename(&directory, directory.with_extension("moved")).is_err());
        drop(file);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn an_unsigned_executable_is_refused_before_it_can_run() {
        let (directory, path) = sample();
        let file = LockedFile::open(&path).unwrap();
        assert!(verify_npcap(&file).is_err());
        drop(file);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn another_publisher_does_not_qualify() {
        assert!(expected_publisher("Nmap Software LLC", "Nmap Software LLC"));
        assert!(!expected_publisher("Nmap Software LLC", "Someone Else"));
        assert!(!expected_publisher("Microsoft Corporation", "Microsoft Corporation"));
        assert!(!expected_publisher("Nmap Software LLC.example", "Nmap Software LLC"));
    }
}
