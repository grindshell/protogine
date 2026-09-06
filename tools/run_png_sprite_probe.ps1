param([ValidateSet('cpu', 'gpu', 'gpu-recreate', 'gpu-early-retire')][string]$Mode = 'cpu')
$ErrorActionPreference = 'Stop'
$probeRoot = Split-Path -Parent $PSScriptRoot
$probeOutput = Join-Path $probeRoot 'target/png-sprite-probe'
New-Item -ItemType Directory -Path $probeOutput -Force | Out-Null
if ($Mode -eq 'cpu') {
    python (Join-Path $PSScriptRoot 'png_probe_fixtures.py')
    if ($LASTEXITCODE -ne 0) { throw 'Fixture generation failed' }
}
$probeExecutable = Join-Path $probeRoot 'target/release/examples/png_sprite_probe.exe'
$probeStdout = Join-Path $probeOutput "$Mode.stdout.txt"
$probeStderr = Join-Path $probeOutput "$Mode.stderr.txt"
$probeProcess = Start-Process -FilePath $probeExecutable -ArgumentList $Mode -WorkingDirectory $probeRoot -PassThru -WindowStyle Hidden -RedirectStandardOutput $probeStdout -RedirectStandardError $probeStderr
# Cache the handle while the child is still running. Windows PowerShell 5.1 otherwise
# leaves ExitCode null for a redirected Start-Process, which would skip the checks below.
$null = $probeProcess.Handle
$probeTimeout = if ($Mode -eq 'cpu') { 10000 } else { 30000 }
if (-not $probeProcess.WaitForExit($probeTimeout)) {
    $probeProcess.Kill()
    $probeProcess.WaitForExit()
    throw "Probe $Mode exceeded its independent $probeTimeout ms watchdog"
}
$probeProcess.Refresh()
$probeExitCode = $probeProcess.ExitCode
Get-Content -LiteralPath $probeStdout
Get-Content -LiteralPath $probeStderr
if ($null -eq $probeExitCode) { throw "Probe $Mode reported no exit code; its result cannot be checked" }
$probeExpectedFailure = $Mode -in @('gpu-recreate', 'gpu-early-retire')
if ($probeExpectedFailure) {
    $probeMarker = if ($Mode -eq 'gpu-recreate') { 'negative control: texture registration cap' } else { 'pixel at 12,12' }
    if ($probeExitCode -eq 0 -or -not (Select-String -LiteralPath $probeStderr -SimpleMatch $probeMarker -Quiet)) {
        throw "Negative control $Mode did not fail at its expected assertion"
    }
    "NEGATIVE CONTROL DETECTED: $Mode"
} else {
    if ($probeExitCode -ne 0) { throw "Probe $Mode exited $probeExitCode" }
    # Exiting zero does not prove the assertions ran, so require the probe's own marker.
    $probeMarker = if ($Mode -eq 'cpu') { 'CPU PASS' } else { "GPU PASS mode=$Mode" }
    if (-not (Select-String -LiteralPath $probeStdout -SimpleMatch $probeMarker -Quiet)) {
        throw "Probe $Mode exited 0 without printing '$probeMarker'"
    }
    "PASS MARKER CONFIRMED: $Mode"
}
