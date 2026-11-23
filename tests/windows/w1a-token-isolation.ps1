$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-w1a-" + [guid]::NewGuid().ToString('N'))
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$runtime = Join-Path $root 'runtime'
$daemon = $null
$runners = [System.Collections.Generic.List[Diagnostics.Process]]::new()
New-Item -ItemType Directory -Path $root | Out-Null

function Invoke-Fleetctl([string[]]$Arguments) {
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
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "fleetctl timed out: $($Arguments[0])"
        }
        $out = $stdout.GetAwaiter().GetResult()
        $err = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "fleetctl $($Arguments[0]) failed: $err" }
        return $out.Trim()
    } finally {
        $process.Dispose()
    }
}

function Track-Runners {
    foreach ($child in @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($daemon.Id)" |
        Where-Object { $_.Name -eq 'fleet-runner.exe' })) {
        if (@($runners | Where-Object { $_.Id -eq $child.ProcessId }).Count -eq 0) {
            $process = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
            if ($null -ne $process) {
                # Keep a handle to the original process, not just a reusable PID.
                [void]$process.Handle
                $runners.Add($process)
            }
        }
    }
}

function Assert-DaemonRejectsToken([string]$Token) {
    $address = ([IO.File]::ReadAllText((Join-Path $runtime 'fleetd.addr'))).Trim()
    $endpoint = [Net.IPEndPoint]::Parse($address)
    $socket = [Net.Sockets.TcpClient]::new()
    try {
        $socket.Connect($endpoint)
        $socket.ReceiveTimeout = 5000
        $stream = $socket.GetStream()
        $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $frame = @{ kind = 'request'; request_id = 'invalid'; request = @{
                type = 'hello'; client_id = 'w1a'; token = $Token
            } } | ConvertTo-Json -Compress
            $writer.WriteLine($frame)
            $writer.Flush()
            $response = $reader.ReadLine() | ConvertFrom-Json
            if ($response.result.Err.code -ne 'Unauthorized') {
                throw 'daemon did not reject an invalid token'
            }
        } finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    } finally {
        $socket.Dispose()
    }
}

try {
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = Join-Path $root 'data/fleet.sqlite'
    $env:FLEET_RUNNER_BIN = Join-Path $bin 'fleet-runner.exe'
    $daemon = Start-Process (Join-Path $bin 'fleetd.exe') -PassThru `
        -RedirectStandardOutput (Join-Path $root 'daemon.out') `
        -RedirectStandardError (Join-Path $root 'daemon.err')
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path (Join-Path $runtime 'fleetd.addr'))) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'daemon did not start within 30 seconds'
        }
        Start-Sleep -Milliseconds 100
    }
    $daemonTokenPath = Join-Path $runtime 'fleetd.token'
    $daemonToken = [IO.File]::ReadAllText($daemonTokenPath)
    $workspaceOutput = Invoke-Fleetctl @('workspace', 'add', $root)
    $workspace = ($workspaceOutput -split "`t")[0]
    $sessions = @()
    $runnerTokens = @()
    for ($i = 0; $i -lt 2; $i++) {
        try {
            $sessionOutput = Invoke-Fleetctl @('session', 'create', '--workspace', $workspace, '--provider', 'fake')
            $session = ($sessionOutput -split "`t")[0]
        } finally {
            Track-Runners
        }
        $sessions += $session
        if ([IO.File]::ReadAllText($daemonTokenPath) -ne $daemonToken) {
            throw "daemon token changed after runner $i started"
        }
        $runnerPath = Join-Path $runtime "fleet-runner-runner-$session.token"
        $runnerToken = [IO.File]::ReadAllText($runnerPath)
        if ($runnerToken -eq $daemonToken -or $runnerTokens -contains $runnerToken) {
            throw 'runner token is not independent'
        }
        $runnerTokens += $runnerToken
        Assert-DaemonRejectsToken $runnerToken
        [void](Invoke-Fleetctl @('session', 'list'))
    }
    Assert-DaemonRejectsToken 'invalid-fixture-token'
    foreach ($session in $sessions) {
        [void](Invoke-Fleetctl @('prompt', $session, 'W1-A token isolation'))
    }
    foreach ($session in $sessions) {
        [void](Invoke-Fleetctl @('stop', $session))
    }
    Write-Output 'PASS: daemon token remains stable; two runners have independent tokens'
} finally {
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        Track-Runners
        Stop-Process -Id $daemon.Id -ErrorAction SilentlyContinue
        $daemon.WaitForExit()
    }
    foreach ($runner in $runners) {
        try {
            if (-not $runner.HasExited) {
                $runner.Kill()
                [void]$runner.WaitForExit(5000)
            }
        } finally {
            $runner.Dispose()
        }
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction Stop
}
