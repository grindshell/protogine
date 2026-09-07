# Negative controls for the collider and swept-solver guards.
#
# Each control removes one rule from the engine source, runs the single test
# that is supposed to catch it, and requires that test to fail at a named
# assertion. A guard that cannot be observed failing is not covered, and a
# passing suite alone does not distinguish the two.
#
# Markers name the exact assertion, never a generic 'assertion' or 'panicked'
# substring: those match any failure at all, which would reduce the harness to
# "something broke" and silently absorb a control that moved to a different
# failure site. Where a control fails somewhere else in release, both markers
# are listed and the profile selects.
#
# Controls marked `Passes` are expected to keep passing. They record a rule that
# is redundant given something else, and say which something, so a later change
# that removes the other half is visibly uncovered rather than silently so. A
# control marked `Crash` reaches no test assertion at all: the library panics on
# its own check first, which is weaker evidence than a test catching the mistake,
# so it is labelled rather than allowed to look like the others.
#
# Every patch is written to an isolated copy of the tree under `target/`, never
# to the working tree, so a build that overlaps this run cannot pick up a
# deliberately broken source. The run proves that rather than asserting it: it
# fingerprints the engine sources before and after and fails if either moved.
#
# Isolation from the working tree is proven; isolation from concurrent `cargo`
# is not. Under heavy parallel cargo load a control has been observed reporting
# uncovered when a serial run on the same tree passes all of them. The mechanism
# is unidentified. Every observed instance has been in the safe direction - the
# harness cried wolf rather than passing a control that had not applied - and
# each conclusion is now separately gated on the patch surviving the run, the
# crate actually recompiling, exactly one test executing, and a detecting
# control's test reporting failure. Re-run serially before believing a red
# result, and read the saved cargo output for the control that failed.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')
$controlSources = @('src/collision.rs', 'src/kernel.rs')
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources

$sweepPair = @'
    let x = sweep(
        map,
        collider,
        Axis::X,
        position.0,
        position.1,
        travel.0,
        work,
    )?;
    let y = sweep(map, collider, Axis::Y, x, position.1, travel.1, work)?;
'@ -replace "`r`n", "`n"

$validatePass = @'
        for (position, velocity) in self.world.query::<(&Position, &Velocity)>().iter() {
            finite(
                position.x + velocity.x * FIXED_DT,
                position.y + velocity.y * FIXED_DT,
            )?;
        }
'@ -replace "`r`n", "`n"

$controls = @(
    # --- The four controls the plan names for this phase ----------------------

    @{ Name = 'endpoint-only'; File = 'src/collision.rs'
       Test = 'travel_across_many_tiles_stops_at_a_one_cell_wall'
       Marker = 'high-speed wall stop: the body must stop at 256, not tunnel past 288'
       Edits = @(@{ F = $sweepPair; R = @'
    let mut x = position.0;
    if check_placement(map, collider, x + travel.0, position.1, work).is_ok() {
        x += travel.0;
    }
    let mut y = position.1;
    if check_placement(map, collider, x, y + travel.1, work).is_ok() {
        y += travel.1;
    }
'@ -replace "`r`n", "`n" }) }

    @{ Name = 'corner-only-sweep'; File = 'src/collision.rs'
       Test = 'an_interior_solid_cell_cannot_hide_between_clear_corners'
       Marker = 'the interior solid cell at column 3 must stop the body at face 96'
       Edits = @(@{ F = '    for across in first..=last {'; R = '    for across in [first, last] {' }) }

    @{ Name = 'corner-only-placement'; File = 'src/collision.rs'
       Test = 'an_interior_solid_cell_cannot_hide_between_clear_corners'
       Marker = 'placement must see the interior solid column, not only the outer two'
       Edits = @(@{ F = '    for column in first..=last {'; R = '    for column in [first, last] {' }) }

    @{ Name = 'y-before-x'; File = 'src/collision.rs'
       Test = 'the_diagonal_corner_resolves_x_before_y'
       Marker = 'x-before-y corner: X resolves fully, then Y is blocked from the resolved X'
       Edits = @(@{ F = $sweepPair; R = @'
    let y = sweep(
        map,
        collider,
        Axis::Y,
        position.0,
        position.1,
        travel.1,
        work,
    )?;
    let x = sweep(map, collider, Axis::X, position.0, y, travel.0, work)?;
'@ -replace "`r`n", "`n" }) }

    @{ Name = 'partial-integration-commit'; File = 'src/kernel.rs'
       Test = 'a_late_refusal_moves_no_entity'
       Marker = 'an entity validated before the refusal must not have been committed'
       Edits = @(@{ F = $validatePass; R = @'
        for (position, velocity) in self.world.query_mut::<(&mut Position, &Velocity)>() {
            let next = (
                position.x + velocity.x * FIXED_DT,
                position.y + velocity.y * FIXED_DT,
            );
            finite(next.0, next.1)?;
            position.x = next.0;
            position.y = next.1;
        }
'@ -replace "`r`n", "`n" }) }

    # --- Work accounting -----------------------------------------------------
    #
    # The budget is both the termination guard and the anti-probing guard, so
    # neither the charging nor its ordering may go unobserved.

    @{ Name = 'cells-visited-uncharged'; File = 'src/collision.rs'
       Test = 'tile_work_is_charged_per_visited_cell_and_survives_a_refusal'
       Marker = 'a 32-pixel box offset by one pixel covers four 32-pixel cells'
       Edits = @(@{ F = '        work.charge(1)?;'; R = '' }) }

    @{ Name = 'charge-after-the-refusal'; File = 'src/collision.rs'
       Test = 'the_solver_refuses_rather_than_overrunning_its_budget'
       Marker = 'the work performed is charged before the refusal'
       Edits = @(@{ F = "        self.used = self.used.saturating_add(units);`n        if self.used > self.limit {`n            return Err(CollisionError::Work);`n        }`n        Ok(())"
                    R = "        if self.used.saturating_add(units) > self.limit {`n            return Err(CollisionError::Work);`n        }`n        self.used = self.used.saturating_add(units);`n        Ok(())" }) }

    # --- Numerical rules -----------------------------------------------------

    @{ Name = 'naive-clamp'; File = 'src/collision.rs'
       Test = 'the_clamped_box_stays_clear_of_the_boundary_face'
       Marker = 'clamped box on the free side: reconstructed 16777216.000000004 past 16777216'
       Edits = @(@{ F = '    while (position + offset) + size > face {'; R = '    while false {' }) }

    @{ Name = 'naive-clamp-mirror'; File = 'src/collision.rs'
       Test = 'the_clamp_postcondition_holds_moving_toward_the_low_boundary'
       Marker = 'clamped box on the free side: -16777215.000000002 below -16777215'
       Edits = @(@{ F = '    while position + offset < face {'; R = '    while false {' }) }

    @{ Name = 'closed-interval-overlap'; File = 'src/collision.rs'
       Test = 'a_solid_edit_under_a_body_refuses_and_leaves_the_cell'
       Marker = 'a body flush against the cell''s face does not overlap it'
       Edits = @(@{ F = 'if !(body.low(axis) < map.face(axis, index + 1) && body.high(axis) > map.face(axis, index))'
                    R = 'if !(body.low(axis) <= map.face(axis, index + 1) && body.high(axis) >= map.face(axis, index))' }) }

    @{ Name = 'span-past-the-far-face'; File = 'src/collision.rs'
       Test = 'a_touching_wall_blocks_only_movement_into_it'
       Marker = 'edge contact is free: a flush box must not overlap the cell beyond it'
       Edits = @(@{ F = '    if map.face(axis, last) >= high {'; R = '    if false {' }) }

    @{ Name = 'skip-the-touching-face'; File = 'src/collision.rs'
       Test = 'a_touching_wall_blocks_only_movement_into_it'
       Marker = 'moving into a touching face yields zero displacement, never a push'
       Edits = @(@{ F = '        if map.face(axis, index) < lead {'; R = '        if map.face(axis, index) <= lead {' }) }

    @{ Name = 'embedded-start-retreats'; File = 'src/collision.rs'
       Test = 'a_body_already_past_its_blocking_face_faults_instead_of_retreating'
       Marker = 'a start beyond the blocking face must fault, never retreat to itself'
       Edits = @(@{ F = "        if position <= start {`n            return Err(CollisionError::Embedded);`n        }"
                    R = "        if position <= start {`n            return Ok(start);`n        }" }) }

    # This was a recorded redundancy until the test that its own comment
    # described got written. Saturation plus "outside the map is solid" does
    # refuse a merely out-of-range box, so removing the check changed nothing
    # observable; what it also refuses, and they do not, is a NaN or inverted
    # edge. Writing that case turned a recorded redundancy into coverage, which
    # is strictly better than recording it.
    @{ Name = 'placement-skips-the-extent-check'; File = 'src/collision.rs'
       Test = 'public_geometry_entry_points_answer_rather_than_diverging_by_profile'
       Marker = 'a NaN edge must be refused, not converted to a cell index'
       Edits = @(@{ F = "        if !(body.low(axis) >= map.low(axis)`n            && body.high(axis) <= map.high(axis)`n            && body.low(axis) <= body.high(axis))`n        {"
                    R = "        if false {" }) }

    # The inverted-edge half alone, so the two clauses are covered separately
    # rather than only jointly.
    @{ Name = 'placement-accepts-an-inverted-box'; File = 'src/collision.rs'
       Test = 'public_geometry_entry_points_answer_rather_than_diverging_by_profile'
       Marker = 'a -inf edge must be refused, not converted to a cell index'
       Edits = @(@{ F = '            && body.low(axis) <= body.high(axis))'; R = '            && true)' }) }

    @{ Name = 'nonfinite-travel-accepted'; File = 'src/collision.rs'
       Test = 'public_geometry_entry_points_answer_rather_than_diverging_by_profile'
       Marker = 'travel (NaN, 0.0) must be refused'
       Edits = @(@{ F = "    if !(x.is_finite() && y.is_finite() && travel.is_finite()) {`n        return Err(CollisionError::Nonfinite);`n    }"
                    R = "    if false {`n        return Err(CollisionError::Nonfinite);`n    }" }) }

    # Observable in debug only, and marked so rather than hidden. Without the
    # guard `index + 1` overflows at `i32::MAX`, which debug catches; in release
    # the wrap lands on a face so far outside the map that the comparison still
    # answers correctly for these coordinates, so the control is redundant there.
    @{ Name = 'cell-overlap-skips-bounds'; File = 'src/collision.rs'
       Test = 'public_geometry_entry_points_answer_rather_than_diverging_by_profile'
       Crash = $true; Passes = $Release
       Marker = 'attempt to add with overflow'
       Edits = @(@{ F = "        if index < 0 || index >= map.info().count(axis) as i32 {`n            return false;`n        }"
                    R = "        if false {`n            return false;`n        }" }) }

    # --- T5 and T6 guards ----------------------------------------------------

    @{ Name = 'teleport-unchecked'; File = 'src/kernel.rs'
       Test = 'a_teleport_crosses_a_wall_but_never_lands_in_one'
       Marker = 'teleporting to (64.0, 64.0) must be refused'
       Edits = @(@{ F = "            placement?;`n        }`n        *self"; R = "            let _ = placement;`n        }`n        *self" }) }

    @{ Name = 'install-unrevalidated'; File = 'src/kernel.rs'
       Test = 'a_map_replacement_that_would_trap_a_body_refuses_without_changing_the_map'
       Marker = 'installation must revalidate every live collider before the swap'
       Edits = @(@{ F = '        refusal?;'; R = '        let _ = refusal;' }) }

    @{ Name = 'clear-ignores-colliders'; File = 'src/kernel.rs'
       Test = 'the_map_can_be_cleared_only_once_every_collider_is_detached'
       Marker = 'T6 refuses to clear the map beneath a body'
       Edits = @(@{ F = '        if self.colliders > 0 {'; R = '        if false {' }) }

    @{ Name = 'edit-ignores-bodies'; File = 'src/kernel.rs'
       Test = 'a_solid_edit_under_a_body_refuses_and_leaves_the_cell'
       Marker = 'an edit that would trap the body must refuse'
       Edits = @(@{ F = '        if becomes_solid && outcome.is_ok() {'; R = '        if false {' }) }

    @{ Name = 'edit-scans-every-change'; File = 'src/kernel.rs'
       Test = 'tile_work_is_charged_per_visited_cell_and_survives_a_refusal'
       Marker = 'a solid cell that stays solid must skip the collider scan'
       Edits = @(@{ F = "        let previous = map.tile(column, row)?;`n        let becomes_solid = map.solid_definition(id)? && !map.solid_definition(previous)?;"
                    R = "        let _previous = map.tile(column, row)?;`n        let becomes_solid = map.solid_definition(id)?;" }) }

    @{ Name = 'attach-unchecked-extent'; File = 'src/kernel.rs'
       Test = 'attaching_and_resizing_check_every_covered_cell'
       Marker = 'attaching must refuse an extent beyond eight tiles'
       Edits = @(@{ F = '        collider.check(&map.info())?;'; R = '        let _ = collider.check(&map.info());' }) }

    @{ Name = 'attach-unchecked-placement'; File = 'src/kernel.rs'
       Test = 'attaching_and_resizing_check_every_covered_cell'
       Marker = 'attaching must check every cell the box covers'
       Edits = @(@{ F = "        placement?;`n        self.world`n            .insert_one(entity, collider)"
                    R = "        let _ = placement;`n        self.world`n            .insert_one(entity, collider)" }) }

    @{ Name = 'collider-limit-ignored'; File = 'src/kernel.rs'
       Test = 'the_collider_limit_is_enforced_and_released'
       Marker = 'the live collider limit must refuse the next attachment'
       Edits = @(@{ F = '        if !attached && self.colliders >= MAX_LIVE_COLLIDERS {'; R = '        if false {' }) }

    @{ Name = 'stop-keeps-collider-count'; File = 'src/kernel.rs'
       Test = 'stopping_releases_the_sweep_scratch'
       Marker = 'all three accounting accessors must agree that a stopped session holds nothing'
       Edits = @(@{ F = '        self.colliders = 0;'; R = '' }) }

    @{ Name = 'despawn-leaks-capacity'; File = 'src/kernel.rs'
       Test = 'the_collider_limit_is_enforced_and_released'
       Marker = 'despawn must release collider capacity'
       Edits = @(@{ F = '        if had_collider {'; R = '        if false && had_collider {' }) }

    # --- Redundancies, recorded rather than mistaken for coverage ------------

    # Redundant by design: bodies never affect one another, so the order the
    # pass resolves them in cannot change any result. Nor is the difference
    # observable through the API - a work or invariant refusal carries no entity
    # - so the sort buys internal reproducibility while debugging, not a
    # behavioural guarantee. It is kept because it is free and correct, and
    # recorded here rather than claimed as coverage.
    @{ Name = 'unsorted-bodies'; File = 'src/kernel.rs'
       Test = 'replay_is_independent_of_insertion_order'; Passes = $true
       Edits = @(@{ F = '        bodies.sort_unstable_by_key(|candidate| candidate.entity.id());'; R = '' }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'collision-controls'
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
# A failing control's full cargo output, saved rather than summarised. A run that
# disagrees with a serial one is a finding about the harness, and it cannot be
# diagnosed from a one-line summary after the fact.
$controlLogs = Join-Path $controlRepo 'target\collision-controls\failures'
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
        foreach ($edit in $control.Edits) {
            $file = if ($edit.File) { $edit.File } else { $control.File }
            if (-not $patched[$file].Contains($edit.F)) { $missing = $true; break }
            $patched[$file] = $patched[$file].Replace($edit.F, $edit.R)
        }
        if ($missing) {
            $controlFailures += "$($control.Name): anchor no longer matches $($control.File); the control is stale, not the code"
            continue
        }

        foreach ($file in $controlSources) {
            [IO.File]::WriteAllText((Join-Path $controlTree $file), $patched[$file], (New-Object Text.UTF8Encoding $false))
        }
        $arguments = @('test', '--manifest-path', $controlManifest)
        if ($Release) { $arguments += '--release' }
        $arguments += @('--test', 'collision', '--no-default-features', '--', $control.Test)
        $output = & cargo @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
        # Read back before restoring: the conclusion is only about this control
        # if the file cargo compiled still carried the mutation.
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
        # rebuild. Both would read as "the guard is dead" or "the guard is live"
        # at random, which is worse than no harness. Require the run to say it
        # executed exactly one test, and a detecting control to say it failed.
        if (-not $applied) {
            $controlFailures += "$($control.Name): the patched source changed under the run, so it proves nothing"
        } elseif ($output -match 'error\[E\d+\]|could not compile') {
            $controlFailures += "$($control.Name): did not compile, so it proves nothing"
        } elseif ($output -notmatch 'Compiling protogine') {
            # Every control edits a source file, so a correct run must rebuild.
            # If cargo decided the crate was up to date, it ran a binary built
            # from different code and the result is about that binary, not this
            # control. Observed under concurrent `cargo` load.
            $controlFailures += "$($control.Name): cargo did not rebuild, so the run is about a stale binary"
        } elseif ($output -notmatch 'running 1 test(?!s)') {
            $controlFailures += "$($control.Name): the run did not execute exactly one test, so it proves nothing"
        } elseif (-not $control.Passes -and $output -notmatch 'test result: FAILED') {
            $controlFailures += "$($control.Name): $($control.Test) did not fail with the rule removed"
        } elseif ($control.Passes) {
            if ($code -ne 0) { $controlFailures += "$($control.Name): recorded as redundant, but it failed at $where" }
            else { "REDUNDANT RULE CONFIRMED: $($control.Name)" }
        } elseif ($code -eq 0) {
            $controlFailures += "$($control.Name): $($control.Test) still passed with the rule removed"
        } elseif ($output -notmatch [regex]::Escape($control.Marker)) {
            $controlFailures += "$($control.Name): failed at '$where', not '$($control.Marker)'"
        } elseif ($control.Crash) {
            # The library panicked on its own check before a test assertion was
            # reached. Weaker evidence than a test catching the mistake, so it is
            # labelled rather than allowed to look like the others.
            "CRASH CONTROL DETECTED: $($control.Name) -> $where"
        } else {
            "CONTROL DETECTED: $($control.Name)"
        }
    }
} finally {
    Restore-ControlSources
}

$arguments = @('test', '--manifest-path', $controlManifest)
if ($Release) { $arguments += '--release' }
$arguments += @('--test', 'collision', '--no-default-features')
$output = & cargo @arguments 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { $controlFailures += 'the suite did not return to green after restoring' }
"`nrestored: " + (($output -split "`r?`n" | Where-Object { $_ -match 'test result' }) -join '')

if ((Get-SourceFingerprint -Repo $controlRepo -Files $controlSources) -ne $controlFingerprint) {
    $controlFailures += 'the working tree changed during the run; controls must only ever patch the copy'
}

if ($controlFailures.Count -gt 0) {
    "`nUNCOVERED GUARDS:"
    $controlFailures | ForEach-Object { "  $_" }
    "`nFull cargo output for each: $controlLogs"
    "A red result from a run that overlapped another cargo invocation is worth"
    "re-running serially before it is believed; see the header."
    exit 1
}
"`nAll $($controls.Count) collision guards behaved as specified."
