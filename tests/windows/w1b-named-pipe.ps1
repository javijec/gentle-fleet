$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-w1b-" + [guid]::NewGuid().ToString('N'))
$runtime = Join-Path $root 'runtime'
$daemon = $null
$competitor = $null
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
    $name = $address.Substring('\\.\pipe\'.Length)
    $client = [IO.Pipes.NamedPipeClientStream]::new('.', $name, [IO.Pipes.PipeDirection]::InOut)
    try {
        $client.Connect(5000)
        $writer = [IO.StreamWriter]::new($client, [Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [IO.StreamReader]::new($client, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $writer.WriteLine('{"kind":"request","request_id":"invalid","request":{"type":"hello","client_id":"w1b","token":"invalid-fixture-token"}}')
            $writer.Flush()
            $line = $reader.ReadLineAsync()
            if (-not $line.Wait(5000)) { throw 'daemon did not respond to invalid token' }
            $response = $line.Result | ConvertFrom-Json
            if ($response.result.Err.code -ne 'Unauthorized') {
                throw 'daemon did not explicitly reject invalid credentials'
            }
        } finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    } finally {
        $client.Dispose()
    }
    # Exercise actual SSH bridge half-close over the named pipe:
    # EOF on stdin must not discard the pending fleetd responses.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $bin 'fleet-ssh-bridge.exe'
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $bridge = [Diagnostics.Process]::Start($start)
    try {
        $bridge.StandardInput.WriteLine('{"kind":"request","request_id":"h1","request":{"type":"hello","client_id":"w1b","token":null}}')
        $bridge.StandardInput.WriteLine('{"kind":"request","request_id":"a1","request":{"type":"list_workspaces"}}')
        $bridge.StandardInput.Close()
        $outTask = $bridge.StandardOutput.ReadToEndAsync()
        $errTask = $bridge.StandardError.ReadToEndAsync()
        if (-not $bridge.WaitForExit(15000)) {
            $bridge.Kill()
            [void]$bridge.WaitForExit(5000)
            $partial = $outTask.GetAwaiter().GetResult()
            $diagnostics = $errTask.GetAwaiter().GetResult()
            $responseIds = @($partial -split "\r?\n" | Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object {
                try { ($_ | ConvertFrom-Json).request_id } catch { '<invalid JSONL>' }
            }) -join ','
            throw "SSH bridge did not exit after EOF (response IDs: $responseIds; diagnostics: $diagnostics)"
        }
        $output = $outTask.GetAwaiter().GetResult()
        $errors = $errTask.GetAwaiter().GetResult()
        if ($bridge.ExitCode -ne 0) {
            throw "bridge exited unsuccessfully after EOF: $errors"
        }
        $frames = @($output -split "\r?\n" | Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object { $_ | ConvertFrom-Json })
        if (@($frames | Where-Object { $_.request_id -eq 'h1' -and $_.result.Ok.type -eq 'hello' }).Count -ne 1) {
            throw 'SSH bridge dropped Hello response after half-close'
        }
        if (@($frames | Where-Object { $_.request_id -eq 'a1' -and $_.result.Ok.type -eq 'workspaces' }).Count -ne 1) {
            throw 'SSH bridge dropped Workspaces response after half-close'
        }
    } finally {
        if (-not $bridge.HasExited) {
            $bridge.Kill()
            [void]$bridge.WaitForExit(5000)
        }
        $bridge.Dispose()
    }

    $env:FLEET_DB_PATH = Join-Path $root 'competitor/fleet.sqlite'
    $competitor = Start-Process (Join-Path $bin 'fleetd.exe') -PassThru `
        -RedirectStandardOutput (Join-Path $root 'competitor.out') `
        -RedirectStandardError (Join-Path $root 'competitor.err')
    if (-not $competitor.WaitForExit(10000)) {
        throw 'second daemon did not reject the active endpoint within 10 seconds'
    }
    if ($competitor.ExitCode -eq 0) { throw 'second daemon unexpectedly succeeded' }
    if ($daemon.HasExited) { throw 'first daemon exited after endpoint collision' }
    if (([IO.File]::ReadAllText($addressFile)).Trim() -ne $address) {
        throw 'second daemon replaced the active endpoint'
    }
    Write-Output 'PASS: named pipe endpoint rejects invalid token and active endpoint collision'
} finally {
    if ($null -ne $competitor) {
        if (-not $competitor.HasExited) {
            $competitor.Kill()
            [void]$competitor.WaitForExit(5000)
        }
        $competitor.Dispose()
    }
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        $daemon.Kill()
        [void]$daemon.WaitForExit(5000)
    }
    if ($null -ne $daemon) { $daemon.Dispose() }
    Remove-Item -LiteralPath $root -Recurse -Force
}
