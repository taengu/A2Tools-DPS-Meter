//! The elevated update helper verifies the bytes Windows Installer will read.

use std::path::Path;

/// Updates install themselves here (the MSI).
pub fn supported() -> bool {
    true
}

/// The manifest's package for this platform.
pub fn package_url<'a>(packages: &crate::platform::UpdatePackages<'a>) -> &'a str {
    packages.msi
}

fn quoted(value: &str) -> Result<String, String> {
    if value.chars().any(|c| c.is_control() || c == '"') {
        return Err("invalid installer path".into());
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn helper_script(package: &Path, sha256: &str, install_dir: &str, system: &Path) -> Result<String, String> {
    installer_script(package, sha256, install_dir, system, false)
}

fn installer_script(package: &Path, sha256: &str, install_dir: &str, system: &Path, npcap: bool) -> Result<String, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("no checksum for the update package".into());
    }
    if !package.is_absolute() || !Path::new(install_dir).is_absolute() || !system.is_absolute() {
        return Err("installer paths must be absolute".into());
    }
    let text = |path: &Path| path.to_str().ok_or_else(|| "installer path is not Unicode".to_string()).and_then(quoted);
    let root = system.parent().ok_or("no Windows directory")?.join("Temp");
    Ok(format!(
        "$source = {}\n$expectedHash = '{}'\n$installDirectory = {}\n$temporaryRoot = {}\n$windowsInstaller = {}\n$npcap = ${}\n{}",
        text(package)?, sha256.to_ascii_lowercase(), quoted(install_dir)?, text(&root)?,
        text(&system.join("msiexec.exe"))?, npcap, include_str!("install_update.ps1")
    ))
}

/// PowerShell's EncodedCommand is base64 of UTF-16LE, with no shell syntax in
/// the command-line parameter itself.
fn encoded_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        result.push(ALPHABET[a >> 2] as char);
        result.push(ALPHABET[((a & 3) << 4) | (b >> 4)] as char);
        result.push(if chunk.len() > 1 { ALPHABET[((b & 15) << 2) | (c >> 6)] as char } else { '=' });
        result.push(if chunk.len() > 2 { ALPHABET[c & 63] as char } else { '=' });
    }
    result
}

/// Start a separate elevated helper before the meter exits. It copies the
/// download into a directory protected from unelevated writers, verifies the
/// stored copy against the signed manifest, and keeps that copy protected until
/// Windows Installer finishes. A changed download fails its checksum there.
pub fn run_installer(package: &Path, sha256: &str, install_dir: &str) -> Result<(), String> {
    let system = super::system_paths::system_directory()?;
    let process = launch_helper(&helper_script(package, sha256, install_dir, &system)?, &system)?;
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(process) };
    Ok(())
}

/// The Npcap publisher was verified while the caller held the original file.
/// The elevated helper hashes its protected copy, so it remains safe even if
/// the meter exits before Npcap's installer finishes reading its own image.
pub(super) fn run_npcap_installer(package: &Path, sha256: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    let system = super::system_paths::system_directory()?;
    let script = installer_script(package, sha256, system.to_str().ok_or("Windows path is not Unicode")?, &system, true)?;
    let process = launch_helper(&script, &system)?;
    let waited = unsafe { WaitForSingleObject(process, INFINITE) };
    let mut exit_code = 1;
    let exit_read = unsafe { GetExitCodeProcess(process, &mut exit_code) };
    let _ = unsafe { CloseHandle(process) };
    if waited != WAIT_OBJECT_0 { return Err("could not wait for Npcap's installer".into()); }
    exit_read.map_err(|e| e.to_string())?;
    if exit_code != 0 { return Err(format!("Npcap installation failed ({exit_code})")); }
    Ok(())
}

fn launch_helper(script: &str, system: &Path) -> Result<windows::Win32::Foundation::HANDLE, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let program = system.join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let file: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
    let working_directory: Vec<u16> = program.parent().ok_or("no PowerShell directory")?.as_os_str().encode_wide().chain(Some(0)).collect();
    let parameters = format!("-NoProfile -NonInteractive -EncodedCommand {}", encoded_command(script));
    if parameters.len() > 30_000 {
        return Err("installer paths exceed the Windows command-line limit".into());
    }
    let arguments: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(arguments.as_ptr()),
        lpDirectory: PCWSTR(working_directory.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: both strings remain alive for the synchronous ShellExecute call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| format!("could not start the update: {e}"))?;
    if info.hProcess.is_invalid() { return Err("Windows did not provide an installer helper process handle".into()); }
    Ok(info.hProcess)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_powershell(script: &str) -> std::process::Output {
        use std::io::Write;
        use std::process::Stdio;
        let system = super::super::system_paths::system_directory().unwrap();
        let mut child = std::process::Command::new(system.join("WindowsPowerShell/v1.0/powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-Command", "$s=[Console]::In.ReadToEnd(); & ([scriptblock]::Create($s))"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn path_data_cannot_become_powershell_syntax() {
        let script = helper_script(Path::new(r"C:\Temp\Bob's $(throw 1).msi"), &"a".repeat(64),
            r"C:\Program Files\A2Tools", Path::new(r"C:\Windows\System32")).unwrap();
        assert!(script.starts_with("$source = 'C:\\Temp\\Bob''s $(throw 1).msi'\n"));
        assert!(helper_script(Path::new("relative.msi"), &"a".repeat(64), r"C:\A2Tools", Path::new(r"C:\Windows\System32")).is_err());
        assert!(helper_script(Path::new(r"C:\x.msi"), "bad", r"C:\A2Tools", Path::new(r"C:\Windows\System32")).is_err());
        assert!(quoted("C:\\bad\npath").is_err());
        assert!(quoted("C:\\bad\"path").is_err());
    }

    #[test]
    fn encoded_command_is_utf16le() {
        assert_eq!(encoded_command("abc"), "YQBiAGMA");
        assert_eq!(encoded_command("a"), "YQA=");
        assert_eq!(encoded_command("é"), "6QA=");
    }

    #[test]
    fn windows_accepts_the_generated_helper_as_syntax() {
        let system = super::super::system_paths::system_directory().unwrap();
        let script = helper_script(Path::new(r"C:\Temp\Bob's $(throw 1).msi"), &"a".repeat(64),
            r"C:\Program Files\A2Tools", &system).unwrap();
        // Parse only: never run the helper, prompt for elevation or install.
        use std::io::Write;
        use std::process::Stdio;
        let parse = "$tokens=$null; $errors=$null; $s=[Console]::In.ReadToEnd(); [void][System.Management.Automation.Language.Parser]::ParseInput($s,[ref]$tokens,[ref]$errors); if ($errors.Count) { $errors | Out-String | Write-Error; exit 1 }";
        let mut child = std::process::Command::new(system.join("WindowsPowerShell/v1.0/powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-Command", parse])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }

    #[test]
    fn the_helper_verifies_its_stored_copy_and_creates_the_initial_acl_exclusively() {
        // Exercise the real copy/verification stage without starting msiexec or
        // elevating. Only the test ACL's owner/allow ACE changes to the test user,
        // since assigning the production Administrators owner requires elevation.
        let stage = include_str!("install_update.ps1").split("    $start = [Diagnostics.ProcessStartInfo]::new()").next().unwrap()
            .replace("[Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')", "[Security.Principal.WindowsIdentity]::GetCurrent().User");
        for (mismatch, npcap) in [(false, false), (true, false), (false, true), (true, true)] {
            let setup = format!(r#"
$ErrorActionPreference = 'Stop'
$expectMismatch = ${mismatch}
$npcap = ${npcap}
$rejected = $false
$temporaryRoot = [IO.Path]::GetTempPath()
$windowsInstaller = [IO.Path]::Combine([Environment]::SystemDirectory, 'msiexec.exe')
$source = [IO.Path]::GetTempFileName()
[IO.File]::WriteAllText($source, 'synthetic verified package')
$hash = [Security.Cryptography.SHA256]::Create()
try {{ $expectedHash = [BitConverter]::ToString($hash.ComputeHash([IO.File]::ReadAllBytes($source))).Replace('-', '').ToLowerInvariant() }} finally {{ $hash.Dispose() }}
if ($expectMismatch) {{ $expectedHash = '0' * 64 }}
{stage}
    if ($expectMismatch) {{ throw 'mismatched bytes were accepted' }}
    $acl = [IO.Directory]::GetAccessControl($directory)
    if (!$acl.AreAccessRulesProtected) {{ throw 'the initial ACL inherited access rules' }}
    if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $administrators.Value) {{ throw 'wrong initial owner' }}
    $existingRefused = $false
    try {{ [A2UpdateDirectory]::Create($directory, $security.GetSecurityDescriptorBinaryForm()) }} catch [ComponentModel.Win32Exception] {{
        if ($_.Exception.NativeErrorCode -ne 183) {{ throw }}
        $existingRefused = $true
    }}
    if (!$existingRefused) {{ throw 'an existing directory name was accepted' }}
    [IO.File]::WriteAllText($source, 'changed after the protected copy was checked')
    if ([IO.File]::ReadAllText($copy) -cne 'synthetic verified package') {{ throw 'the stored copy changed with the source' }}
    $expectedExtension = if ($npcap) {{ '.exe' }} else {{ '.msi' }}
    if ([IO.Path]::GetExtension($copy) -cne $expectedExtension) {{ throw 'wrong installer copy type' }}
}} catch {{
    if ($expectMismatch -and $_.Exception.Message -eq 'The update package does not match its signed checksum.') {{ $rejected = $true }} else {{ throw }}
}} finally {{
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
    if ($copy -and [IO.File]::Exists($copy)) {{ [IO.File]::Delete($copy) }}
    if ($directory -and [IO.Directory]::Exists($directory)) {{ [IO.Directory]::Delete($directory, $false) }}
    if ($bootstrap -and [IO.Directory]::Exists($bootstrap)) {{
        foreach ($temporaryFile in [IO.Directory]::EnumerateFiles($bootstrap)) {{ [IO.File]::Delete($temporaryFile) }}
        [IO.Directory]::Delete($bootstrap, $false)
    }}
    [IO.File]::Delete($source)
}}
if ($expectMismatch -and !$rejected) {{ throw 'the mismatched copy was not refused' }}
"#);
            let result = run_powershell(&setup);
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        }
    }
}
