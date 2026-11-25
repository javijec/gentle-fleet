# W1-C red-first fixture: durable workspace and private database directory.
# Run only against an isolated native Windows build. No real provider is used.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet w1c " + [guid]::NewGuid().ToString('N'))
$runtime = Join-Path $root 'runtime'
$data = Join-Path $root 'private data'
$db = Join-Path $data 'fleet.sqlite'
$workspace = Join-Path $root 'workspace with spaces'
$daemon = $null
$competitor = $null
$previousRuntime = [Environment]::GetEnvironmentVariable('FLEET_RUNTIME_DIR', 'Process')
$previousDb = [Environment]::GetEnvironmentVariable('FLEET_DB_PATH', 'Process')

function Invoke-FixtureCli([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $bin 'fleetctl.exe'
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            throw 'fixture CLI timed out'
        }
        if ($process.ExitCode -ne 0) {
            throw "fixture CLI failed: $($stderr.GetAwaiter().GetResult())"
        }
        return $stdout.GetAwaiter().GetResult().Trim()
    } finally {
        if (-not $process.HasExited) {
            $process.Kill()
            [void]$process.WaitForExit(5000)
        }
        $process.Dispose()
    }
}

function Start-FixtureDaemon([string]$Label) {
    Start-Process (Join-Path $bin 'fleetd.exe') -PassThru `
        -RedirectStandardOutput (Join-Path $root "$Label.out") `
        -RedirectStandardError (Join-Path $root "$Label.err")
}

function Wait-Ready([Diagnostics.Process]$Process) {
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not $Process.HasExited -and $deadline.Elapsed.TotalSeconds -lt 30) {
        try {
            $null = Invoke-FixtureCli @('workspace', 'list')
            if (-not $Process.HasExited) { return }
        } catch {
            Start-Sleep -Milliseconds 100
        }
    }
    throw 'daemon did not become ready'
}

function Assert-PrivateData([string]$Path, [bool]$Protected) {
    $item = Get-Item -LiteralPath $Path
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'data path is a reparse point'
    }
    $acl = Get-Acl -LiteralPath $Path
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid) {
        throw "data owner differs from current SID: $($item.Name)"
    }
    if ($Protected -and -not $acl.AreAccessRulesProtected) {
        throw "data directory DACL is not protected: $($item.Name)"
    }
    $ownerAllow = $false
    foreach ($rule in $acl.Access) {
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        $ruleSid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
        if ($ruleSid -ne $sid) { throw "non-owner data access: $($item.Name)" }
        if (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq
            [Security.AccessControl.FileSystemRights]::FullControl) {
            $ownerAllow = $true
        }
    }
    if (-not $ownerAllow) { throw "owner full control missing: $($item.Name)" }
}

New-Item -ItemType Directory -Path $root | Out-Null
try {
    New-Item -ItemType Directory -Path $workspace | Out-Null
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = $db
    $daemon = Start-FixtureDaemon 'first'
    Wait-Ready $daemon
    $registered = Invoke-FixtureCli @('workspace', 'add', $workspace)
    $id = ($registered -split "`t")[0]
    if ([string]::IsNullOrWhiteSpace($id)) { throw 'workspace ID missing' }
    $before = @( (Invoke-FixtureCli @('workspace', 'list')) -split "`r?`n" |
        Where-Object { ($_ -split "`t")[0] -eq $id } )
    if ($before.Count -ne 1) { throw 'registered workspace missing or duplicated' }

    # Distinct runtime and a case alias of the same durable root must not
    # let a second daemon reach SQLite or recovery.
    $env:FLEET_RUNTIME_DIR = Join-Path $root 'competitor-runtime'
    $env:FLEET_DB_PATH = Join-Path $root 'PRIVATE DATA/other.sqlite'
    $competitor = Start-FixtureDaemon 'competitor'
    try {
        if (-not $competitor.WaitForExit(10000)) {
            throw 'second daemon did not reject the occupied durable root'
        }
        if ($competitor.ExitCode -eq 0 -or
            (Test-Path -LiteralPath $env:FLEET_DB_PATH) -or
            (Test-Path -LiteralPath (Join-Path $env:FLEET_RUNTIME_DIR 'fleetd.addr'))) {
            throw 'second daemon opened another database or published an endpoint in the same root'
        }
        if ((Get-Content -LiteralPath (Join-Path $root 'competitor.err') -Raw) -notmatch 'durable storage lease unavailable') {
            throw 'second daemon did not fail at the expected durable lease'
        }
    } finally {
        if (-not $competitor.HasExited) { $competitor.Kill(); [void]$competitor.WaitForExit(5000) }
        $competitor.Dispose()
        $competitor = $null
        $env:FLEET_RUNTIME_DIR = $runtime
        $env:FLEET_DB_PATH = $db
    }
    if ($daemon.HasExited) { throw 'original daemon stopped during lease contention' }
    $stillThere = Invoke-FixtureCli @('workspace', 'list')
    if ($stillThere -notmatch [regex]::Escape($id)) { throw 'original workspace lost during lease contention' }
    Assert-PrivateData (Join-Path $data 'fleetd.lease') $false
    Write-Output 'PASS: distinct runtimes cannot share an occupied durable data root'

    $daemon.Kill()
    if (-not $daemon.WaitForExit(10000)) { throw 'fixture daemon did not terminate' }
    $daemon.Dispose()
    $daemon = $null

    function Assert-IdentityRejected([string]$Label) {
        $candidate = Start-FixtureDaemon $Label
        try {
            if (-not $candidate.WaitForExit(10000)) { throw 'identity competitor did not exit' }
            if ($candidate.ExitCode -eq 0 -or
                (Get-Content -LiteralPath (Join-Path $root "$Label.err") -Raw) -notmatch 'bound durable database is missing|durable database identity mismatch') {
                throw 'identity competitor was not rejected for database identity'
            }
            if (Test-Path -LiteralPath (Join-Path $env:FLEET_RUNTIME_DIR 'fleetd.addr')) {
                throw 'identity competitor published an endpoint'
            }
        } finally {
            if (-not $candidate.HasExited) { $candidate.Kill(); [void]$candidate.WaitForExit(5000) }
            $candidate.Dispose()
        }
    }

    $env:FLEET_RUNTIME_DIR = Join-Path $root 'identity-runtime'
    $env:FLEET_DB_PATH = Join-Path $data 'other.sqlite'
    Assert-IdentityRejected 'different-db'
    if (Test-Path -LiteralPath $env:FLEET_DB_PATH) { throw 'identity rejection created an alternate DB' }

    # Even the same filename must not adopt a different physical file.
    # Move the original aside temporarily in this disposable fixture only.
    $backup = Join-Path $data 'fixture-original.sqlite'
    Move-Item -LiteralPath $db -Destination $backup
    try {
        Copy-Item -LiteralPath $backup -Destination $db
        $env:FLEET_DB_PATH = $db
        $hash = (Get-FileHash -LiteralPath $db).Hash
        Assert-IdentityRejected 'replacement-db'
        if ((Get-FileHash -LiteralPath $db).Hash -ne $hash) { throw 'replacement DB was modified' }
    } finally {
        if (Test-Path -LiteralPath $db) { Remove-Item -LiteralPath $db }
        Move-Item -LiteralPath $backup -Destination $db
    }
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = $db
    Write-Output 'PASS: durable root refuses alternate or replaced database after shutdown'
    $daemon = Start-FixtureDaemon 'restart'
    Wait-Ready $daemon
    $after = @( (Invoke-FixtureCli @('workspace', 'list')) -split "`r?`n" |
        Where-Object { ($_ -split "`t")[0] -eq $id } )
    if ($after.Count -ne 1 -or $before[0] -ne $after[0]) {
        throw 'committed workspace did not survive termination unchanged'
    }
    Write-Output 'PASS: committed workspace survives native daemon termination'

    Assert-PrivateData $data $true
    Assert-PrivateData $db $false
    Write-Output 'PASS: data directory and database owner-only ACL'
} finally {
    if ($null -ne $daemon) {
        if (-not $daemon.HasExited) {
            $daemon.Kill()
            [void]$daemon.WaitForExit(5000)
        }
        $daemon.Dispose()
    }
    [Environment]::SetEnvironmentVariable('FLEET_RUNTIME_DIR', $previousRuntime, 'Process')
    [Environment]::SetEnvironmentVariable('FLEET_DB_PATH', $previousDb, 'Process')
    Remove-Item -LiteralPath $root -Recurse -Force
}
