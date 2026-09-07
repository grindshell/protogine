# Materialise an isolated copy of the working tree for a mutation-control run.
#
# Controls patch engine sources in place. Doing that in the working tree makes
# every build that happens to overlap the run - another session's `cargo test`,
# an editor checking on save, a second harness - silently compile a deliberately
# broken tree. That is non-reproducible and gives no signal at the time, so a
# gate can report a result that was never about the code under review.
#
# Patching a copy removes the hazard rather than serialising around it: two runs
# can overlap freely, and the working tree is never written to at all. The copy
# lives under `target/`, which is already ignored, and keeps its own build
# artifacts between runs so only the engine crate rebuilds per control.
function New-ControlTree {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Name
    )
    $tree = Join-Path $Repo "target\$Name\tree"
    New-Item -ItemType Directory -Force -Path $tree | Out-Null
    foreach ($entry in Get-ChildItem -LiteralPath $Repo -Force) {
        # `target` would recurse into the copy; `.git` is large and unused here.
        if ($entry.Name -eq 'target' -or $entry.Name -eq '.git') { continue }
        $destination = Join-Path $tree $entry.Name
        # Replaced rather than merged, so a file deleted since the last run does
        # not survive in the copy and quietly keep a stale control compiling.
        if (Test-Path -LiteralPath $destination) {
            Remove-Item -LiteralPath $destination -Recurse -Force
        }
        Copy-Item -LiteralPath $entry.FullName -Destination $destination -Recurse -Force
    }
    # `Copy-Item` preserves the source's timestamps, which can be older than the
    # fingerprints cargo recorded in the copy's reused `target/` on an earlier
    # run. Cargo decides staleness by modification time, so an older-looking
    # source can leave it reusing a binary built from different code. Stamping
    # the copy now keeps time moving forward across runs.
    $stamp = Get-Date
    Get-ChildItem -LiteralPath $tree -Recurse -File -Force |
        Where-Object { $_.FullName -notmatch '\\target\\' } |
        ForEach-Object { $_.LastWriteTime = $stamp }
    $tree
}

# A fingerprint of the files a run is allowed to leave untouched, so the harness
# can prove it wrote nothing to the working tree instead of asserting it.
function Get-SourceFingerprint {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string[]]$Files
    )
    ($Files | ForEach-Object {
        "$_=" + (Get-FileHash -LiteralPath (Join-Path $Repo $_) -Algorithm SHA256).Hash
    }) -join ';'
}
