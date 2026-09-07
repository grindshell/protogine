# Drive the Phase 0 tilemap/collision probe under an independent child watchdog.
#
# The probe itself is prototype geometry, not the production kernel, so this
# runner only owns process isolation, timeouts and marker checking. A positive
# mode must print its own PASS marker; a control mode must fail at the named
# assertion it exists to trip, not merely exit nonzero or time out.
param(
    [ValidateSet('numeric', 'work', 'control-endpoint', 'control-corners', 'control-truncate',
        'control-yfirst', 'control-naive-clamp')]
    [string]$Mode = 'numeric'
)
$ErrorActionPreference = 'Stop'
$probeRoot = Split-Path -Parent $PSScriptRoot
$probeOutput = Join-Path $probeRoot 'target/tilemap-probe'
New-Item -ItemType Directory -Path $probeOutput -Force | Out-Null
$probeExecutable = Join-Path $probeRoot 'target/release/examples/tilemap_probe.exe'
if (-not (Test-Path -LiteralPath $probeExecutable)) {
    throw "Build it first: cargo build --release --example tilemap_probe --no-default-features"
}
$probeStdout = Join-Path $probeOutput "$Mode.stdout.txt"
$probeStderr = Join-Path $probeOutput "$Mode.stderr.txt"
$probeProcess = Start-Process -FilePath $probeExecutable -ArgumentList $Mode -WorkingDirectory $probeRoot `
    -PassThru -WindowStyle Hidden -RedirectStandardOutput $probeStdout -RedirectStandardError $probeStderr
# Cache the handle while the child is still running. Windows PowerShell 5.1 otherwise
# leaves ExitCode null for a redirected Start-Process, which would skip the checks below.
$null = $probeProcess.Handle
$probeTimeout = if ($Mode -eq 'work') { 60000 } else { 30000 }
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

# Each control replaces one frozen rule with the mistake it prevents, and names
# the assertion that must catch it.
$probeControls = @{
    'control-endpoint'    = 'high-speed wall stop'
    'control-corners'     = 'interior solid cell'
    'control-truncate'    = 'negative origin cell index'
    'control-yfirst'      = 'x-before-y corner'
    'control-naive-clamp' = 'clamped box on the free side'
}
if ($probeControls.ContainsKey($Mode)) {
    $probeMarker = $probeControls[$Mode]
    if ($probeExitCode -eq 0) { throw "Negative control $Mode exited 0; its rule no longer discriminates" }
    if (-not (Select-String -LiteralPath $probeStderr -SimpleMatch $probeMarker -Quiet)) {
        throw "Negative control $Mode failed somewhere other than '$probeMarker'"
    }
    "NEGATIVE CONTROL DETECTED: $Mode at '$probeMarker'"
} else {
    if ($probeExitCode -ne 0) { throw "Probe $Mode exited $probeExitCode" }
    # Exiting zero does not prove the assertions ran, so require the probe's own marker.
    $probeMarker = "TILEMAP PASS mode=$Mode"
    if (-not (Select-String -LiteralPath $probeStdout -SimpleMatch $probeMarker -Quiet)) {
        throw "Probe $Mode exited 0 without printing '$probeMarker'"
    }
    "PASS MARKER CONFIRMED: $Mode"
}
