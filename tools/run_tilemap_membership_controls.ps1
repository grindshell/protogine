# Negative controls for M2 Phase 2: membership, transfer and the fixed pass.
#
# Each control removes one rule, runs the single test that is supposed to catch
# it, and requires that test to fail at a named assertion. A guard that cannot be
# observed failing is not covered, and a passing suite alone does not distinguish
# the two.
#
# Markers name the exact assertion, never a generic 'assertion' or 'panicked'
# substring. A named marker is also only reachable if nothing before it can
# panic first, which is why the fixtures these target compare `Result`s and name
# their `expect`s rather than unwrapping.
#
# **The registry harness carries M2-R1's four scoping controls, not this one.**
# Those were written in Phase 1 and re-earned here rather than replaced: their
# anchors moved from `current == target` to the body's own membership, and their
# fixture moved from one body on one map to a member on each of two maps with a
# third empty. Keeping them under their original names is what makes "re-earned"
# a claim about the same controls.
#
# One entry is a **recorded redundancy** and is expected to keep passing: the
# same per-body term that the prediction catches, run against the equality,
# which cannot see it. That is the contract's "necessary and not sufficient"
# claim demonstrated rather than asserted.
#
# Every patch is written to an isolated copy of the tree under `target/`, never
# to the working tree. The run proves that rather than asserting it: it
# fingerprints the sources before and after and fails if either moved.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')
$controlSources = @('src/kernel.rs')
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources
$controlLock = Enter-ControlLock -Repo $controlRepo -Name 'tilemap-membership-controls'

# Core configuration: membership is engine state and needs no decoder, VM or
# window. Two suites, because the fixed-pass rules and the behavioural ones
# live in different files and a control names the one it belongs to.
$behaviour = @('--test', 'tilemap_membership', '--no-default-features')
$fixedPass = @('--test', 'tilemap_fixed_pass', '--no-default-features')

# The sweep's collection loop, whose query shape is the rule rather than an
# implementation detail. Written out in full because the mutation is a change of
# *shape* - `Option<&Membership>` to `&Membership` - and the surrounding lines
# have to go with it.
#
# The `outcome` binding comes with it: removing the only `Err` arm leaves its
# error type unconstrained, so the patch has to annotate what the discarded
# branch used to infer. That is a property of the mutation, not a workaround -
# the refusal really was the only thing naming the type.
$optionalMembership = @'
        let mut outcome = Ok(());
        for (entity, position, velocity, collider, membership) in world
            .query::<(
                Entity,
                &Position,
                &Velocity,
                &TileCollider,
                Option<&Membership>,
            )>()
            .iter()
        {
            let Some(membership) = membership else {
                outcome = Err(KernelError::UnpairedCollider);
                break;
            };
'@
$requiredMembership = @'
        let mut outcome: Result<(), KernelError> = Ok(());
        for (entity, position, velocity, collider, membership) in world
            .query::<(Entity, &Position, &Velocity, &TileCollider, &Membership)>()
            .iter()
        {
'@
# Attachment writing only the collider, which is the state M2-2 makes
# representable and the bundle makes unreachable.
$pairedWrite = @'
        self.world
            .insert(entity, (placement.collider, Membership(id)))
'@
$unpairedWrite = @'
        self.world
            .insert_one(entity, placement.collider)
'@
# The same write moved ahead of the refusal. The call still refuses; what
# changes is that membership and geometry have already moved when it does,
# which is the partial outcome M2-5's order of checks exists to prevent.
$validateThenWrite = @'
        legal?;
        self.world
            .insert(entity, (placement.collider, Membership(id)))
            .map_err(|_| KernelError::InvalidHandle)?;
'@
$writeThenValidate = @'
        self.world
            .insert(entity, (placement.collider, Membership(id)))
            .map_err(|_| KernelError::InvalidHandle)?;
        legal?;
'@

$controls = @(
    # --- Membership decides which map a body collides against ----------------

    # The contract asks for "membership read from position rather than from the
    # component". Two overlapping maps make that literally ambiguous - which is
    # M2-3's whole point - so the removable rule is the one underneath it: the
    # sweep consults each body's own membership rather than one map for all.
    @{ Name = 'sweep-ignores-membership'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Marker = 'while a body at the same coordinates on the far-walled map passes it'
       Edits = @(@{ F = '                map: membership.0,'
                    R = '                map: bodies.first().map_or(membership.0, |first| first.map),' }) }

    # M2-R2. The near map here is the implicit one deliberately, so a teleport
    # resolving the session's current map instead of the body's own passes every
    # assertion until the body transfers away.
    @{ Name = 'teleport-uses-the-implicit-map'; Suite = $behaviour
       Test = 'a_teleport_validates_against_the_member_map_and_no_other'
       Marker = 'the destination is legal on the map the body is now a member of'
       Edits = @(@{ F = '                .get(membership.0)'
                    R = '                .get(self.current.expect("an implicit map"))' }) }

    # --- Transfer is atomic --------------------------------------------------

    # M2-5's order of checks exists so a refusal leaves membership, geometry and
    # position untouched. Writing before validating leaves all three moved.
    @{ Name = 'transfer-writes-before-validating'; Suite = $behaviour
       Test = 'a_refused_transfer_leaves_membership_geometry_and_position_untouched'
       Marker = 'an illegal destination box left membership and geometry alone'
       Edits = @(@{ F = $validateThenWrite; R = $writeThenValidate }) }

    # --- The pair the design makes unreachable -------------------------------

    # M2's sixth required control, and the one that pays for M2-2's choice of a
    # representable invalid state. It cannot be built through the API - both
    # components are written as one bundle - so it patches the write and then
    # observes the fixed pass, which is where the state would surface.
    @{ Name = 'attach-writes-no-membership'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Marker = 'every collider must sweep against the map it is a member of'
       Edits = @(@{ F = $pairedWrite; R = $unpairedWrite }) }

    # **The silent version of the same fault, which is the one worth having.**
    # Requiring `&Membership` in the sweep's query is the natural way to write
    # it, and with the pair broken an unpaired body then matches neither the
    # sweep nor free flight - `fixed_update` runs that branch only for entities
    # with no collider - so its position is never written and it freezes with
    # nothing reported. Two edits, because the failure needs both. What catches
    # it is the length check, which is why that check is not decoration.
    @{ Name = 'sweep-requires-membership-and-loses-a-body'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Marker = 'every collider must become a swept candidate'
       Edits = @(
           @{ F = $pairedWrite; R = $unpairedWrite },
           @{ F = $optionalMembership; R = $requiredMembership }) }

    # The other half of M2's sixth control: a collider that survives its map's
    # removal. `has_members` is what stops that, so removing its effect lets a
    # map with a live member be retired underneath it.
    @{ Name = 'removal-ignores-its-own-members'; Suite = $behaviour
       Test = 'removing_a_map_leaves_unrelated_maps_and_their_bodies_running'
       Marker = 'a map with a member refuses removal'
       Edits = @(@{ F = '            .any(|membership| membership.0 == id)'
                    R = '            .any(|membership| membership.0 == id && false)' }) }

    # The collider count is maintained at three sites and `fixed_update`'s
    # early-out trusts it, so a desync skips the sweep entirely and free-flights
    # every body through every wall. `sweep_bodies`' length check observes the
    # counter from the other side, but only on the branch where the sweep runs -
    # so the despawn site gets a control of its own rather than relying on it.
    @{ Name = 'despawn-keeps-the-collider-count'; Suite = $behaviour
       Test = 'a_collider_and_its_membership_arrive_and_leave_together'
       Marker = 'despawn must release the collider count as well as the components'
       Edits = @(@{ F = @'
        if had_collider {
            self.colliders -= 1;
        }
'@
                    R = '' }) }

    # --- The fixed pass ------------------------------------------------------

    # A uniform per-body term - the pass resolving membership once per body,
    # which is a completely plausible implementation. This is the defect the
    # prediction exists for.
    @{ Name = 'fixed-pass-adds-a-per-body-term'; Suite = $fixedPass
       Test = 'every_body_charges_exactly_what_its_geometry_predicts'
       Marker = 'every body must charge exactly what its geometry predicts'
       Edits = @(@{ F = '        *fixed_work = work.used();'
                    R = '        *fixed_work = work.used() + bodies.len() as u64;' }) }

    # --- Recorded redundancy: the equality cannot see the same defect --------

    # Expected to keep passing, and that is the point. The identical patch above
    # raises both arms by 1,024 and leaves them equal, so the equality is
    # satisfied by exactly the defect its neighbour catches. This is the
    # contract's "necessary and not sufficient" claim, demonstrated rather than
    # asserted - and the reason nobody should prune the prediction because the
    # equality beside it passes.
    #
    # It only demonstrates that while the equality test asserts an equality and
    # nothing else. The first draft of that test pinned the absolute total too,
    # which made this control fail rather than pass - a redundancy that turned
    # out not to be one, and the reason the two assertions now live in separate
    # tests.
    @{ Name = 'equality-alone-cannot-see-a-per-body-term'; Suite = $fixedPass; Passes = $true
       Test = 'the_same_bodies_charge_the_same_across_one_map_and_sixty_four'
       Edits = @(@{ F = '        *fixed_work = work.used();'
                    R = '        *fixed_work = work.used() + bodies.len() as u64;' }) }

    # --- Self-tests: the harness must refuse all four ------------------------

    @{ Name = 'self-test-clobbered-source'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Expect = 'the patched source changed under the run'; Clobber = $true
       Edits = @(@{ F = $pairedWrite; R = $unpairedWrite }) }

    @{ Name = 'self-test-backdated-source'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Expect = 'cargo did not rebuild'; Backdate = $true
       Edits = @(@{ F = $pairedWrite; R = $unpairedWrite }) }

    @{ Name = 'self-test-missing-test'; Suite = $behaviour
       Test = 'a_test_name_that_does_not_exist'
       Expect = 'did not execute exactly one test'
       Edits = @(@{ F = $pairedWrite; R = $unpairedWrite }) }

    @{ Name = 'self-test-inert-edit'; Suite = $behaviour
       Test = 'two_overlapping_maps_stop_their_own_bodies_at_their_own_walls'
       Expect = 'did not fail with the rule removed'
       Edits = @(@{ F = 'pub const ENTITY_LIMIT: u32 = 16_384;'
                    R = 'pub const ENTITY_LIMIT: u32 = 16_383;' }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'tilemap-membership-controls'
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
$controlLogs = Join-Path $controlRepo 'target\tilemap-membership-controls\runs'
if (Test-Path -LiteralPath $controlLogs) { Remove-Item -LiteralPath $controlLogs -Recurse -Force }
New-Item -ItemType Directory -Force -Path $controlLogs | Out-Null
function Save-ControlOutput {
    param([string]$Name, [string]$Text)
    [IO.File]::WriteAllText((Join-Path $controlLogs "$Name.txt"), $Text, (New-Object Text.UTF8Encoding $false))
}
try {
    foreach ($control in $controls) {
        $patched = @{}
        foreach ($file in $controlSources) { $patched[$file] = $controlOriginals[$file] }
        $missing = $false
        foreach ($edit in $control.Edits) {
            $file = if ($edit.File) { $edit.File } else { $controlSources[0] }
            if (-not $patched[$file].Contains($edit.F)) { $missing = $true; break }
            $patched[$file] = $patched[$file].Replace($edit.F, $edit.R)
        }
        if ($missing) {
            $controlFailures += "$($control.Name): anchor no longer matches; the control is stale, not the code"
            continue
        }

        foreach ($file in $controlSources) {
            [IO.File]::WriteAllText((Join-Path $controlTree $file), $patched[$file], (New-Object Text.UTF8Encoding $false))
        }
        if ($control.Backdate) {
            $stale = (Get-Date).AddDays(-30)
            foreach ($file in $controlSources) {
                (Get-Item -LiteralPath (Join-Path $controlTree $file)).LastWriteTime = $stale
            }
        }
        $arguments = @('test', '--manifest-path', $controlManifest)
        if ($Release) { $arguments += '--release' }
        $arguments += $control.Suite + @('--', $control.Test)
        $output = & cargo @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
        if ($control.Clobber) {
            foreach ($file in $controlSources) {
                [IO.File]::WriteAllText((Join-Path $controlTree $file), $controlOriginals[$file], (New-Object Text.UTF8Encoding $false))
            }
        }
        $applied = $true
        foreach ($file in $controlSources) {
            if ([IO.File]::ReadAllText((Join-Path $controlTree $file)) -ne $patched[$file]) {
                $applied = $false
            }
        }
        Restore-ControlSources
        Save-ControlOutput -Name $control.Name -Text $output

        $where = ($output -split "`r?`n" | Where-Object { $_ -match 'panicked at|assertion|left:|right:' } | Select-Object -First 3) -join ' | '
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
        } elseif (-not $control.Passes -and $output -cnotmatch 'test result: FAILED') {
            # Case-sensitive: a green line reads "test result: ok. N passed; 0
            # failed", so a folded FAILED is one prefix away from matching every
            # run. Safe by construction rather than by prefix.
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

$restoredLines = @()
foreach ($restoreSuite in @($behaviour, $fixedPass)) {
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
# `@(...)` around every filter: a pipeline matching one item returns that item,
# and a hashtable answers `.Count` with its field count.
$guards = @($controls | Where-Object { -not $_.Expect }).Count
$redundant = @($controls | Where-Object { $_.Passes }).Count
$selfTests = @($controls).Count - $guards
"`nAll $guards membership guards behaved as specified: $($guards - $redundant) detected with the" `
    + " rule removed and $redundant confirmed redundant. The harness refused all $selfTests self-tests."
