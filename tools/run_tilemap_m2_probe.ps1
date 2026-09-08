# Drive the M2 Phase 0 probe under an independent child watchdog.
#
# The probe is prototype geometry over a `Vec<TileMap>`, not the production
# kernel, so this runner only owns process isolation, timeouts and marker
# checking. Every mode must print its own PASS marker: exiting zero does not
# prove the assertions ran, and this probe's assertions are most of its value -
# each mode checks that its own configuration was exercised, not only that its
# result looked plausible.
#
# There are no control modes, unlike M1's Phase 0 runner. That probe froze
# numerical rules and each control replaced one with the mistake it prevents;
# M2's Phase 0 freezes no numerical rule, so there is nothing for a control to
# remove. The six mutation controls M2 needs belong to Phases 1 to 3.
param(
    [ValidateSet('storage', 'work', 'shapes', 'timing')]
    [string]$Mode = 'storage'
)
$ErrorActionPreference = 'Stop'
$probeRoot = Split-Path -Parent $PSScriptRoot
$probeOutput = Join-Path $probeRoot 'target/tilemap-m2-probe'
New-Item -ItemType Directory -Path $probeOutput -Force | Out-Null
$probeExecutable = Join-Path $probeRoot 'target/release/examples/tilemap_m2_probe.exe'
if (-not (Test-Path -LiteralPath $probeExecutable)) {
    throw "Build it first: cargo build --release --example tilemap_m2_probe --no-default-features"
}
$probeStdout = Join-Path $probeOutput "$Mode.stdout.txt"
$probeStderr = Join-Path $probeOutput "$Mode.stderr.txt"
$probeProcess = Start-Process -FilePath $probeExecutable -ArgumentList $Mode -WorkingDirectory $probeRoot `
    -PassThru -WindowStyle Hidden -RedirectStandardOutput $probeStdout -RedirectStandardError $probeStderr
# Cache the handle while the child is still running. Windows PowerShell 5.1
# otherwise leaves ExitCode null for a redirected Start-Process, which would
# skip the checks below.
$null = $probeProcess.Handle
# `timing` repeats the pass fifteen times; the rest are a single arrangement.
$probeTimeout = if ($Mode -eq 'timing') { 60000 } else { 30000 }
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
if ($probeExitCode -ne 0) { throw "Probe $Mode exited $probeExitCode" }
$probeMarker = "TILEMAP-M2 PASS mode=$Mode"
if (-not (Select-String -LiteralPath $probeStdout -SimpleMatch $probeMarker -Quiet)) {
    throw "Probe $Mode exited 0 without printing '$probeMarker'"
}
"PASS MARKER CONFIRMED: $Mode"
