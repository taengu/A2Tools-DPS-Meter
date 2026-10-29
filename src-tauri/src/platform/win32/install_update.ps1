$ErrorActionPreference = 'Stop'
$PSModuleAutoLoadingPreference = 'None'
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
$systemDirectory = [IO.Path]::GetDirectoryName($windowsInstaller)
$env:PATH = $systemDirectory
$env:COMSPEC = [IO.Path]::Combine($systemDirectory, 'cmd.exe')
$env:SystemRoot = [IO.Directory]::GetParent($systemDirectory).FullName
$env:windir = $env:SystemRoot
Microsoft.PowerShell.Core\Import-Module -Name ([IO.Path]::Combine($PSHOME, 'Modules', 'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Utility.psd1')) -ErrorAction Stop
$directory = $null
$copy = $null
$bootstrap = $null
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
try {
    if (([IO.File]::GetAttributes($temporaryRoot) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'The Windows temporary directory is a reparse point.'
    }
    $administrators = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $security.SetOwner($administrators)
    $inherit = [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
    foreach ($identity in @($administrators, $system)) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new($identity,
            [Security.AccessControl.FileSystemRights]::FullControl, $inherit,
            [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow)
        $security.AddAccessRule($rule)
    }
    # The framework supplies the initial ACL to CreateDirectoryW. Check the
    # resulting owner and rules before using it, including if a name existed.
    $candidate = [IO.Path]::Combine($temporaryRoot, 'a2tools-update-' + [Guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($candidate, $security)
    $actualSecurity = [IO.Directory]::GetAccessControl($candidate)
    $rules = $actualSecurity.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
    if (!$actualSecurity.AreAccessRulesProtected -or
        $actualSecurity.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $administrators.Value -or
        $rules.Count -ne 2) { throw 'The update helper directory is not protected.' }
    foreach ($rule in $rules) {
        if ($rule.IdentityReference.Value -notin @($administrators.Value, $system.Value) -or
            $rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
            ($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -ne [Security.AccessControl.FileSystemRights]::FullControl -or
            $rule.InheritanceFlags -ne $inherit) { throw 'The update helper directory has unexpected access rules.' }
    }
    $bootstrap = $candidate
    # Add-Type's compiler intermediates must be protected too. Never compile
    # elevated code in the inherited, user-writable temporary directory.
    $env:TEMP = $bootstrap
    $env:TMP = $bootstrap
    $compiler = [CodeDom.Compiler.CompilerParameters]::new()
    $compiler.GenerateInMemory = $true
    $compiler.TempFiles = [CodeDom.Compiler.TempFileCollection]::new($bootstrap, $false)
    [void]$compiler.ReferencedAssemblies.Add('System.dll')
    # CreateDirectoryW refuses an existing name and applies the protected ACL
    # in the same operation: no writable interval before an ACL change.
    Microsoft.PowerShell.Utility\Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class A2UpdateDirectory {
    [StructLayout(LayoutKind.Sequential)]
    private struct SecurityAttributes {
        public int Length;
        public IntPtr Descriptor;
        [MarshalAs(UnmanagedType.Bool)] public bool InheritHandle;
    }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreateDirectoryW(string path, ref SecurityAttributes attributes);
    public static void Create(string path, byte[] descriptor) {
        IntPtr memory = Marshal.AllocHGlobal(descriptor.Length);
        try {
            Marshal.Copy(descriptor, 0, memory, descriptor.Length);
            var attributes = new SecurityAttributes {
                Length = Marshal.SizeOf(typeof(SecurityAttributes)), Descriptor = memory, InheritHandle = false
            };
            if (!CreateDirectoryW(path, ref attributes)) throw new Win32Exception(Marshal.GetLastWin32Error());
        } finally { Marshal.FreeHGlobal(memory); }
    }
}
'@ -CompilerParameters $compiler
    $directory = [IO.Path]::Combine($bootstrap, 'package')
    [A2UpdateDirectory]::Create($directory, $security.GetSecurityDescriptorBinaryForm())
    $packageName = if ($npcap) { 'npcap.exe' } else { 'update.msi' }
    $copy = [IO.Path]::Combine($directory, $packageName)
    $inputFile = [IO.File]::Open($source, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($inputFile.Length -gt 268435456) { throw 'The update package is too large.' }
        $outputFile = [IO.File]::Open($copy, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
        try {
            $inputFile.CopyTo($outputFile)
            $outputFile.Flush($true)
            $outputFile.Position = 0
            $sha256 = [Security.Cryptography.SHA256]::Create()
            try {
                $actualHash = [BitConverter]::ToString($sha256.ComputeHash($outputFile)).Replace('-', '').ToLowerInvariant()
            } finally { $sha256.Dispose() }
            if ($actualHash -cne $expectedHash) { throw 'The update package does not match its signed checksum.' }
        } finally { $outputFile.Dispose() }
    } finally { $inputFile.Dispose() }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = if ($npcap) { $copy } else { $windowsInstaller }
    $start.UseShellExecute = $false
    $start.WorkingDirectory = $directory
    $start.Arguments = if ($npcap) { '' } else { '/i "' + $copy + '" /passive INSTALLDIR="' + $installDirectory + '" AUTOLAUNCHAPP=1' }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $process.WaitForExit()
        if ($process.ExitCode -notin @(0, 1641, 3010)) {
            throw ('Windows Installer returned ' + $process.ExitCode)
        }
    } finally { $process.Dispose() }
} catch {
    Microsoft.PowerShell.Utility\Add-Type -AssemblyName PresentationFramework
    [void][System.Windows.MessageBox]::Show($_.Exception.Message, 'A2Tools update failed')
    exit 1
} finally {
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
    # An installer may keep a file open or leave a subdirectory. Cleanup is
    # best effort and never changes the verified installation's result.
    if ($copy -and [IO.File]::Exists($copy)) { try { [IO.File]::Delete($copy) } catch {} }
    if ($directory -and [IO.Directory]::Exists($directory)) { try { [IO.Directory]::Delete($directory, $false) } catch {} }
    if ($bootstrap -and [IO.Directory]::Exists($bootstrap)) {
        foreach ($temporaryFile in [IO.Directory]::EnumerateFiles($bootstrap)) { try { [IO.File]::Delete($temporaryFile) } catch {} }
        try { [IO.Directory]::Delete($bootstrap, $false) } catch {}
    }
}
