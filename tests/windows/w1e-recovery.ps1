# W1-E native recovery. All processes and databases belong to the D fixture.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Push-Location $repo
try {
    & cargo build -p fleet-runner --example w1e_spool_probe --locked
    if ($LASTEXITCODE -ne 0) { throw 'W1-E spool probe build failed' }
    foreach ($scenario in @('Recovery', 'RunnerLoss', 'ApprovalRecovery')) {
        Write-Output "W1-E scenario: $scenario"
        & (Join-Path $PSScriptRoot 'w1d-lifecycle.ps1') -Scenario $scenario
    }
} finally {
    Pop-Location
}
