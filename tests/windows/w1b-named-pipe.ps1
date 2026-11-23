$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-w1b-" + [guid]::NewGuid().ToString('N'))
$runtime = Join-Path $root 'runtime'
$daemon = $null
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = Join-Path $root 'data/fleet.sqlite'
    $daemon = Start-Process (Join-Path $bin 'fleetd.exe') -PassThru `
        -RedirectStandardOutput (Join-Path $root 'daemon.out') `
        -RedirectStandardError (Join-Path $root 'daemon.err')
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    $addressFile = Join-Path $runtime 'fleetd.addr'
    while (-not (Test-Path $addressFile)) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'daemon failed to publish endpoint within 30 seconds'
        }
        Start-Sleep -Milliseconds 100
    }
    $address = ([IO.File]::ReadAllText($addressFile)).Trim()
    if (-not $address.StartsWith('\\.\pipe\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Windows daemon endpoint is not a named pipe'
    }
    Write-Output 'PASS: daemon publishes a named pipe endpoint'
} finally {
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        $daemon.Kill()
        [void]$daemon.WaitForExit(5000)
    }
    if ($null -ne $daemon) { $daemon.Dispose() }
    Remove-Item -LiteralPath $root -Recurse -Force
}
