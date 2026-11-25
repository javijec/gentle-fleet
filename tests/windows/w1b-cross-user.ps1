param(
    [Parameter(Mandatory)][ValidateSet('Owner','Probe')][string]$Mode,
    [string]$Address,
    [string]$Runtime,
    [string]$OwnerSid
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
function Assert-AccessDenied([string]$Label, [scriptblock]$Attempt) {
    try { & $Attempt } catch {
        $errorObject = $_.Exception
        while ($null -ne $errorObject) {
            if ($errorObject -is [UnauthorizedAccessException] -or
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
    Assert-AccessDenied 'named pipe connection' { Connect-Fixture $Address }
    Assert-AccessDenied 'runtime enumeration' { [void][IO.Directory]::GetFileSystemEntries($Runtime) }
    Assert-AccessDenied 'credential read' { [void][IO.File]::ReadAllText((Join-Path $Runtime 'fleetd.token')) }
    Assert-AccessDenied 'endpoint DELETE_CHILD' { [IO.File]::Delete((Join-Path $Runtime 'fleetd.addr')) }
    Write-Output "PASS: distinct SID probe ($sid)"
    exit 0
}
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-cross-user-" + [guid]::NewGuid().ToString('N'))
$Runtime = Join-Path $root 'runtime'
$daemon = $null
$oldRuntime = $env:FLEET_RUNTIME_DIR
$oldDb = $env:FLEET_DB_PATH
New-Item -ItemType Directory -Path $root | Out-Null
try {
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
    Connect-Fixture $Address
    Write-Output 'PASS: owner positive control (pipe and files)'
    Write-Output "OwnerSid: $sid"
    Write-Output "Address: $Address"
    Write-Output "Runtime: $Runtime"
    Write-Output 'Run Probe in another authorized non-elevated Windows session while this process waits.'
    [void](Read-Host 'Press Enter only after the probe completes')
    if ($daemon.HasExited -or [IO.File]::ReadAllText($addr).Trim() -ne $Address -or
        [IO.File]::ReadAllText($token) -ne $secret) {
        throw 'Fixture was interrupted or its endpoint files changed'
    }
    Connect-Fixture $Address
    Write-Output 'PASS: owner post-probe positive control; combine with separate Probe output'
} finally {
    if ($null -ne $daemon) {
        if (-not $daemon.HasExited) { $daemon.Kill(); [void]$daemon.WaitForExit(5000) }
        $daemon.Dispose()
    }
    $env:FLEET_RUNTIME_DIR = $oldRuntime
    $env:FLEET_DB_PATH = $oldDb
    Remove-Item -LiteralPath $root -Recurse -Force
}
