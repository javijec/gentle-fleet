param(
    [Parameter(Mandatory)][ValidateSet('Owner','Probe')][string]$Mode,
    [string]$Address,
    [string]$Runtime,
    [string]$OwnerSid,
    [string]$ProbeSid,
    [string]$FixtureParent = [Environment]::GetFolderPath('CommonDocuments')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User.Value
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Use an ordinary non-elevated test account'
}
function Connect-Fixture([string]$Endpoint) {
    $prefix = '\\.\pipe\gentle-fleet-'
    if (-not $Endpoint.StartsWith($prefix, [StringComparison]::Ordinal)) {
        throw 'Not a Fleet fixture pipe'
    }
    $pipe = [IO.Pipes.NamedPipeClientStream]::new(
        '.', $Endpoint.Substring('\\.\pipe\'.Length), [IO.Pipes.PipeDirection]::InOut)
    try { $pipe.Connect(5000) } finally { $pipe.Dispose() }
}
# Open a handle with the exact requested right without changing data or ACLs.
# A missing object or a sharing violation is not accepted as access denial.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class FleetW1bAccess {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    public static void Open(string path, uint access) {
        using (var h = CreateFileW(path, access, 7, IntPtr.Zero, 3,
                0x02200000, IntPtr.Zero)) {
            if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
        }
    }
}
'@
function Set-FixtureAcl([string]$Path, [string]$OtherSid, [string]$OtherRights) {
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $owner = [Security.Principal.SecurityIdentifier]::new($sid)
    $other = [Security.Principal.SecurityIdentifier]::new($OtherSid)
    $acl.SetOwner($owner)
    $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        $owner, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        $other, $OtherRights, 'None', 'None', 'Allow'))
    Set-Acl -LiteralPath $Path -AclObject $acl
}
function Assert-AccessDenied([string]$Label, [scriptblock]$Attempt) {
    try { & $Attempt } catch {
        $errorObject = $_.Exception
        while ($null -ne $errorObject) {
            if ($errorObject -is [UnauthorizedAccessException] -or
                ($errorObject -is [ComponentModel.Win32Exception] -and $errorObject.NativeErrorCode -eq 5) -or
                ($errorObject.HResult -band 0xffff) -eq 5) {
                Write-Output "PASS: $Label denied by OS"
                return
            }
            $errorObject = $errorObject.InnerException
        }
        throw "$Label did not return access denied (timeout/missing path is not PASS)"
    }
    throw "$Label unexpectedly succeeded"
}
if ($Mode -eq 'Probe') {
    if (-not $OwnerSid -or $OwnerSid -eq $sid -or -not $Address -or -not $Runtime) {
        throw 'Probe requires a different owner SID, live fixture address and runtime path'
    }
    if ([IO.Path]::GetFileName($Runtime) -ne 'runtime' -or
        [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($Runtime)) -notmatch '^fleet-cross-user-[a-f0-9]{32}$') {
        throw 'Refusing to probe a path outside the disposable fixture naming convention'
    }
    $root = [IO.Path]::GetDirectoryName($Runtime)
    $control = Join-Path $root 'control'
    if ([IO.File]::ReadAllText((Join-Path $control 'marker')) -ne "$OwnerSid|$sid|$([IO.Path]::GetFileName($root))") {
        throw 'Fixture marker does not match the authorized pair of accounts'
    }
    # Both sibling paths share the same ancestors. Demonstrate traversal,
    # enumeration and explicit rights on the control before testing denial.
    [void][IO.Directory]::GetFileSystemEntries($root)
    [void][IO.Directory]::GetFileSystemEntries($control)
    foreach ($right in @(0x2, 0x40, 0x10000, 0x40000)) {
        [FleetW1bAccess]::Open($control, $right)
    }
    Write-Output 'PASS: positive traversal and control directory rights'
    Assert-AccessDenied 'named pipe connection' { Connect-Fixture $Address }
    Assert-AccessDenied 'runtime enumeration' { [void][IO.Directory]::GetFileSystemEntries($Runtime) }
    foreach ($name in @('fleetd.addr', 'fleetd.token')) {
        $file = Join-Path $Runtime $name
        Assert-AccessDenied "$name read" { [void][IO.File]::ReadAllText($file) }
        Assert-AccessDenied "$name write" { [FleetW1bAccess]::Open($file, 0x2) }
        Assert-AccessDenied "$name delete/replace" { [FleetW1bAccess]::Open($file, 0x10000) }
        Assert-AccessDenied "$name WRITE_DAC" { [FleetW1bAccess]::Open($file, 0x40000) }
    }
    Assert-AccessDenied 'runtime FILE_ADD_FILE' { [FleetW1bAccess]::Open($Runtime, 0x2) }
    Assert-AccessDenied 'runtime DELETE_CHILD' { [FleetW1bAccess]::Open($Runtime, 0x40) }
    Assert-AccessDenied 'runtime DELETE/rename' { [FleetW1bAccess]::Open($Runtime, 0x10000) }
    Assert-AccessDenied 'runtime WRITE_DAC' { [FleetW1bAccess]::Open($Runtime, 0x40000) }
    Write-Output "PASS: distinct SID probe ($sid)"
    exit 0
}
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $ProbeSid -or $ProbeSid -eq $sid) {
    throw 'Owner requires the SID of a different authorized ordinary test account'
}
[void][Security.Principal.SecurityIdentifier]::new($ProbeSid)
$root = Join-Path $FixtureParent ("fleet-cross-user-" + [guid]::NewGuid().ToString('N'))
$Runtime = Join-Path $root 'runtime'
$daemon = $null
$oldRuntime = $env:FLEET_RUNTIME_DIR
$oldDb = $env:FLEET_DB_PATH
New-Item -ItemType Directory -Path $root | Out-Null
try {
    # Only the isolated fixture root/control receive rights for the probe.
    # Fleet itself must create the protected runtime with its production DACL.
    Set-FixtureAcl $root $ProbeSid 'ReadAndExecute'
    $control = Join-Path $root 'control'
    New-Item -ItemType Directory -Path $control | Out-Null
    Set-FixtureAcl $control $ProbeSid 'FullControl'
    $marker = Join-Path $control 'marker'
    [IO.File]::WriteAllText($marker, "$sid|$ProbeSid|$([IO.Path]::GetFileName($root))")
    # Explicit marker access; control directory ACE does not inherit.
    $markerAcl = Get-Acl -LiteralPath $marker
    $markerAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        [Security.Principal.SecurityIdentifier]::new($ProbeSid), 'Read', 'Allow'))
    Set-Acl -LiteralPath $marker -AclObject $markerAcl
    $env:FLEET_RUNTIME_DIR = $Runtime
    $env:FLEET_DB_PATH = Join-Path $root 'data/fleet.sqlite'
    $daemon = Start-Process (Join-Path $repo 'target/debug/fleetd.exe') -PassThru
    $addr = Join-Path $Runtime 'fleetd.addr'
    $token = Join-Path $Runtime 'fleetd.token'
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $addr)) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'Fixture daemon did not publish its endpoint'
        }
        Start-Sleep -Milliseconds 100
    }
    $Address = [IO.File]::ReadAllText($addr).Trim()
    $secret = [IO.File]::ReadAllText($token)
    function Assert-OwnerHello {
        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = Join-Path $repo 'target/debug/fleet-ssh-bridge.exe'
        $start.UseShellExecute = $false
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $bridge = [Diagnostics.Process]::Start($start)
        try {
            $bridge.StandardInput.WriteLine('{"kind":"request","request_id":"cross-user","request":{"type":"hello","client_id":"w1b-cross-user","token":null}}')
            $bridge.StandardInput.Close()
            $out = $bridge.StandardOutput.ReadToEndAsync()
            $err = $bridge.StandardError.ReadToEndAsync()
            if (-not $bridge.WaitForExit(10000)) { throw 'Owner Hello timed out' }
            if ($bridge.ExitCode -ne 0) { throw 'Owner Hello bridge failed' }
            $frames = @($out.GetAwaiter().GetResult() -split '\r?\n' |
                Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object { $_ | ConvertFrom-Json })
            if (@($frames | Where-Object { $_.request_id -eq 'cross-user' -and $_.result.Ok.type -eq 'hello' }).Count -ne 1) {
                throw 'Owner Hello was not authenticated'
            }
        } finally {
            if (-not $bridge.HasExited) { $bridge.Kill(); [void]$bridge.WaitForExit(5000) }
            $bridge.Dispose()
        }
    }
    Assert-OwnerHello
    Write-Output 'PASS: owner authenticated positive control (pipe and files)'
    Write-Output "OwnerSid: $sid"
    Write-Output "Address: $Address"
    Write-Output "Runtime: $Runtime"
    Write-Output 'Run Probe in another authorized non-elevated Windows session while this process waits.'
    [void](Read-Host 'Press Enter only after the probe completes')
    if ($daemon.HasExited -or [IO.File]::ReadAllText($addr).Trim() -ne $Address -or
        [IO.File]::ReadAllText($token) -ne $secret) {
        throw 'Fixture was interrupted or its endpoint files changed'
    }
    Assert-OwnerHello
    Write-Output 'PASS: owner authenticated post-probe control; combine with separate Probe output'
} finally {
    if ($null -ne $daemon) {
        if (-not $daemon.HasExited) { $daemon.Kill(); [void]$daemon.WaitForExit(5000) }
        $daemon.Dispose()
    }
    $env:FLEET_RUNTIME_DIR = $oldRuntime
    $env:FLEET_DB_PATH = $oldDb
    Remove-Item -LiteralPath $root -Recurse -Force
}
