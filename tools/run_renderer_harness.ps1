# Run one mode of the Phase 3 GPU harness under an independent watchdog.
#
# The harness drives the production MacroquadRenderer and AssetStore inside a
# live graphics context on the main thread, which a cargo test harness cannot
# supply. `recreate` is a negative control and must fail at its named assertion.
param([ValidateSet('cycles', 'bands', 'eviction', 'pressure', 'recreate')][string]$Mode = 'cycles')
$ErrorActionPreference = 'Stop'
$harnessRoot = Split-Path -Parent $PSScriptRoot
$harnessOutput = Join-Path $harnessRoot 'target/renderer-harness'
$harnessBundle = Join-Path $harnessOutput "$Mode-bundle"
New-Item -ItemType Directory -Path $harnessOutput -Force | Out-Null
Remove-Item -Recurse -Force $harnessBundle -ErrorAction SilentlyContinue

$harnessExecutable = Join-Path $harnessRoot 'target/release/examples/renderer_harness.exe'
if (-not (Test-Path -LiteralPath $harnessExecutable)) {
    throw "Build it first: cargo build --release --no-default-features --features graphics --example renderer_harness"
}
$harnessStdout = Join-Path $harnessOutput "$Mode.stdout.txt"
$harnessStderr = Join-Path $harnessOutput "$Mode.stderr.txt"
$harnessProcess = Start-Process -FilePath $harnessExecutable -ArgumentList $Mode, $harnessBundle -WorkingDirectory $harnessRoot -PassThru -WindowStyle Hidden -RedirectStandardOutput $harnessStdout -RedirectStandardError $harnessStderr
# Cache the handle while the child runs. Windows PowerShell 5.1 otherwise leaves
# ExitCode null for a redirected Start-Process, which would skip the checks below.
$null = $harnessProcess.Handle
$harnessTimeout = 60000
if (-not $harnessProcess.WaitForExit($harnessTimeout)) {
    $harnessProcess.Kill()
    $harnessProcess.WaitForExit()
    throw "Harness $Mode exceeded its independent $harnessTimeout ms watchdog"
}
$harnessProcess.Refresh()
$harnessExitCode = $harnessProcess.ExitCode
Get-Content -LiteralPath $harnessStdout
Get-Content -LiteralPath $harnessStderr
if ($null -eq $harnessExitCode) { throw "Harness $Mode reported no exit code; its result cannot be checked" }
if ($Mode -eq 'recreate') {
    $harnessMarker = 'negative control:'
    if ($harnessExitCode -eq 0 -or -not (Select-String -LiteralPath $harnessStderr -SimpleMatch $harnessMarker -Quiet)) {
        throw "Negative control $Mode did not fail at its expected assertion"
    }
    "NEGATIVE CONTROL DETECTED: $Mode"
} else {
    if ($harnessExitCode -ne 0) { throw "Harness $Mode exited $harnessExitCode" }
    # Exiting zero does not prove the assertions ran, so require the marker.
    if (-not (Select-String -LiteralPath $harnessStdout -SimpleMatch "PASS mode=$Mode" -Quiet)) {
        throw "Harness $Mode exited 0 without printing 'PASS mode=$Mode'"
    }
    "PASS MARKER CONFIRMED: $Mode"
}
