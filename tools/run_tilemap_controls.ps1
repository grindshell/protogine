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
# Source files are restored after every control and again on any failure or
# interruption, and the run refuses to start on a dirty working tree so a
# restore can never be mistaken for the author's own edit.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
$controlSources = @('src/tilemap.rs', 'src/kernel.rs')

$controlDirty = & git -C $controlRepo status --porcelain -- $controlSources
if ($controlDirty) {
    throw "Uncommitted changes in $($controlSources -join ', '); commit or stash them so a restore cannot lose work."
}

$controls = @(
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

    @{ Name = 'edit-skips-bounds'; File = 'src/tilemap.rs'; Test = 'single_cell_edits_apply_immediately'; Crash = $true
       Marker = if ($Release) { 'index out of bounds: the len is 15 but the index is 18446744073709551615' } else { 'assertion failed: self.contains(column, row)' }
       Edits = @(@{ F = "if !self.contains(column, row) {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()"; R = "if false {`n            return Err(TileMapError::Bounds);`n        }`n        if id > self.highest_id()" }) }
)

$controlOriginals = @{}
foreach ($file in $controlSources) {
    $controlOriginals[$file] = [IO.File]::ReadAllText((Join-Path $controlRepo $file))
}
function Restore-ControlSources {
    foreach ($file in $controlSources) {
        [IO.File]::WriteAllText((Join-Path $controlRepo $file), $controlOriginals[$file], (New-Object Text.UTF8Encoding $false))
    }
}

$controlFailures = @()
try {
    foreach ($control in $controls) {
        $path = Join-Path $controlRepo $control.File
        $patched = $controlOriginals[$control.File]
        $missing = $false
        foreach ($edit in $control.Edits) {
            if (-not $patched.Contains($edit.F)) { $missing = $true; break }
            $patched = $patched.Replace($edit.F, $edit.R)
        }
        if ($missing) {
            $controlFailures += "$($control.Name): anchor no longer matches $($control.File); the control is stale, not the code"
            continue
        }

        [IO.File]::WriteAllText($path, $patched, (New-Object Text.UTF8Encoding $false))
        $arguments = @('test')
        if ($Release) { $arguments += '--release' }
        $arguments += @('--test', 'tilemap', '--no-default-features', '--', $control.Test)
        $output = & cargo @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
        Restore-ControlSources

        $where = ($output -split "`r?`n" | Where-Object { $_ -match 'panicked at|assertion' } | Select-Object -First 2) -join ' | '
        if ($output -match 'error\[E\d+\]|could not compile') {
            $controlFailures += "$($control.Name): did not compile, so it proves nothing"
        } elseif ($control.Marker -eq 'PASSES') {
            if ($code -ne 0) { $controlFailures += "$($control.Name): expected to still pass, but failed at $where" }
            else { "REDUNDANT GUARD CONFIRMED: $($control.Name)" }
        } elseif ($code -eq 0) {
            $controlFailures += "$($control.Name): $($control.Test) still passed with the guard removed"
        } elseif ($output -notmatch [regex]::Escape($control.Marker)) {
            $controlFailures += "$($control.Name): failed at '$where', not '$($control.Marker)'"
        } elseif ($control.Crash) {
            "CRASH CONTROL DETECTED: $($control.Name) -> $where"
        } else {
            "CONTROL DETECTED: $($control.Name) -> $where"
        }
    }
} finally {
    Restore-ControlSources
}

$arguments = @('test')
if ($Release) { $arguments += '--release' }
$arguments += @('--test', 'tilemap', '--no-default-features')
$output = & cargo @arguments 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { $controlFailures += 'the suite did not return to green after restoring' }
"`nrestored: " + (($output -split "`r?`n" | Where-Object { $_ -match 'test result' }) -join '')

$controlDirty = & git -C $controlRepo status --porcelain -- $controlSources
if ($controlDirty) { $controlFailures += "sources were not restored cleanly: $controlDirty" }

if ($controlFailures.Count -gt 0) {
    "`nUNCOVERED GUARDS:"
    $controlFailures | ForEach-Object { "  $_" }
    exit 1
}
"`nAll $($controls.Count) map guards behaved as specified."
