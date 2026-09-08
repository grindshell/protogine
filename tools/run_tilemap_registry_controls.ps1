# Negative controls for the M2 Phase 1 map registry.
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
# Every rule here lives in a `.rs` source, so unlike the sprites harness all four
# gates apply to every control - including the rebuild gate, since a binary built
# from unpatched code is a reachable way to report a live guard as dead.
#
# These are unit tests inside the library, so the run is `cargo test --lib`
# rather than `--test <name>`. That is not incidental. Two of the fixtures below
# read `TileMapHandle::slot`, which is `#[cfg(test)]` and `pub(crate)`: M2-1
# makes slot *reuse* a contract clause the fixture has to prove it forced, and
# does not make the slot *number* something a game observes. An integration test
# could not see it, so the fixtures live where the visibility does.
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
    'src/maps.rs',
    'src/kernel.rs'
)
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources
# Claimed before the copy exists, because the copy is what two runs would share.
# Released after the fingerprint check below, so both exits pass through it.
$controlLock = Enter-ControlLock -Repo $controlRepo -Name 'tilemap-registry-controls'

# Core configuration: the registry is engine state, not a scripting or graphics
# concern, and Phase 1's exit gate names core as the configuration it must hold
# in. Nothing here needs a decoder, a VM or a window.
$suite = @('--lib', '--no-default-features')
# Three controls target the public round trip instead, which lives in an
# integration test because its whole point is being outside the crate. A control
# names its own suite rather than the harness guessing from the test path.
$publicSuite = @('--test', 'tilemap_registry', '--no-default-features')

# `validate_map` with the session and identity checks moved ahead of the
# liveness check. Written out in full because the rule being removed is an
# *ordering* rather than a line: every check still runs, and only which one
# answers first changes.
$identityFirst = @'
    fn validate_map(&self, handle: &TileMapHandle) -> Result<TileMapId, KernelError> {
        if !Rc::ptr_eq(&self.session, &handle.session) {
            return Err(KernelError::InvalidTileMap);
        }
        self.maps.get(handle.id).map_err(KernelError::from_table)?;
        self.require_active()?;
        Ok(handle.id)
    }
'@
$activeFirst = @'
    fn validate_map(&self, handle: &TileMapHandle) -> Result<TileMapId, KernelError> {
        self.require_active()?;
        if !Rc::ptr_eq(&self.session, &handle.session) {
            return Err(KernelError::InvalidTileMap);
        }
        self.maps.get(handle.id).map_err(KernelError::from_table)?;
        Ok(handle.id)
    }
'@

$controls = @(
    # --- Identity: a handle names one map of one session ----------------------

    # M2's required control. The generation is what separates "removed" from
    # "removed and its slot handed to someone else", and only the second is
    # reachable once a free list exists.
    @{ Name = 'generation-unchecked'; File = 'src/maps.rs'
       Test = 'kernel::tests::a_reused_slot_refuses_the_handle_it_displaced'
       Marker = 'a handle to a removed map must refuse after its slot is reused'
       Edits = @(@{ F = 'Some(slot) if slot.generation == id.generation && slot.map.is_some() => Ok(index),'
                    R = 'Some(slot) if slot.map.is_some() => Ok(index),' }) }

    # The review session's, and it ranks above two of the others. `TileMapId`
    # carries no session, so two kernels holding one map each name it
    # identically; without `Rc::ptr_eq` a foreign handle reads whatever occupies
    # that slot here rather than refusing.
    @{ Name = 'session-unchecked'; File = 'src/kernel.rs'
       Test = 'kernel::tests::a_foreign_handle_refuses_against_a_map_holding_the_same_slot'
       Marker = 'a handle from another session must refuse, not read the slot it names'
       Edits = @(@{ F = @'
        if !Rc::ptr_eq(&self.session, &handle.session) {
            return Err(KernelError::InvalidTileMap);
        }
        self.maps.get(handle.id).map_err(KernelError::from_table)?;
'@
                    R = @'
        self.maps.get(handle.id).map_err(KernelError::from_table)?;
'@ }) }

    # M2-1 fixes the mechanism, not only the outcome: after `stop` every entry
    # point refuses `Inactive` *before* a slot is examined. Reordering the same
    # three checks makes a live handle answer `InvalidTileMap` instead, because
    # the table has been released. Nothing else about the function changes.
    @{ Name = 'stop-refuses-by-identity'; File = 'src/kernel.rs'
       Test = 'kernel::tests::stopping_refuses_by_session_where_removal_refuses_by_identity'
       Marker = 'a stopped session refuses before any slot is examined'
       Edits = @(@{ F = $activeFirst; R = $identityFirst }) }

    # --- Allocation: the free list order is a contract clause -----------------

    # The one that proves the reuse fixtures test generations rather than an
    # unrecycled slot. Under first-in-first-out the landing assertion fails
    # before any stale handle is presented.
    @{ Name = 'free-list-fifo'; File = 'src/maps.rs'
       Test = 'maps::tests::reuse_takes_the_most_recently_freed_slot'
       Marker = 'the most recently freed slot must be handed out first'
       Edits = @(@{ F = 'let slot = match self.free.pop() {'
                    R = "let slot = match if self.free.is_empty() { None } else { Some(self.free.remove(0)) } {" }) }

    # --- Budgets: admission subtracts what it replaces ------------------------

    # `live + candidate` rather than `live - replaced + candidate`. A session
    # using the budget it was granted could then never replace a map, including
    # with an identical one.
    @{ Name = 'admission-ignores-the-outgoing-map'; File = 'src/maps.rs'
       Test = 'maps::tests::replacement_charges_the_difference_rather_than_the_whole_candidate'
       Marker = 'an equal-sized replacement fits a full aggregate'
       Edits = @(@{ F = 'self.admit(cells, outgoing)?;'; R = 'self.admit(cells, 0)?;' }) }

    # --- M2-R1: every scan is scoped to the edited map's own members ----------
    #
    # **These four were re-earned in Phase 2 and their anchors moved.** In Phase
    # 1 the scoping predicate was `current == target`, because only the implicit
    # map could have members; now it is the body's own membership. The fixture
    # they target moved with them, from one body on one map to a member on each
    # of two maps and a third with none - without that, "scan the edited map's
    # members" and "scan every collider" are the same scan and all four would
    # keep passing against an implementation that had reverted to a global one.
    # The prior is the review session's.

    # The inherited mistake, and the contract says it matters more than its
    # siblings for exactly that reason: `set_tile`'s global collider query is
    # correct M1 code, so an implementer reading it has no local reason to touch
    # it. Scanning every collider makes an edit to one map refuse because of a
    # body standing at the same world coordinates on another.
    @{ Name = 'cell-edit-scans-all-colliders'; File = 'src/kernel.rs'
       Test = 'kernel::tests::removing_and_replacing_an_unrelated_map_ignores_another_maps_bodies'
       Marker = 'a cell edit must ignore bodies on another map'
       Edits = @(@{ F = @'
                if membership.0 != target {
                    continue;
                }
'@
                    R = '' }) }

    # Its mirror, and the reason the fixture pairs every map-local success with
    # the same operation on a map that does have a member refusing. Scanning
    # nobody makes the map-local assertion above pass for the wrong reason; only
    # the paired one catches it.
    @{ Name = 'cell-edit-scans-nobody'; File = 'src/kernel.rs'
       Test = 'kernel::tests::removing_and_replacing_an_unrelated_map_ignores_another_maps_bodies'
       Marker = 'while a body on the edited map still refuses'
       Edits = @(@{ F = 'if membership.0 != target {'
                    R = 'if true {' }) }

    @{ Name = 'replacement-revalidates-every-map'; File = 'src/kernel.rs'
       Test = 'kernel::tests::removing_and_replacing_an_unrelated_map_ignores_another_maps_bodies'
       Marker = 'replacing a map with no members must revalidate nobody'
       Edits = @(@{ F = @'
            if membership.0 != id {
                continue;
            }
'@
                    R = '' }) }

    @{ Name = 'removal-scans-all-colliders'; File = 'src/kernel.rs'
       Test = 'kernel::tests::removing_and_replacing_an_unrelated_map_ignores_another_maps_bodies'
       Marker = "removing a map with no members must not see anyone else's"
       Edits = @(@{ F = 'if self.has_members(id) {'
                    R = 'if self.colliders > 0 {' }) }

    # --- The public surface: reachable, and each accessor its own quantity ----

    # M2-4 keeps a handle valid across a replacement, which is the whole of what
    # separates it from remove-then-create. Nothing else in the phase would fail
    # if `replace` advanced the generation.
    @{ Name = 'replacement-invalidates-its-handle'; File = 'src/maps.rs'; Suite = $publicSuite
       Test = 'a_map_is_created_read_replaced_and_removed_through_its_handle'
       Marker = 'a replaced map must still answer to the handle that named it'
       Edits = @(@{ F = @'
        self.slots[index].map = Some(map);
        self.cells = self.cells - outgoing as u32 + cells as u32;
'@
                    R = @'
        self.slots[index].map = Some(map);
        self.slots[index].generation += 1;
        self.cells = self.cells - outgoing as u32 + cells as u32;
'@ }) }

    # The two accessors that had no call site anywhere before the public file
    # existed. Both delegate to adjacent methods on one table, so a copy-paste
    # between them is the realistic mistake, and only distinct values catch it.
    @{ Name = 'table-bytes-reports-the-maps'; File = 'src/kernel.rs'; Suite = $publicSuite
       Test = 'each_accounting_accessor_reports_the_quantity_it_names'
       Marker = "the table's overhead is its slots, not its maps"
       Edits = @(@{ F = 'self.maps.table_bytes()'; R = 'self.maps.storage_bytes()' }) }

    @{ Name = 'cells-reports-the-map-count'; File = 'src/kernel.rs'; Suite = $publicSuite
       Test = 'each_accounting_accessor_reports_the_quantity_it_names'
       Marker = 'cells across live maps, not the map count'
       Edits = @(@{ F = @'
    pub fn tilemap_cells(&self) -> u32 {
        self.maps.cells()
    }
'@
                    R = @'
    pub fn tilemap_cells(&self) -> u32 {
        self.maps.len()
    }
'@ }) }

    # --- Self-tests: the harness must refuse all four -------------------------

    @{ Name = 'self-test-clobbered-source'; File = 'src/maps.rs'
       Test = 'kernel::tests::a_reused_slot_refuses_the_handle_it_displaced'
       Expect = 'the patched source changed under the run'; Clobber = $true
       Edits = @(@{ F = 'Some(slot) if slot.generation == id.generation && slot.map.is_some() => Ok(index),'
                    R = 'Some(slot) if slot.map.is_some() => Ok(index),' }) }

    @{ Name = 'self-test-backdated-source'; File = 'src/maps.rs'
       Test = 'kernel::tests::a_reused_slot_refuses_the_handle_it_displaced'
       Expect = 'cargo did not rebuild'; Backdate = $true
       Edits = @(@{ F = 'Some(slot) if slot.generation == id.generation && slot.map.is_some() => Ok(index),'
                    R = 'Some(slot) if slot.map.is_some() => Ok(index),' }) }

    @{ Name = 'self-test-missing-test'; File = 'src/maps.rs'
       Test = 'a_test_name_that_does_not_exist'
       Expect = 'did not execute exactly one test'
       Edits = @(@{ F = 'Some(slot) if slot.generation == id.generation && slot.map.is_some() => Ok(index),'
                    R = 'Some(slot) if slot.map.is_some() => Ok(index),' }) }

    @{ Name = 'self-test-inert-edit'; File = 'src/maps.rs'
       Test = 'kernel::tests::a_reused_slot_refuses_the_handle_it_displaced'
       Expect = 'did not fail with the rule removed'
       Edits = @(@{ F = 'pub const MAX_TILEMAPS: u32 = 64;'; R = 'pub const MAX_TILEMAPS: u32 = 63;' }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'tilemap-registry-controls'
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
$controlLogs = Join-Path $controlRepo 'target\tilemap-registry-controls\runs'
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
        $controlSuite = if ($control.Suite) { $control.Suite } else { $suite }
        $arguments += $controlSuite + @('--', $control.Test)
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
        # in place when cargo exited.
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
        } elseif ($output -notmatch 'Compiling protogine') {
            $verdict = "$($control.Name): cargo did not rebuild, so the run is about a stale binary"
        } elseif ($output -notmatch 'running 1 test(?!s)') {
            $verdict = "$($control.Name): the run did not execute exactly one test, so it proves nothing"
        # `-cnotmatch`, case-sensitively, and that is the one gate where it
        # matters. Every other pattern here is unambiguous under folding, but a
        # green line reads "test result: ok. N passed; 0 failed", so a
        # case-insensitive FAILED is one prefix away from matching every run.
        # This harness's author hit exactly that bug in a hand-written suite
        # counter during this phase; safe by construction beats safe by prefix.
        } elseif (-not $control.Passes -and $output -cnotmatch 'test result: FAILED') {
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
# Both suites, because controls patch sources that back each of them, and a
# restore that only satisfied one would leave the other's evidence unchecked.
$restoredLines = @()
foreach ($restoreSuite in @($suite, $publicSuite)) {
    $arguments = @('test', '--manifest-path', $controlManifest)
    if ($Release) { $arguments += '--release' }
    $arguments += $restoreSuite
    $output = & cargo @arguments 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        $controlFailures += "the $($restoreSuite[1]) suite did not return to green after restoring"
    }
    $restoredLines += ($output -split "`r?`n" | Where-Object { $_ -match 'test result' })
}
"`nrestored: " + ($restoredLines -join ' ')

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
# Reported as its parts rather than one total, because a single number invites a
# doc to say "N controls, M of them redundancies" and get the arithmetic between
# them wrong.
#
# `@(...)` around every filter, and it is not style. A pipeline that matches one
# item returns that item, not a one-element array; PowerShell then answers
# `.Count` with 1 only for an object that has no `Count` member of its own, and a
# hashtable has one - its number of keys. So a single matching control reports
# its own field count.
$guards = @($controls | Where-Object { -not $_.Expect }).Count
$selfTests = @($controls).Count - $guards
"`nAll $guards registry guards detected with their rule removed." `
    + " The harness refused all $selfTests self-tests."
