# W1-F: native process acceptance; only isolated FakeDriver sessions.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Push-Location $repo
try {
    & cargo build -p fleetd --example w1f_acceptance --locked
    if ($LASTEXITCODE -ne 0) { throw 'W1-F probe build failed' }
    & (Join-Path $PSScriptRoot 'w1d-lifecycle.ps1') -Scenario Acceptance
} finally {
    Pop-Location
}
