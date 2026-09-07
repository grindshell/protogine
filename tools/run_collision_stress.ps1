# Max-load stress for the swept solver, under an independent child watchdog.
#
# The plan requires the 1,024 x 256 map, 1,024 maximum-footprint colliders and
# the full 16,384-entity population as evidence, in both a long-sweep and a
# sparse short-motion arrangement. The long-sweep arrangement must complete
# inside the fixed-pass ceiling rather than fault: a fault there would mean the
# ceiling is wrong, not that the workload is unreasonable.
#
# The watchdog is separate from the test's own assertions on purpose. A run that
# hangs is not a slow pass, and `cargo test` alone cannot tell the two apart.
param([int]$TimeoutSeconds = 300)
$ErrorActionPreference = 'Stop'
$stressRepo = Split-Path -Parent $PSScriptRoot

$arguments = @(
    'test', '--release', '--test', 'collision', '--no-default-features',
    '--', '--ignored', '--nocapture', '--test-threads', '1',
    'max_load_stress_completes_inside_the_fixed_pass_ceiling'
)
$stressOutput = Join-Path ([IO.Path]::GetTempPath()) ("collision-stress-" + [guid]::NewGuid().ToString('N') + ".txt")
$stressProcess = Start-Process -FilePath 'cargo' -ArgumentList $arguments -WorkingDirectory $stressRepo `
    -NoNewWindow -PassThru -RedirectStandardOutput $stressOutput -RedirectStandardError "$stressOutput.err"

if (-not $stressProcess.WaitForExit($TimeoutSeconds * 1000)) {
    try { $stressProcess.Kill($true) } catch { }
    Get-Content $stressOutput -ErrorAction SilentlyContinue
    throw "stress run exceeded $TimeoutSeconds seconds and was killed; a hang is not a slow pass"
}

$stressText = (Get-Content $stressOutput -Raw -ErrorAction SilentlyContinue) +
              (Get-Content "$stressOutput.err" -Raw -ErrorAction SilentlyContinue)
Remove-Item $stressOutput, "$stressOutput.err" -ErrorAction SilentlyContinue

if ($stressProcess.ExitCode -ne 0) {
    # A failure here is a real finding about the ceiling or the solver, so show
    # everything rather than the measurement lines a filter would keep.
    $stressText
    throw "stress run failed with exit code $($stressProcess.ExitCode)"
}
$stressText -split "`r?`n" | Where-Object { $_ -match 'long-sweep|sparse-short-motion|test result' }
# A zero exit does not prove the measurements ran, so require both arrangements
# to have reported.
foreach ($arrangement in @('long-sweep:', 'sparse-short-motion:')) {
    if ($stressText -notmatch [regex]::Escape($arrangement)) {
        throw "the $arrangement arrangement produced no measurement line"
    }
}
"`nBoth mandated arrangements completed inside the fixed-pass ceiling."
