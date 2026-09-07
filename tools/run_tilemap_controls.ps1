# Negative controls for the kernel map guards.
#
# Each control removes one guard from the engine source, runs the single test
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
# Two controls are marked `Crash`. Removing those guards does not reach a test
# assertion at all; the library panics first on its own bounds check. That is
# weaker evidence than a test catching the mistake, so they are labelled rather
# than allowed to look like assertion controls.
#
# Every patch is written to an isolated copy of the tree under `target/`, never
# to the working tree, so a build that overlaps this run cannot pick up a
# deliberately broken source. The run proves that rather than asserting it: it
# fingerprints the engine sources before and after and fails if either moved.
#
# Isolation from the working tree is proven; isolation from concurrent `cargo`
# is not. See the header of `run_collision_controls.ps1`: re-run serially before
# believing a red result.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')
$controlSources = @('src/tilemap.rs', 'src/kernel.rs')
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources

$controls = @(
    # --- Self-tests: controls the harness must refuse ------------------------
    #
    # There is otherwise no control on the controls. Both of these used to be
    # accepted as ordinary results, and either would have reported every guard
    # as covered while proving nothing.

    @{ Name = 'self-test-missing-test'; File = 'src/tilemap.rs'
       Test = 'a_test_name_that_does_not_exist'; Marker = 'unused'
       Expect = 'did not execute exactly one test'
       Edits = @(@{ F = 'id != 0 && self.solids[id as usize - 1]'; R = 'id != 0' }) }

    @{ Name = 'self-test-inert-edit'; File = 'src/tilemap.rs'
       Test = 'row_major_ids_and_solidity'; Marker = 'unused'
       Expect = 'did not fail with the guard removed'
       Edits = @(@{ F = 'pub const MAX_REGION_CELLS: u32 = 4_096;'; R = 'pub const MAX_REGION_CELLS: u32 = 4_097;' }) }

    @{ Name = 'stop-keeps-map'; File = 'src/kernel.rs'; Test = 'stopping_releases_map_storage'
       Marker = 'stop must release map storage'
       Edits = @(@{ F = "self.active = false;`n        self.tilemap = None;"; R = 'self.active = false;' }) }

    @{ Name = 'no-saturation'; File = 'src/tilemap.rs'; Test = 'cell_lookup_saturates'
       Marker = 'must saturate one cell out'
       Edits = @(@{ F = 'guess.clamp(-1.0, f64::from(count)) as i32'; R = 'guess as i32' }) }

    # Floor and the exact-face correction are each sufficient alone, so this has
    # to remove both. In debug the library's own convergence assertion fires; in
    # release, where that is compiled out, the test's assertion does.
    @{ Name = 'truncate-without-correction'; File = 'src/tilemap.rs'; Test = 'rectangular_tiles_and_negative_origins'
       Marker = if ($Release) { 'left of a negative origin' } else { 'did not converge' }
       Edits = @(
           @{ F = 'let guess = ((world - origin) / tile).floor();'; R = 'let guess = ((world - origin) / tile).trunc();' }
           @{ F = 'for _ in 0..CELL_CORRECTIONS {'; R = 'for _ in 0..0 {' }) }

    # Deliberately still passes: it records that the two rules above are
    # redundant by design, which is why the control above removes both.
    @{ Name = 'correction-covers-truncation'; File = 'src/tilemap.rs'; Test = 'rectangular_tiles_and_negative_origins'
       Marker = 'PASSES'
       Edits = @(@{ F = 'let guess = ((world - origin) / tile).floor();'; R = 'let guess = ((world - origin) / tile).trunc();' }) }

    @{ Name = 'transposed-offset'; File = 'src/tilemap.rs'; Test = 'row_major_ids_and_solidity'
       Marker = 'tile ID at 1,0'
       Edits = @(@{ F = 'row as usize * self.info.columns as usize + column as usize'; R = 'column as usize * self.info.rows as usize + row as usize' }) }

    @{ Name = 'outside-not-solid'; File = 'src/tilemap.rs'; Test = 'outside_the_map_is_solid'
       Marker = 'must be solid'
       Edits = @(@{ F = "if !self.contains(column, row) {`n            return true;`n        }"; R = "if !self.contains(column, row) {`n            return false;`n        }" }) }

    @{ Name = 'nonzero-is-solid'; File = 'src/tilemap.rs'; Test = 'row_major_ids_and_solidity'
       Marker = 'solidity at 3,0'
       Edits = @(@{ F = 'id != 0 && self.solids[id as usize - 1]'; R = 'id != 0' }) }

    @{ Name = 'unbounded-region'; File = 'src/tilemap.rs'; Test = 'regions_copy_a_bounded_rectangle'; Crash = $true
       Marker = 'range end index 16 out of range for slice of length 15'
       Edits = @(@{ F = 'if far_column > i64::from(self.info.columns) || far_row > i64::from(self.info.rows) {'; R = 'if false {' }) }

    # In release the negative column becomes a huge usize and the slice range
    # rejects it; in debug the contains assertion inside `offset` gets there
    # first. Only Vec's own bounds checking stands behind this guard, which is
    # why it is worth controlling at all.
    @{ Name = 'region-negative-origin'; File = 'src/tilemap.rs'; Test = 'regions_copy_a_bounded_rectangle'; Crash = $true
       Marker = if ($Release) { 'range start index 18446744073709551615 out of range for slice of length 15' } else { 'assertion failed: self.contains(column, row)' }
       Edits = @(@{ F = 'if column < 0 || row < 0 {'; R = 'if false {' }) }

    @{ Name = 'empty-region-allowed'; File = 'src/tilemap.rs'; Test = 'regions_copy_a_bounded_rectangle'
       Marker = 'region must refuse an empty width'
       Edits = @(@{ F = 'if columns == 0 || rows == 0 {'; R = 'if false {' }) }

    @{ Name = 'uncapped-region'; File = 'src/tilemap.rs'; Test = 'regions_copy_a_bounded_rectangle'
       Marker = 'only the per-call cap can refuse a region this map contains'
       Edits = @(@{ F = 'if requested > u64::from(MAX_REGION_CELLS) {'; R = 'if false {' }) }

    @{ Name = 'unchecked-tile-id'; File = 'src/tilemap.rs'; Test = 'malformed_dimensions_ids_and_arrays'
       Marker = 'bad ID at'
       Edits = @(@{ F = 'if cells.iter().any(|id| *id > highest) {'; R = 'if false {' }) }

    @{ Name = 'unchecked-cell-product'; File = 'src/tilemap.rs'; Test = 'malformed_dimensions_ids_and_arrays'
       Marker = 'the cell product bound must refuse a 1024x1024 map'
       Edits = @(@{ F = 'if u64::from(self.columns) * u64::from(self.rows) > u64::from(MAX_CELLS) {'; R = 'if false {' }) }

    @{ Name = 'unchecked-dimension'; File = 'src/tilemap.rs'; Test = 'malformed_dimensions_ids_and_arrays'
       Marker = 'schema must refuse zero columns'
       Edits = @(@{ F = "if !(1..=MAX_DIMENSION).contains(&count) {"; R = 'if false {' }) }

    @{ Name = 'unchecked-tile-size'; File = 'src/tilemap.rs'; Test = 'malformed_dimensions_ids_and_arrays'
       Marker = 'schema must refuse zero tile width'
       Edits = @(@{ F = "if !(1..=MAX_TILE_SIZE).contains(&tile) {"; R = 'if false {' }) }

    @{ Name = 'no-geometry-limit'; File = 'src/tilemap.rs'; Test = 'faces_stay_exact_against_the_geometry_limit'
       Marker = 'one pixel past the geometry limit must be refused'
       Edits = @(@{ F = 'if origin < -GEOMETRY_LIMIT || far > GEOMETRY_LIMIT {'; R = 'if false {' }) }

    # Two rules now refuse an out-of-bounds edit on the kernel path, so this has
    # to remove both. Phase 2 gave `Kernel::set_tile` a read of the previous ID,
    # to decide whether an edit turns a cell solid, and that read refuses the
    # coordinates before `TileMap::set_tile` is ever reached.
    @{ Name = 'edit-skips-bounds'; File = 'src/tilemap.rs'; Test = 'single_cell_edits_apply_immediately'; Crash = $true
       Marker = if ($Release) { 'index out of bounds: the len is 15 but the index is 18446744073709551615' } else { 'assertion failed: self.contains(column, row)' }
       Edits = @(
           @{ F = "if !self.contains(column, row) {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()"; R = "if false {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()" }
           @{ File = 'src/kernel.rs'; F = '        let previous = map.tile(column, row)?;'; R = '        let previous = map.tile(column, row).unwrap_or(0);' }) }

    # Deliberately still passes: it records that the kernel's own precheck now
    # covers the edit bounds by itself, which is why the control above removes
    # both rules rather than only `TileMap::set_tile`'s.
    @{ Name = 'kernel-precheck-covers-edit-bounds'; File = 'src/tilemap.rs'
       Test = 'single_cell_edits_apply_immediately'; Marker = 'PASSES'
       Edits = @(@{ F = "if !self.contains(column, row) {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()"; R = "if false {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()" }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'tilemap-controls'
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
        $arguments += @('--test', 'tilemap', '--no-default-features', '--', $control.Test)
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

        $where = ($output -split "`r?`n" | Where-Object { $_ -match 'panicked at|assertion' } | Select-Object -First 2) -join ' | '
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
        } elseif ($output -notmatch 'Compiling protogine') {
            # Every control edits a source file, so a correct run must rebuild.
            # If cargo decided the crate was up to date, it ran a binary built
            # from different code and the result is about that binary, not this
            # control. Observed under concurrent `cargo` load.
            $verdict = "$($control.Name): cargo did not rebuild, so the run is about a stale binary"
        } elseif ($output -notmatch 'running 1 test(?!s)') {
            $verdict = "$($control.Name): the run did not execute exactly one test, so it proves nothing"
        } elseif ($control.Marker -ne 'PASSES' -and $output -notmatch 'test result: FAILED') {
            $verdict = "$($control.Name): $($control.Test) did not fail with the guard removed"
        } elseif ($control.Marker -eq 'PASSES') {
            if ($code -ne 0) { $verdict = "$($control.Name): expected to still pass, but failed at $where" }
            else { $note = "REDUNDANT GUARD CONFIRMED: $($control.Name)" }
        } elseif ($code -eq 0) {
            $verdict = "$($control.Name): $($control.Test) still passed with the guard removed"
        } elseif ($output -notmatch [regex]::Escape($control.Marker)) {
            $verdict = "$($control.Name): failed at '$where', not '$($control.Marker)'"
        } elseif ($control.Crash) {
            $note = "CRASH CONTROL DETECTED: $($control.Name) -> $where"
        } else {
            $note = "CONTROL DETECTED: $($control.Name) -> $where"
        }

        if ($control.Expect) {
            # A self-test: the harness is supposed to refuse this one. Reaching a
            # verdict at all, or the wrong verdict, means a gate is not working.
            if (-not $verdict) {
                $controlFailures += "$($control.Name): the harness accepted a control it must refuse"
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
$arguments += @('--test', 'tilemap', '--no-default-features')
$output = & cargo @arguments 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { $controlFailures += 'the suite did not return to green after restoring' }
"`nrestored: " + (($output -split "`r?`n" | Where-Object { $_ -match 'test result' }) -join '')

if ((Get-SourceFingerprint -Repo $controlRepo -Files $controlSources) -ne $controlFingerprint) {
    $controlFailures += 'the working tree changed during the run; controls must only ever patch the copy'
}

if ($controlFailures.Count -gt 0) {
    "`nUNCOVERED GUARDS:"
    $controlFailures | ForEach-Object { "  $_" }
    exit 1
}
$guards = ($controls | Where-Object { -not $_.Expect }).Count
$selfTests = $controls.Count - $guards
"`nAll $guards map guards behaved as specified, and the harness refused all $selfTests self-tests."
