# Negative controls for the migrated sprite sample and the boundary it sits on.
#
# Each control removes one rule, runs the single test that is supposed to catch
# it, and requires that test to fail at a named assertion. A guard that cannot be
# observed failing is not covered, and a passing suite alone does not distinguish
# the two.
#
# Markers name the exact assertion, never a generic 'assertion' or 'panicked'
# substring: those match any failure at all, which would reduce the harness to
# "something broke" and silently absorb a control that moved to a different
# failure site.
#
# **Most of the rules here live in Luau, not in Rust, and that changes one gate.**
# `examples/games/sprites/main.luau` is data the test bundle loads at run time,
# so patching it makes cargo rebuild nothing at all: the previous test binary
# reads the copy's current file and behaves differently, which is exactly what
# the control wants. The rebuild gate therefore applies only to controls that
# edit a `.rs` source, where a binary built from different code really is a
# reachable way to report a live guard as dead. The staleness the gate guards
# against has no analogue for a file read at run time - there is no compiled
# copy of it to go stale - and the other three gates are unchanged.
#
# Every patch is written to an isolated copy of the tree under `target/`, never
# to the working tree, so a build that overlaps this run cannot pick up a
# deliberately broken source. The run proves that rather than asserting it: it
# fingerprints the sources before and after and fails if either moved.
#
# One run at a time. Two runs of one harness share its patched copy and would
# overwrite each other's controls mid-run, so `Enter-ControlLock` refuses the
# second by name rather than accommodating it.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')
$controlSources = @(
    'examples/games/sprites/main.luau',
    'src/runtime.rs'
)
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources
# Claimed before the copy exists, because the copy is what two runs would share.
# Released after the fingerprint check below, so both exits pass through it.
$controlLock = Enter-ControlLock -Repo $controlRepo -Name 'sprites-controls'

$features = @('--no-default-features', '--features', 'scripting')
$sample = @('--test', 'sprites_sample') + $features

# The authored room, read directly instead of the engine's grid: the "second
# mutable collision grid in Luau" the plan forbids, in the only form a migrated
# sample could plausibly grow one. `room.tiles` never changes, so a cell the
# game breaks keeps drawing as it was authored.
$authoredTile = 'room.legend[string.sub(room.tiles[row + 1], column + 1, column + 1)]'

$controls = @(
    # --- The sample reads the engine's grid, not its own -----------------------

    @{ Name = 'stale-luau-grid'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Marker = 'the broken crate must draw as floor'
       Edits = @(@{ F = '                    local tile = palette[cells[row * room.columns + column + 1]]'
                    R = "                    local tile = $authoredTile" }) }

    @{ Name = 'stale-placeholder-grid'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Marker = 'the broken crate must leave no placeholder wall behind'
       Edits = @(@{ F = '                    if palette[cells[row * room.columns + column + 1]].solid then'
                    R = "                    if $authoredTile.solid then" }) }

    # --- The sample moves through the engine, not around it --------------------

    @{ Name = 'collider-not-attached'; File = 'examples/games/sprites/main.luau'
       Test = 'collision_holds_while_the_room_image_is_loading_and_after_it_is_unloaded'
       Marker = 'the fence must stop the character while its art is still loading'
       Edits = @(@{ F = '        ctx.world.set_tile_collider(body, {offset_x = 0, offset_y = 0, width = SIZE, height = SIZE})'
                    R = '' }) }

    @{ Name = 'fixed-pass-in-draw'; File = 'src/runtime.rs'
       Test = 'repeated_draws_and_zero_tick_frames_move_nothing'
       Marker = 'a draw must not run the fixed pass'
       Edits = @(@{ F = '        let engine = EngineContext::new(&mut self.kernel, self.draw_input);'
                    R = "        let _ = self.kernel.fixed_update();`n        let engine = EngineContext::new(&mut self.kernel, self.draw_input);" }) }

    # --- The edit the sample makes is the one it meant to make -----------------

    @{ Name = 'crate-break-unchecked'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Marker = 'only a crate may break under a refused push'
       Edits = @(@{ F = '                if world.tile(column, row) == CRATE then'; R = '                if true then' }) }

    @{ Name = 'push-detection-ignores-the-refusal'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Marker = 'the crate must stop the character at its face'
       Edits = @(@{ F = '        if last_dx ~= 0 and p.x == last_x then'; R = '        if last_dx ~= 0 then' }) }

    # The centring guards two different things, so it gets one control per half
    # rather than one for the pair. This is the perpendicular half: the box
    # straddles rows 9 and 10 with the crate in the lower one, so the top edge
    # alone names an empty cell and the leading-edge form finds no crate.
    @{ Name = 'ahead-perpendicular-uncentred'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Marker = 'the broken crate must draw as floor'
       Edits = @(@{ F = '    return (px + SIZE / 2 + dx * SIZE) // SIZE, (py + SIZE / 2 + dy * SIZE) // SIZE'
                    R = '    return (px + SIZE / 2 + dx * SIZE) // SIZE, (py + dy * SIZE) // SIZE' }) }

    # --- Recorded redundancy ---------------------------------------------------

    # The pressed half of the same rule, and expected to keep passing. The rule
    # only fires when that axis did not move, which means the box is flush
    # against a face. The clamp's ideal is `(face - size) - offset` moving
    # forward, in `clamp_below`, and `face - offset` moving back, in
    # `clamp_above` - no size term at all, and the one this left-pressing fixture
    # actually reaches. With this collider's zero offset, 32-pixel extent and
    # 32-pixel tiles both are exact, so the committed coordinate *is* the face,
    # and a coordinate exactly on a face floors to the same cell under both forms
    # of `ahead`, in every direction, so nothing this sample can do distinguishes
    # them. The centring stays because the 2^-28 gap is real in the general
    # contract - a non-zero offset or a non-integer extent reaches it - and a
    # sample should be written to the contract rather than to its own arithmetic.
    #
    # This entry replaced a TODO that would have had someone build a fixture
    # pressing a crate from the west or the north. Those walks exist, and both
    # pass with this patch applied, so that fixture would have been a control
    # that cannot fail.
    @{ Name = 'ahead-pressed-axis-uncentred'; File = 'examples/games/sprites/main.luau'; Passes = $true
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Edits = @(@{ F = '    return (px + SIZE / 2 + dx * SIZE) // SIZE, (py + SIZE / 2 + dy * SIZE) // SIZE'
                    R = '    return (px + dx * SIZE) // SIZE, (py + SIZE / 2 + dy * SIZE) // SIZE' }) }

    # Expected to keep passing. The room's border is solid, so the character can
    # never stand in it, and the cell one tile from its centre is always inside
    # the grid. The check stays because `tile` refuses an outside index rather
    # than answering it, and a sample should not hand an engine call an argument
    # it has not checked - but nothing here can reach the branch, and saying so
    # is better than implying a fixture covers it.
    @{ Name = 'ahead-bounds-unchecked'; File = 'examples/games/sprites/main.luau'; Passes = $true
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Edits = @(@{ F = '            if column >= 0 and column < room.columns and row >= 0 and row < room.rows then'
                    R = '            if true then' }) }

    # --- Self-tests: the harness must refuse all four --------------------------

    @{ Name = 'self-test-clobbered-source'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Expect = 'the patched source changed under the run'; Clobber = $true
       Edits = @(@{ F = '                    local tile = palette[cells[row * room.columns + column + 1]]'
                    R = "                    local tile = $authoredTile" }) }

    # Only meaningful against a Rust edit, which is the point: it is the one
    # control here whose conclusion depends on cargo having rebuilt.
    @{ Name = 'self-test-backdated-source'; File = 'src/runtime.rs'
       Test = 'repeated_draws_and_zero_tick_frames_move_nothing'
       Expect = 'cargo did not rebuild'; Backdate = $true
       Edits = @(@{ F = '        let engine = EngineContext::new(&mut self.kernel, self.draw_input);'
                    R = "        let _ = self.kernel.fixed_update();`n        let engine = EngineContext::new(&mut self.kernel, self.draw_input);" }) }

    @{ Name = 'self-test-missing-test'; File = 'examples/games/sprites/main.luau'
       Test = 'a_test_name_that_does_not_exist'
       Expect = 'did not execute exactly one test'
       Edits = @(@{ F = '                    local tile = palette[cells[row * room.columns + column + 1]]'
                    R = "                    local tile = $authoredTile" }) }

    @{ Name = 'self-test-inert-edit'; File = 'examples/games/sprites/main.luau'
       Test = 'pushing_into_a_crate_breaks_the_one_cell_that_both_draws_and_blocks'
       Expect = 'did not fail with the rule removed'
       Edits = @(@{ F = 'local BAR_MARGIN = 24'; R = 'local BAR_MARGIN = 20' }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'sprites-controls'
$controlManifest = Join-Path $controlTree 'Cargo.toml'
$controlOriginals = @{}
foreach ($file in $controlSources) {
    $controlOriginals[$file] = [IO.File]::ReadAllText((Join-Path $controlRepo $file))
}
function Restore-ControlSources {
    foreach ($file in $controlSources) {
        [IO.File]::WriteAllText((Join-Path $controlTree $file), $controlOriginals[$file], (New-Object Text.UTF8Encoding $false))
    }
}

$controlFailures = @()
# Every control's full cargo output, saved rather than summarised, because
# diagnosing a disagreement usually means comparing against a control that
# behaved. Passing ones are kept for the same reason.
$controlLogs = Join-Path $controlRepo 'target\sprites-controls\runs'
if (Test-Path -LiteralPath $controlLogs) { Remove-Item -LiteralPath $controlLogs -Recurse -Force }
New-Item -ItemType Directory -Force -Path $controlLogs | Out-Null
function Save-ControlOutput {
    param([string]$Name, [string]$Text)
    [IO.File]::WriteAllText((Join-Path $controlLogs "$Name.txt"), $Text, (New-Object Text.UTF8Encoding $false))
}
try {
    foreach ($control in $controls) {
        # An edit defaults to the control's own file but may name another, so a
        # rule that two files now enforce jointly can be removed from both.
        $patched = @{}
        foreach ($file in $controlSources) { $patched[$file] = $controlOriginals[$file] }
        $missing = $false
        $touchedRust = $false
        foreach ($edit in $control.Edits) {
            $file = if ($edit.File) { $edit.File } else { $control.File }
            if (-not $patched[$file].Contains($edit.F)) { $missing = $true; break }
            $patched[$file] = $patched[$file].Replace($edit.F, $edit.R)
            if ($file.EndsWith('.rs')) { $touchedRust = $true }
        }
        if ($missing) {
            $controlFailures += "$($control.Name): anchor no longer matches $($control.File); the control is stale, not the code"
            continue
        }

        foreach ($file in $controlSources) {
            [IO.File]::WriteAllText((Join-Path $controlTree $file), $patched[$file], (New-Object Text.UTF8Encoding $false))
        }
        if ($control.Backdate) {
            # Self-test for the rebuild gate. Cargo decides freshness by
            # modification time, so a source that looks older than the last
            # build is skipped and the previous binary runs with the mutation
            # compiled out entirely. A genuine instance of the failure mode that
            # gate catches, not a synthetic stand-in for it.
            $stale = (Get-Date).AddDays(-30)
            foreach ($file in $controlSources) {
                (Get-Item -LiteralPath (Join-Path $controlTree $file)).LastWriteTime = $stale
            }
        }
        $arguments = @('test', '--manifest-path', $controlManifest)
        if ($Release) { $arguments += '--release' }
        $arguments += $sample + @('--', $control.Test)
        $output = & cargo @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
        if ($control.Clobber) {
            # Self-test for the patch-survival gate. The cargo result is
            # identical to a real detection; only the evidence chain is broken,
            # so the harness must discard a conclusion that looks correct.
            foreach ($file in $controlSources) {
                [IO.File]::WriteAllText((Join-Path $controlTree $file), $controlOriginals[$file], (New-Object Text.UTF8Encoding $false))
            }
        }
        # Read back before restoring, which establishes that the patch was still
        # in place when cargo exited. That is weaker than "cargo compiled it",
        # and for a Luau control it is the load-bearing gate rather than a
        # secondary one, since nothing compiles the file at all.
        $applied = $true
        foreach ($file in $controlSources) {
            if ([IO.File]::ReadAllText((Join-Path $controlTree $file)) -ne $patched[$file]) {
                $applied = $false
            }
        }
        Restore-ControlSources
        Save-ControlOutput -Name $control.Name -Text $output

        $where = ($output -split "`r?`n" | Where-Object { $_ -match 'panicked at|assertion|left:|right:' } | Select-Object -First 3) -join ' | '
        # A zero exit is not evidence on its own: a filter that selects no test
        # also exits zero, and so does a stale binary cargo decided not to
        # rebuild. Require the run to say it executed exactly one test, and a
        # detecting control to say it failed.
        $verdict = $null
        $note = $null
        if (-not $applied) {
            $verdict = "$($control.Name): the patched source changed under the run, so it proves nothing"
        } elseif ($output -match 'error\[E\d+\]|could not compile') {
            $verdict = "$($control.Name): did not compile, so it proves nothing"
        } elseif ($touchedRust -and $output -notmatch 'Compiling protogine') {
            # Only for a Rust edit. A Luau control changes a file the binary
            # reads at run time, so cargo correctly rebuilds nothing and the
            # patched bundle is still what the test loads.
            $verdict = "$($control.Name): cargo did not rebuild, so the run is about a stale binary"
        } elseif ($output -notmatch 'running 1 test(?!s)') {
            $verdict = "$($control.Name): the run did not execute exactly one test, so it proves nothing"
        } elseif (-not $control.Passes -and $output -notmatch 'test result: FAILED') {
            $verdict = "$($control.Name): $($control.Test) did not fail with the rule removed"
        } elseif ($control.Passes) {
            if ($code -ne 0) { $verdict = "$($control.Name): recorded as redundant, but it failed at $where" }
            else { $note = "REDUNDANT RULE CONFIRMED: $($control.Name)" }
        } elseif ($code -eq 0) {
            $verdict = "$($control.Name): $($control.Test) still passed with the rule removed"
        } elseif (-not $control.Expect -and $output -notmatch [regex]::Escape($control.Marker)) {
            $verdict = "$($control.Name): failed at '$where', not '$($control.Marker)'"
        } else {
            $note = "CONTROL DETECTED: $($control.Name)"
        }

        if ($control.Expect) {
            # A self-test: the harness is supposed to refuse this one. Reaching a
            # verdict at all, or the wrong verdict, means a gate is not working.
            if (-not $verdict) {
                $controlFailures += "$($control.Name): the harness accepted a control it must refuse; the '$($control.Expect)' gate is not working"
            } elseif ($verdict -notmatch [regex]::Escape($control.Expect)) {
                $controlFailures += "$($control.Name): refused as '$verdict', not '$($control.Expect)'"
            } else {
                "SELF-TEST REFUSED AS EXPECTED: $($control.Name)"
            }
        } elseif ($verdict) {
            $controlFailures += $verdict
        } else {
            $note
        }
    }
} finally {
    Restore-ControlSources
}

$arguments = @('test', '--manifest-path', $controlManifest)
if ($Release) { $arguments += '--release' }
$arguments += $sample
$output = & cargo @arguments 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { $controlFailures += 'the suite did not return to green after restoring' }
"`nrestored: " + (($output -split "`r?`n" | Where-Object { $_ -match 'test result' }) -join '')

Exit-ControlLock -Path $controlLock

if ((Get-SourceFingerprint -Repo $controlRepo -Files $controlSources) -ne $controlFingerprint) {
    $controlFailures += 'the working tree changed during the run; controls must only ever patch the copy'
}

if ($controlFailures.Count -gt 0) {
    "`nUNCOVERED GUARDS:"
    $controlFailures | ForEach-Object { "  $_" }
    "`nFull cargo output for each: $controlLogs"
    exit 1
}
# Reported as its three parts rather than one total, because a single number
# invites a doc to say "N controls, M of them redundancies" and get the
# arithmetic between them wrong.
#
# `@(...)` around every filter, and it is not style. A pipeline that matches one
# item returns that item, not a one-element array; PowerShell then answers
# `.Count` with 1 only for an object that has no `Count` member of its own, and a
# hashtable has one - its number of keys. So a single matching control reports
# its own field count. That is how the redundancy below first reported itself as
# five: a measured, plausible number about the wrong object. The hazard is the
# element type, not the match count, so wrap the filter rather than reasoning
# about how many controls it can select.
$guards = @($controls | Where-Object { -not $_.Expect }).Count
$redundant = @($controls | Where-Object { $_.Passes }).Count
$selfTests = @($controls).Count - $guards
"`nAll $guards sample guards behaved as specified: $($guards - $redundant) detected with the" `
    + " rule removed and $redundant confirmed redundant. The harness refused all $selfTests self-tests."
