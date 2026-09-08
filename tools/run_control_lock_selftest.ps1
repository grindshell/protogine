# Self-tests for the mutation-control run lock in `control_tree.ps1`.
#
# The lock is what stops two runs of one harness from sharing a patched tree and
# overwriting each other's controls. It guards every conclusion all three
# harnesses produce, so it owes the same standard they enforce: a guard nobody
# has watched fail is not a guard.
#
# It runs no cargo and takes about a second, so run it whenever `control_tree.ps1`
# changes rather than only when something looks wrong.
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')

$name = 'control-lock-selftest'
$directory = Join-Path $repo "target\$name"
$lock = Join-Path $directory 'run.lock'
if (Test-Path -LiteralPath $directory) { Remove-Item -LiteralPath $directory -Recurse -Force }
New-Item -ItemType Directory -Force -Path $directory | Out-Null

$failures = @()
function Hold-Lock {
    param([string]$Text)
    [IO.File]::WriteAllText($lock, $Text, (New-Object Text.UTF8Encoding $false))
}

# Every acquisition must yield exactly one usable path. This is not pedantry
# about types: the caller keeps the result in one variable and hands it to a
# `[string]` release parameter, so an acquisition that also wrote a note to the
# output stream returned two values, the release matched nothing, and the first
# takeover left a lock that every later run took over and never released. That
# is the defect this assertion exists for, and the version of this file that
# only checked truthiness did not catch it.
function Assert-SinglePath {
    param($Value, [string]$What)
    if (@($Value).Count -ne 1) {
        $script:failures += "$What returned $(@($Value).Count) values, not one path"
    } elseif ($Value -isnot [string]) {
        $script:failures += "$What returned a $($Value.GetType().Name), not a path"
    } elseif (-not (Test-Path -LiteralPath $Value)) {
        $script:failures += "$What returned a path that does not exist"
    }
}

# 1. A clean acquisition takes the lock and releases it.
$held = Enter-ControlLock -Repo $repo -Name $name
Assert-SinglePath $held 'a clean acquisition'
Exit-ControlLock -Path $held
if (Test-Path -LiteralPath $lock) { $failures += 'the release after a clean acquisition left the lock' }

# 2. A live holder is refused, by name, so the message says which run to wait for.
Hold-Lock "$PID $((Get-Process -Id $PID).StartTime.ToString('o'))`n"
try {
    Enter-ControlLock -Repo $repo -Name $name | Out-Null
    $failures += 'a live holder did not refuse the second run'
} catch {
    if ($_.Exception.Message -notmatch 'already using') {
        $failures += "the refusal did not say the tree was in use: $($_.Exception.Message)"
    } elseif ($_.Exception.Message -notmatch [regex]::Escape("$PID")) {
        $failures += 'the refusal did not name the holding process'
    }
}
if (-not (Test-Path -LiteralPath $lock)) { $failures += 'a refused run removed the holder''s lock' }

# 3. A lock whose process is gone is taken over, not treated as fatal: a run
#    killed part-way through would otherwise wedge the harness until someone
#    deleted a file by hand.
Hold-Lock "999999 2020-01-01T00:00:00.0000000+00:00`n"
$held = Enter-ControlLock -Repo $repo -Name $name
Assert-SinglePath $held 'a takeover'
Exit-ControlLock -Path $held
if (Test-Path -LiteralPath $lock) { $failures += 'the release after a takeover left the lock' }

# 4. The operating system reuses process identifiers, so the recorded start time
#    decides, not the number.
Hold-Lock "$PID 2020-01-01T00:00:00.0000000+00:00`n"
$held = Enter-ControlLock -Repo $repo -Name $name
Assert-SinglePath $held 'a recycled identifier with a different start time'
Exit-ControlLock -Path $held

# 5. A malformed lock is taken over rather than crashing the run that finds it,
#    and says so rather than claiming a process by that name is not running.
#
#    `-InformationVariable` captures the note without displacing the return
#    value. Redirecting the stream and rebuilding the path by hand also reads the
#    note, but then the acquisition's own answer is thrown away and
#    `Assert-SinglePath` checks a string this file just constructed - which can
#    only ever pass. Adding the note assertion that way silently removed the one
#    it sits beside.
Hold-Lock "not-a-process`n"
$held = Enter-ControlLock -Repo $repo -Name $name -InformationVariable note
Assert-SinglePath $held 'a malformed lock'
if ("$note" -notmatch 'malformed') {
    $failures += "the malformed-lock note did not say it was malformed: $("$note".Trim())"
}
Exit-ControlLock -Path $held

Remove-Item -LiteralPath $directory -Recurse -Force
if ($failures.Count -gt 0) {
    "`nCONTROL LOCK IS BROKEN:"
    $failures | ForEach-Object { "  $_" }
    exit 1
}
"`nAll five control-lock cases behave as specified."
