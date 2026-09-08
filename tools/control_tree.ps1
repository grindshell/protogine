# Materialise an isolated copy of the working tree for a mutation-control run.
#
# Controls patch engine sources in place. Doing that in the working tree makes
# every build that happens to overlap the run - another session's `cargo test`,
# an editor checking on save - silently compile a deliberately broken tree. That
# is non-reproducible and gives no signal at the time, so a gate can report a
# result that was never about the code under review.
#
# Patching a copy removes that hazard: the working tree is never written to at
# all, and an unrelated build cannot see a control's patch. The copy lives under
# `target/`, which is already ignored, and keeps its own build artifacts between
# runs so only the engine crate rebuilds per control.
#
# **It does not make two runs of the same harness safe, and an earlier version of
# this comment claimed it did.** The copy's path is derived from the harness
# name, so two concurrent runs of one harness share a single patched tree: each
# writes its own control's patch and each restores the originals in its own loop,
# so they overwrite one another mid-control. The saved cargo output is worse,
# because each run clears that directory at startup and would delete the other's
# evidence. `Enter-ControlLock` is what makes the exclusion real; the copy alone
# only ever protected the working tree.
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

# Claim exclusive use of one harness's patched tree, or refuse loudly.
#
# A lock rather than a per-run path, deliberately. Unique paths would let two
# runs proceed, but each would need its own build cache, so every run would pay a
# from-scratch build of the whole dependency graph; and two runs of a
# verification harness is a mistake rather than a use case, so the useful
# response is to say so by name instead of quietly accommodating it. Sharing the
# build cache is fine; sharing patched sources is not.
#
# A lock left by a dead process is taken over rather than treated as fatal: a run
# killed part-way through would otherwise wedge the harness until someone deleted
# a file by hand. The recorded start time is compared as well as the identifier,
# because the operating system reuses process identifiers.
function Enter-ControlLock {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Name
    )
    $directory = Join-Path $Repo "target\$Name"
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    $lock = Join-Path $directory 'run.lock'
    if (Test-Path -LiteralPath $lock) {
        $held = ([IO.File]::ReadAllText($lock) -split "`n")[0] -split ' '
        $owner = $null
        if ($held.Count -ge 1 -and $held[0] -match '^\d+$') {
            $owner = Get-Process -Id ([int]$held[0]) -ErrorAction SilentlyContinue
        }
        # Same identifier and same start time: the run that took this lock is
        # still going. Same identifier alone proves nothing after a reboot.
        if ($owner -and $held.Count -ge 2 -and $owner.StartTime.ToString('o') -eq $held[1]) {
            throw @"
another $Name run is already using target\$Name (process $($held[0]), started $($held[1])).
Two runs of one harness share a single patched tree and would overwrite each
other's controls mid-run, so this one refuses rather than producing results about
the wrong bytes. Wait for it to finish, or stop it and delete
  $lock
"@
        }
        # `Write-Host`, not `Write-Output`: a function returns everything it
        # writes to the output stream, so a note there would make this return
        # the note *and* the path as an array. The caller stores that in one
        # variable, passes it to a `[string]` parameter, and the release quietly
        # matches nothing - so the first takeover would leave a lock that every
        # later run takes over and never releases. That happened.
        # Say what it is. The single wording claimed a process by the recorded
        # name was no longer running, which reads as a fact about a process even
        # when the file held no process at all.
        $note = if ($held.Count -ge 2 -and $held[0] -match '^\d+$') {
            "taking over a $Name lock left by process $($held[0]), which is no longer running"
        } else {
            "taking over a malformed $Name lock"
        }
        Write-Host "note: $note"
    }
    [IO.File]::WriteAllText(
        $lock,
        "$PID $((Get-Process -Id $PID).StartTime.ToString('o'))`n",
        (New-Object Text.UTF8Encoding $false)
    )
    return $lock
}

function Exit-ControlLock {
    param([string]$Path)
    if ($Path -and (Test-Path -LiteralPath $Path)) {
        Remove-Item -LiteralPath $Path -Force
    }
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
