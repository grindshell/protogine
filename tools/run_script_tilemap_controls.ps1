# Negative controls for the `ctx.world` map and collider bindings.
#
# Each control removes one rule from the engine source, runs the single test
# that is supposed to catch it, and requires that test to fail at a named
# assertion. A guard that cannot be observed failing is not covered, and a
# passing suite alone does not distinguish the two.
#
# Markers name the exact assertion, never a generic 'assertion' or 'panicked'
# substring: those match any failure at all, which would reduce the harness to
# "something broke" and silently absorb a control that moved to a different
# failure site.
#
# Controls marked `Passes` are expected to keep passing. They record a rule that
# is redundant given something else, and say which something, so a later change
# that removes the other half is visibly uncovered rather than silently so.
#
# Every patch is written to an isolated copy of the tree under `target/`, never
# to the working tree, so a build that overlaps this run cannot pick up a
# deliberately broken source. The run proves that rather than asserting it: it
# fingerprints the engine sources before and after and fails if either moved.
#
# The one real concurrency hazard is now identified and closed, and it was never
# cargo. The copy's path derives from the harness name, so two concurrent runs of
# *this* harness shared one patched tree: each wrote its own control's patch and
# each restored the originals in its own loop, overwriting the other mid-control,
# while each cleared the other's saved cargo output at startup.
# `Enter-ControlLock` now refuses the second run by name instead of accommodating
# it. Sharing the build cache between runs is fine; sharing patched sources never
# was, and the copy alone only ever protected the working tree.
#
# **Markers alias, and no gate here catches it.** Staleness catches a vanished
# anchor, ambiguity a widened one, and the marker gate a failure that moved -
# all three check text, so a marker that is a *substring* of another message
# passes every one of them while witnessing a different rule. The sweep that
# finds it is: for each control's run log, does it contain any other control's
# declared marker. Two controls here declared their fixture's
# `panic!("...: {error}")` wrapper, which is the sentence every failure of that
# fixture produces, so between them they rendered in seven other controls' logs.
# Both now name a latch-specific assertion instead, and each names its own rule
# rather than sharing a tail.
#
# Seven aliases remain and every one is understood: `call-budget-is-the-whole-ceiling`
# and `set-tile-budgeted-against-the-ceiling` share a marker verbatim, which is
# the shared-helper-and-call-site pair working as designed;
# `callback-work-unenforced` and `work-limit-catchable` are the recorded
# shared-verdict pair; the two self-tests carry `array-metatable-accepted`'s
# marker because they deliberately reuse its edit; and
# `plain-unknown-names-in-sprite-options` carries `dense-early-exit`'s, which
# cannot mislead because that control is redundant-by-design and never panics.
#
# Every observed instance was in the safe direction - the harness cried wolf
# rather than passing a control that had not applied - because each conclusion is
# separately gated on the patch surviving the run, the crate actually
# recompiling, exactly one test executing, and a detecting control's test
# reporting failure. Those gates are what caught this, twice. Read the saved
# cargo output for any control that fails.
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$controlRepo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'control_tree.ps1')
$controlSources = @(
    'src/scripting/world.rs',
    'src/scripting/tilemap.rs',
    'src/scripting/utilities.rs',
    'src/kernel.rs'
)
$controlFingerprint = Get-SourceFingerprint -Repo $controlRepo -Files $controlSources
# Claimed before the copy exists, because the copy is what two runs would share.
# Released after the fingerprint check below, so both exits pass through it; a
# run that dies before then leaves a lock its own dead process identifies.
$controlLock = Enter-ControlLock -Repo $controlRepo -Name 'script-tilemap-controls'

# Every target uses the same feature set, so switching between them costs no
# extra rebuild and the 'Compiling protogine' gate stays meaningful.
$features = @('--no-default-features', '--features', 'scripting')
$bindings = @('--test', 'script_tilemap') + $features
$kernelTests = @('--test', 'collision') + $features
$unitTests = @('--lib') + $features
# `tilemap_membership` is a core suite and carries no feature gate, so it builds
# under this feature set too. Running it here rather than under
# `--no-default-features` alone keeps every target on one build, which is what
# makes the 'Compiling protogine' gate mean the patch rebuilt rather than that
# the feature set changed.
$membership = @('--test', 'tilemap_membership') + $features

$regionCharged = @'
                self.region_output(budget, u64::from(columns) * u64::from(rows))?;
                let result = self
                    .kernel
                    .borrow()
                    .tiles_region(&map, column, row, columns, rows);
                let ids = kernel_result(budget, result)?;
'@ -replace "`r`n", "`n"

$chargeThenCheck = @'
        self.callback_work = self.callback_work.saturating_add(units);
        if self.callback_work > MAX_CALLBACK_WORK {
            return Err(CollisionError::Work.into());
        }
        Ok(())
'@ -replace "`r`n", "`n"

$plainUnknownName = @'
        if !known {
            return Err(mlua::Error::runtime(format!("{what} has an unknown field")));
        }
'@ -replace "`r`n", "`n"

$arrayMetatable = @'
    if table.metatable().is_some() {
        return Err(mlua::Error::runtime(format!(
            "{what} must be a plain array with no metatable"
        )));
    }
'@ -replace "`r`n", "`n"

$controls = @(
    # --- Schema validation and copying ---------------------------------------

    @{ Name = 'description-unvalidated'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'an unknown field is refused'
       Edits = @(@{ F = '    plain(desc, DESCRIPTION_FIELDS, "tilemap description")?;'; R = '' }) }

    # Re-earned rather than replaced: M2-6 turned the options table into a
    # placement, so the schema this removes is `PLACEMENT_FIELDS` and the name
    # follows it. Same rule, same fixture, same marker - which is what makes
    # "re-earned" a claim about the same control rather than a new one wearing
    # its number.
    @{ Name = 'placement-unvalidated'; File = 'src/scripting/tilemap.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'
       Marker = 'an unknown option'
       Edits = @(@{ F = '    plain(options, PLACEMENT_FIELDS, "collider placement")?;'; R = '' }) }

    # --- M2-5's transfer, and the refusal it needed ---------------------------
    #
    # The rules added with `transfer_collider`. The first two are why it is a
    # separate call rather than an optional position on the placement table: the
    # destination has to be the one named, and the box has to be the body's own
    # rather than one the call rebuilds.

    @{ Name = 'transfer-ignores-its-destination'; File = 'src/kernel.rs'
       Test = 'a_transferred_body_is_stopped_by_the_destination_rooms_wall'
       Marker = 'and now of the second'
       # The second anchor carries the line below it. `map: map.clone(),` alone
       # occurs twice - here and in a `cfg(test)` helper whose deeper
       # indentation still contains the shorter string - and the ambiguity gate
       # above now refuses that. The helper's next line is
       # `collider: TileCollider {`, so including `collider,` names this site
       # alone.
       Edits = @(@{ F = '        let Some((_, collider)) = self.tile_collider(entity)? else {'
                    R = '        let Some((held, collider)) = self.tile_collider(entity)? else {' },
                 @{ F = "                map: map.clone(),`n                collider,"
                    R = "                map: held,`n                collider," }) }

    @{ Name = 'transfer-rebuilds-the-box'; File = 'src/kernel.rs'; Target = $membership
       Test = 'transfer_carries_the_body_s_own_box_and_refuses_a_body_that_has_none'
       Marker = "the transfer must carry the body's own box"
       Edits = @(@{ F = '                collider,
                position,'
                    R = '                collider: TileCollider { offset_x: 0.0, offset_y: 0.0, width: 32.0, height: 32.0 },
                position,' }) }

    @{ Name = 'transfer-accepts-a-body-with-no-collider'; File = 'src/kernel.rs'
       Test = 'every_refused_transfer_leaves_membership_geometry_and_position_untouched'
       Marker = 'a body with no collider'
       Edits = @(@{ F = '            return Err(KernelError::NoCollider);'; R = '            return Ok(());' }) }

    # M2-8 puts an illegal transfer on the catchable side. Latching it would let
    # a refusal a script is entitled to `pcall` take the session down instead.
    # The marker is the latch-specific assertion's, not the fixture's wrapper.
    # It used to be 'must stay catchable rather than latch', which is a sentence
    # every failure of that fixture produced - and which is also a substring of
    # two other fixtures' wrappers, so it rendered in seven other controls' logs.
    # The patched message ends "limit exceeded" because that is the shape every
    # latch in this engine has, and the assertion tests the classification
    # rather than this variant's text.
    # The two latch markers name their own rule - "illegal transfer" against
    # "malformed argument" - rather than sharing the tail, because a shared tail
    # would alias the moment a control targeted the other fixture.
    @{ Name = 'no-collider-latches'; File = 'src/scripting/world.rs'
       Test = 'every_refused_transfer_leaves_membership_geometry_and_position_untouched'
       Marker = 'an illegal transfer latched a budget'
       Edits = @(@{ F = '        Err(KernelError::NoCollider)
        | Err(KernelError::Inactive)'
                    R = '        Err(KernelError::NoCollider) => Some("no collider limit exceeded"),
        Err(KernelError::Inactive)' }) }

    # A half-given position is a malformed call, not one axis kept. Silently
    # dropping it would move the body on one axis and leave the other, which is
    # a wrong position rather than a refusal.
    @{ Name = 'half-a-transfer-position-accepted'; File = 'src/scripting/world.rs'
       Test = 'every_refused_transfer_leaves_membership_geometry_and_position_untouched'
       Marker = 'x without y'
       Edits = @(@{ F = '                    _ => {
                        return Err(mlua::Error::runtime(
                            "a transfer position needs both x and y",
                        ));
                    }'
                    R = '                    _ => None,' }) }

    # The sweep's *read* of membership, which is a different site from the write
    # above and invisible to every Luau assertion: `tile_collider` reads the
    # stored component and never goes through `sweep_bodies`, so this fault
    # leaves the membership checks green and the body resting at the wrong wall.
    # Only the position assertions can see it.
    #
    # The patch is the one `run_tilemap_membership_controls.ps1` already uses -
    # resolve every candidate against the first one's map - and it is inert with
    # a single body, because the first candidate falls back to its own
    # membership. The fixture carries a second body that stays behind for
    # exactly this reason, and asserts *both* positions, so whichever way the
    # query order runs, one of them is at the wrong wall. Found by the review
    # session, correcting a note of mine that had recorded this as unreachable.
    @{ Name = 'sweep-resolves-one-map-for-all'; File = 'src/kernel.rs'
       Test = 'a_transferred_body_is_stopped_by_the_destination_rooms_wall'
       Marker = "after the transfer the body must be stopped by the second room's wall"
       Edits = @(@{ F = '                map: membership.0,'
                    R = '                map: bodies.first().map_or(membership.0, |first| first.map),' }) }

    # M2-6 returns the map alongside the box because M2-2 makes membership
    # uninferable from position. Dropping it leaves a script no way to observe
    # which map a body is on at all.
    @{ Name = 'tile-collider-drops-the-map'; File = 'src/scripting/world.rs'
       Test = 'a_transferred_body_is_stopped_by_the_destination_rooms_wall'
       Marker = 'a member of the first room'
       Edits = @(@{ F = '                        table.raw_set("map", lua.create_userdata(map)?)?;'; R = '' }) }

    @{ Name = 'array-metatable-accepted'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a cells metatable is refused'
       Edits = @(@{ F = $arrayMetatable; R = '' }) }

    @{ Name = 'array-holes-accepted'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a hole in cells'
       Edits = @(@{ F = "    if seen != expected {`n        return Err(length());`n    }"; R = '' }) }

    @{ Name = 'dimensions-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a fractional dimension is refused'
       Edits = @(@{ F = "    if !whole(count, 0.0, f64::from(u32::MAX)) {`n        return Err(mlua::Error::runtime(format!(`n            `"tilemap {key} must be a nonnegative whole number`"`n        )));`n    }"
                    R = '' }) }

    @{ Name = 'origins-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a fractional origin is refused'
       Edits = @(@{ F = "    if !whole(origin, INDEX_LOW, INDEX_HIGH) {`n        return Err(mlua::Error::runtime(format!(`n            `"tilemap {key} must be a whole number of world pixels`"`n        )));`n    }"
                    R = '' }) }

    @{ Name = 'ids-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a fractional ID is refused'
       Edits = @(@{ F = '            whole(id, 0.0, f64::from(u16::MAX)).then_some(id as u16)'
                    R = '            Some(id as u16)' }) }

    @{ Name = 'solids-truthy-accepted'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Marker = 'a truthy substitute is not a boolean'
       Edits = @(@{ F = "            Value::Boolean(flag) => Some(flag),`n            _ => None,"
                    R = "            Value::Boolean(flag) => Some(flag),`n            _ => Some(true)," }) }

    # --- Arguments, refused without narrowing or wrapping --------------------

    @{ Name = 'indices-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'
       Marker = 'tile column'
       Edits = @(@{ F = '    let index = number(value).filter(|index| whole(*index, INDEX_LOW, INDEX_HIGH));'
                    R = '    let index = number(value);' }) }

    @{ Name = 'extents-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'
       Marker = 'region width'
       Edits = @(@{ F = '    let extent = number(value).filter(|extent| whole(*extent, 0.0, f64::from(u32::MAX)));'
                    R = '    let extent = number(value);' }) }

    @{ Name = 'tile-ids-narrowed'; File = 'src/scripting/tilemap.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'
       Marker = 'tile ID'
       Edits = @(@{ F = '    let id = number(value).filter(|id| whole(*id, 0.0, f64::from(u16::MAX)));'
                    R = '    let id = number(value);' }) }

    # --- Phase gating and the shared attempt budget --------------------------

    # Re-earned: the binding this ungated was M1's implicit installer, retired
    # with the rest of that surface, so the phase gate is now demonstrated on
    # its successor. Same rule, same fixture; the marker follows the call's name
    # because the fixture builds it at runtime from the phase and the call.
    @{ Name = 'mutation-phase-ungated'; File = 'src/scripting/world.rs'
       Test = 'only_init_and_update_may_mutate_the_map_or_its_colliders'
       Marker = 'draw: create_tilemap must refuse'
       Edits = @(@{ F = "            `"create_tilemap`",`n            scope.create_function(move |lua, args: MultiValue| {`n                self.begin(budget, true, writable)?;"
                    R = "            `"create_tilemap`",`n            scope.create_function(move |lua, args: MultiValue| {`n                self.begin(budget, false, writable)?;" }) }

    @{ Name = 'map-calls-uncounted'; File = 'src/scripting/world.rs'
       Test = 'map_calls_share_the_existing_world_attempt_budget'
       Marker = 'map calls must share the 4096 world attempts, not have their own'
       Edits = @(@{ F = "            `"tile_solid`",`n            scope.create_function(move |lua, args: MultiValue| {`n                self.begin(budget, false, writable)?;"
                    R = "            `"tile_solid`",`n            scope.create_function(move |lua, args: MultiValue| {" }) }

    # --- Region output -------------------------------------------------------

    @{ Name = 'region-output-uncharged'; File = 'src/scripting/world.rs'
       Test = 'the_region_output_ceiling_latches_outside_pcall'
       Marker = 'the region output ceiling must refuse the 65th read, uncatchably'
       Edits = @(@{ F = '                self.region_output(budget, u64::from(columns) * u64::from(rows))?;'
                    R = '' }) }

    # A single request larger than the whole callback ceiling must stay an
    # ordinary bounds error. Charging what was asked for rather than what could
    # have been returned turns one out-of-range argument into a session fault,
    # which contradicts the README's own taxonomy.
    @{ Name = 'region-charges-what-was-asked-for'; File = 'src/scripting/world.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'
       # Not the Luau assertion beside the request: the latch escapes `pcall` at
       # the next VM interrupt, so the callback dies before `assert` can report.
       # That is the point of the control and it was the wrong first guess.
       # As with `no-collider-latches`: the marker is the latch-specific
       # assertion's, not the fixture's wrapper, which every failure of that
       # fixture produced.
       Marker = 'a malformed argument latched a budget'
       Edits = @(@{ F = '        let charged = ids.min(u64::from(MAX_REGION_CELLS));'
                    R = '        let charged = ids;' }) }

    # The cap's *value*, which nothing witnessed until this phase. Its fixture
    # used to ask for exactly `MAX_REGION_CELLS`, so `ids.min(…)` was a no-op
    # there and doubling the cap left every fixture green - the cap's existence
    # was covered, through latching, two fixtures away; its value was covered
    # nowhere. The fixture now asks for four times the cap, so the charge is
    # capped rather than coincidentally exact, and this control fails it at a
    # marker of its own rather than sharing `region-refusal-uncharged`'s.
    # Found by the review session's (name, edit, marker) audit against run logs.
    @{ Name = 'region-cap-charges-more-than-it-returns'; File = 'src/scripting/world.rs'
       Test = 'a_refused_region_is_charged_for_what_it_could_have_returned'
       Marker = 'all 64 refusals must fit under the ceiling'
       Edits = @(@{ F = '        let charged = ids.min(u64::from(MAX_REGION_CELLS));'
                    R = '        let charged = ids.min(u64::from(MAX_REGION_CELLS) * 2);' }) }

    @{ Name = 'region-refusal-uncharged'; File = 'src/scripting/world.rs'
       Test = 'a_refused_region_is_charged_for_what_it_could_have_returned'
       Marker = '64 refused requests must exhaust the region output ceiling'
       Edits = @(@{ F = $regionCharged; R = @'
                let result = self
                    .kernel
                    .borrow()
                    .tiles_region(&map, column, row, columns, rows);
                let ids = kernel_result(budget, result)?;
                self.region_output(budget, ids.len() as u64)?;
'@ -replace "`r`n", "`n" }) }

    # --- The aggregate tile-work ceiling -------------------------------------
    #
    # **`callback-work-unenforced` and `work-limit-catchable` share their
    # evidence, and each name claims more than the shared evidence shows.** They
    # are genuinely different rules - whether the ceiling is enforced at all, and
    # whether its refusal latches - but in this fixture they produce identical
    # observable outcomes: under either patch `init()` returns `Ok`, the `pcall`
    # swallows, `unreachable` is logged, and `expect_err` fires before anything
    # downstream can tell the two apart. Both detect; neither *isolates* the rule
    # its name asserts.
    #
    # Recorded rather than fixed, because separating them needs a fixture where
    # the ceiling is enforced and the classification is wrong - a different
    # fixture, not another assertion - and the pair's verdict is real either way.
    # Found by the review session auditing whether any two controls share a
    # verdict, which is a question the harness's own summary cannot ask: it
    # reports that each control detected, never that two detected the same thing.
    #
    # `call-budget-is-the-whole-ceiling` and `set-tile-budgeted-against-the-ceiling`
    # also share a marker, and that pair is fine - it is the shared-helper and
    # call-site pair working as designed, sharing a marker only because
    # `set_tile`'s assertion happens to come first.

    @{ Name = 'callback-work-unenforced'; File = 'src/kernel.rs'
       Test = 'the_aggregate_tile_work_ceiling_latches_outside_pcall'
       Marker = 'the aggregate tile-work ceiling must refuse the 64th install, uncatchably'
       Edits = @(@{ F = "        if self.callback_work > MAX_CALLBACK_WORK {`n            return Err(CollisionError::Work.into());`n        }"
                    R = '' }) }

    # **These two are the same rule and take different patch shapes, and which
    # shape is available is a property of the match rather than of the rule.**
    # Deletion works for an arm whose variant a *broader pattern later* also
    # covers, and dies for an arm that is the only cover for its variant - the
    # difference between an arm that changes a classification and an arm that
    # *is* one. That predicts which controls break the next time someone adds or
    # removes a grouping, which "the match is exhaustive now" does not.
    #
    # `Collision(Work)` can be deleted because `| Err(KernelError::Collision(_))`
    # follows it in the catchable arm and picks the variant up. **So arm order is
    # load-bearing for this control as well as for the behaviour**: move the
    # broad arm above the specific one and the rule changes and the control stops
    # compiling, in either order of discovery.
    #
    # `ColliderLimit` has no such sibling, so deleting it is `E0004`. It is
    # reclassified instead, which tests the same thing - the arm is what puts the
    # variant on the latching side. That control was silently dead from the
    # moment Phase 2 made the match exhaustive until this phase ran the harness;
    # my first account of that said deletion had died for *every* arm at once,
    # which the control immediately above falsifies. Found by the review session.
    @{ Name = 'work-limit-catchable'; File = 'src/scripting/world.rs'
       Test = 'the_aggregate_tile_work_ceiling_latches_outside_pcall'
       Marker = 'the aggregate tile-work ceiling must refuse the 64th install, uncatchably'
       Edits = @(@{ F = '        Err(KernelError::Collision(CollisionError::Work)) => Some("tile work limit exceeded"),'
                    R = '' }) }

    @{ Name = 'collider-limit-catchable'; File = 'src/scripting/world.rs'
       Test = 'the_live_collider_limit_latches_outside_pcall'
       Marker = 'the collider limit must refuse the 1025th attachment, uncatchably'
       Edits = @(@{ F = '        Err(KernelError::ColliderLimit) => Some("collider limit exceeded"),'
                    R = '        Err(KernelError::ColliderLimit) => None,' }) }

    @{ Name = 'callback-work-never-reset'; File = 'src/scripting/world.rs'
       Test = 'tile_work_accounting_starts_over_in_every_callback'
       Marker = 'tile-work accounting must restart each callback'
       Edits = @(@{ F = '        kernel.begin_callback();'; R = '' }) }

    # The shared helper, and then each of its four call sites separately. The
    # helper being right does not establish that all four reach for it: reverting
    # any one of them individually left the whole suite green until the fixture
    # grew one assertion per entry point.
    @{ Name = 'call-budget-is-the-whole-ceiling'; File = 'src/kernel.rs'
       Test = 'every_entry_point_is_budgeted_against_what_the_callback_has_left'; Target = $kernelTests
       Marker = 'set_tile must be budgeted against what the callback has left, not the whole ceiling'
       Edits = @(@{ F = "    fn remaining_work(callback_work: u64) -> WorkBudget {`n        WorkBudget::new(MAX_CALLBACK_WORK.saturating_sub(callback_work))`n    }"
                    R = "    fn remaining_work(_callback_work: u64) -> WorkBudget {`n        WorkBudget::new(MAX_CALLBACK_WORK)`n    }" }) }

    @{ Name = 'set-tile-budgeted-against-the-ceiling'; File = 'src/kernel.rs'
       Test = 'every_entry_point_is_budgeted_against_what_the_callback_has_left'; Target = $kernelTests
       Marker = 'set_tile must be budgeted against what the callback has left, not the whole ceiling'
       Edits = @(@{ F = "        // this one too rather than relying on the collider limit to do it.`n        let mut work = Self::remaining_work(*callback_work);"
                    R = "        // this one too rather than relying on the collider limit to do it.`n        let mut work = WorkBudget::new(MAX_CALLBACK_WORK);" }) }

    @{ Name = 'set-position-budgeted-against-the-ceiling'; File = 'src/kernel.rs'
       Test = 'every_entry_point_is_budgeted_against_what_the_callback_has_left'; Target = $kernelTests
       Marker = 'set_position must be budgeted against what the callback has left'
       # Re-earned: M2-R2 made `set_position` resolve the body's *member* map,
       # so the line above the budget is the membership lookup rather than
       # M1's `self.tilemap`. Same rule, same fixture, same marker.
       Edits = @(@{ F = "                .expect(`"a member map cannot be removed while it has members`");`n            let mut work = Self::remaining_work(self.callback_work);"
                    R = "                .expect(`"a member map cannot be removed while it has members`");`n            let mut work = WorkBudget::new(MAX_CALLBACK_WORK);" }) }

    @{ Name = 'attach-budgeted-against-the-ceiling'; File = 'src/kernel.rs'
       Test = 'every_entry_point_is_budgeted_against_what_the_callback_has_left'; Target = $kernelTests
       Marker = 'set_tile_collider must be budgeted against what the callback has left'
       # Re-earned: Phase 2 put the map lookup and the extent check between the
       # position read and the budget, so the anchor moves to the line that is
       # actually above it now.
       Edits = @(@{ F = "        placement.collider.check(&map.info())?;`n        let mut work = Self::remaining_work(self.callback_work);"
                    R = "        placement.collider.check(&map.info())?;`n        let mut work = WorkBudget::new(MAX_CALLBACK_WORK);" }) }

    @{ Name = 'install-budgeted-against-the-ceiling'; File = 'src/kernel.rs'
       Test = 'every_entry_point_is_budgeted_against_what_the_callback_has_left'; Target = $kernelTests
       # The fixture's install is `replace_tilemap` now: only a map with members
       # charges a placement check per collider, and only a replacement of a map
       # that has them reaches that path. The marker follows.
       Marker = 'replace_tilemap must be budgeted against what the callback has left'
       Edits = @(@{ F = "        let info = map.info();`n        let mut work = Self::remaining_work(*callback_work);"
                    R = "        let info = map.info();`n        let mut work = WorkBudget::new(MAX_CALLBACK_WORK);" }) }

    # The anti-probing half, one control per entry point for the same reason.
    @{ Name = 'set-tile-charges-after-refusing'; File = 'src/kernel.rs'
       Test = 'tile_work_is_charged_per_visited_cell_and_survives_a_refusal'; Target = $kernelTests
       Marker = 'a refused edit is charged for the scan it performed'
       Edits = @(@{ F = "        *callback_work = callback_work.saturating_add(work.used());`n        outcome?;"
                    R = "        outcome?;`n        *callback_work = callback_work.saturating_add(work.used());" }) }

    @{ Name = 'set-position-charges-after-refusing'; File = 'src/kernel.rs'
       Test = 'every_entry_point_charges_the_work_it_performed_before_refusing'; Target = $kernelTests
       Marker = 'a refused teleport is charged for the cells it checked'
       Edits = @(@{ F = "            self.callback_work = self.callback_work.saturating_add(work.used());`n            placement?;"
                    R = "            placement?;`n            self.callback_work = self.callback_work.saturating_add(work.used());" }) }

    @{ Name = 'attach-charges-after-refusing'; File = 'src/kernel.rs'
       Test = 'every_entry_point_charges_the_work_it_performed_before_refusing'; Target = $kernelTests
       Marker = 'a refused attachment is charged for the cells it checked'
       # Re-earned: Phase 2 renamed the deferred result from `placement` to
       # `legal`. The swap is the same one - charge after refusing instead of
       # before - and the marker has not moved.
       Edits = @(@{ F = "        self.callback_work = self.callback_work.saturating_add(work.used());`n        legal?;"
                    R = "        legal?;`n        self.callback_work = self.callback_work.saturating_add(work.used());" }) }

    @{ Name = 'install-charges-after-refusing'; File = 'src/kernel.rs'
       Test = 'every_entry_point_charges_the_work_it_performed_before_refusing'; Target = $kernelTests
       Marker = 'a refused installation is charged for the colliders it revalidated'
       Edits = @(@{ F = "        *callback_work = callback_work.saturating_add(work.used());`n        refusal?;"
                    R = "        refusal?;`n        *callback_work = callback_work.saturating_add(work.used());" }) }

    @{ Name = 'charge-after-the-refusal'; File = 'src/kernel.rs'
       Test = 'every_entry_point_charges_the_work_it_performed_before_refusing'; Target = $kernelTests
       Marker = 'a refused charge is still charged'
       Edits = @(@{ F = $chargeThenCheck; R = @'
        if self.callback_work.saturating_add(units) > MAX_CALLBACK_WORK {
            return Err(CollisionError::Work.into());
        }
        self.callback_work = self.callback_work.saturating_add(units);
        Ok(())
'@ -replace "`r`n", "`n" }) }

    # --- The deadline between conversion batches -----------------------------
    #
    # `dense`'s is here. `region_table`'s check has no control and cannot have
    # one: it runs after the kernel call, on a read that mutates nothing, so a
    # deadline expiring during output conversion leaves no state that differs
    # from one expiring after it. It is kept because the contract requires the
    # observation, and recorded as uncovered rather than left to look covered.

    @{ Name = 'conversion-skips-the-deadline'; File = 'src/scripting/tilemap.rs'
       Test = 'scripting::world::tests::a_description_copy_stops_at_the_deadline_without_installing_a_map'
       Target = $unitTests
       # The marker follows the assertion's reworded message. It used to read
       # "the copy stopped at a batch boundary rather than publishing a map",
       # which claimed a property that assertion does not test - it checks
       # `is_none()`, so what it witnesses is that no map was published. The
       # boundary claim lives on the assertion below it, which exists because
       # that property was once unasserted. Same rule, same fixture, same
       # assertion; only the message is honest now. Found by the review session
       # comparing declared markers against panic sites in the run logs.
       Marker = 'a refused copy must publish no map'
       Edits = @(@{ F = "            budget.check()?;`n            charge(CONVERSION_BATCH.min(expected - seen) as u64)?;"
                    R = "            charge(CONVERSION_BATCH.min(expected - seen) as u64)?;" }) }

    # --- Redundancies, recorded rather than mistaken for coverage ------------

    # Redundant with the range check in `array_index`: keys are unique, so a
    # table cannot hold more than `expected` of them without holding one outside
    # `1..=expected`, which is refused on its own. The early exit bounds nothing
    # extra either, because an array part is traversed in index order. It is kept
    # because it states the postcondition where a reader looks for it.
    @{ Name = 'dense-early-exit'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'; Passes = $true
       Edits = @(@{ F = "        if seen == expected {`n            return Err(length());`n        }"; R = '' }) }

    # Redundant with the `fract` test beside it: `f64::fract` is NaN for both NaN
    # and infinity, so `fract() != 0.0` already refuses every non-finite value.
    # It is kept because the intent should not depend on that, and because a
    # later reader comparing against a range would otherwise have to rediscover
    # why NaN does not slip through the ordered comparisons.
    @{ Name = 'whole-without-the-finite-test'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'; Passes = $true
       Edits = @(@{ F = '    value.is_finite() && value.fract() == 0.0 && value >= low && value <= high'
                    R = '    value.fract() == 0.0 && value >= low && value <= high' }) }

    # `plain` is shared, so one edit needs three targets: the same removal is
    # covered in one place and dead in two, and a control that ran only against
    # this phase's suite would have printed REDUNDANT RULE CONFIRMED whether or
    # not anything covered it. A redundancy control on a shared helper has to be
    # wider than one on a private function - that generalisation is worth more
    # than the finding that produced it.
    #
    # Every description and collider field is required, so a table carrying an
    # unknown name either has too many keys, which the count above refuses, or
    # displaces a required one, which the field read refuses. `SPRITE_FIELDS` is
    # six *optional* names, so `{width = 8, bogus = 1}` is two keys under the
    # count limit with nothing else to refuse it.
    @{ Name = 'plain-unknown-names-in-sprite-options'; File = 'src/scripting/utilities.rs'
       Test = 'sprite_options_reject_metatables_unknown_fields_and_coercions'
       Target = @('--test', 'script_assets') + $features
       Marker = 'accepted an invalid option'
       Edits = @(@{ F = $plainUnknownName; R = '' }) }

    @{ Name = 'plain-unknown-names-in-map-schemas'; File = 'src/scripting/utilities.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'; Passes = $true
       Edits = @(@{ F = $plainUnknownName; R = '' }) }

    @{ Name = 'plain-unknown-names-in-collider-options'; File = 'src/scripting/utilities.rs'
       Test = 'arguments_and_collider_options_are_refused_without_narrowing'; Passes = $true
       Edits = @(@{ F = $plainUnknownName; R = '' }) }

    # --- Self-tests: controls the harness must refuse ------------------------
    #
    # There is otherwise no control on the controls, and this harness's own rule
    # applies to itself: a gate that cannot be observed failing is not covered.
    # One per gate, each built on a genuine instance of what that gate catches
    # rather than a synthetic stand-in, and each fails naming the gate if it is
    # ever removed.
    #
    # They run last because the stale-binary one needs a previous build in the
    # copy's target directory to be skipped in favour of.

    @{ Name = 'self-test-clobbered-source'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Clobber = $true; Expect = 'the patched source changed under the run'
       Edits = @(@{ F = $arrayMetatable; R = '' }) }

    @{ Name = 'self-test-stale-binary'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Backdate = $true; Expect = 'cargo did not rebuild'
       Edits = @(@{ F = $arrayMetatable; R = '' }) }

    @{ Name = 'self-test-missing-test'; File = 'src/scripting/tilemap.rs'
       Test = 'a_test_name_that_does_not_exist'
       Expect = 'did not execute exactly one test'
       Edits = @(@{ F = $arrayMetatable; R = '' }) }

    @{ Name = 'self-test-inert-edit'; File = 'src/scripting/tilemap.rs'
       Test = 'the_description_schema_refuses_every_malformed_shape_catchably'
       Expect = 'did not fail with the rule removed'
       Edits = @(@{ F = 'const CONVERSION_BATCH: usize = 256;'; R = 'const CONVERSION_BATCH: usize = 128;' }) }
)

$controlTree = New-ControlTree -Repo $controlRepo -Name 'script-tilemap-controls'
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
# Every control's full cargo output, saved rather than summarised: a run that
# disagrees with a serial one is a finding about the harness, and it cannot be
# diagnosed from a one-line summary after the fact. Passing controls are kept
# too, because the diagnosis is usually a comparison against one.
$controlLogs = Join-Path $controlRepo 'target\script-tilemap-controls\runs'
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
        # Two ways an anchor can fail to name the site the control claims, and
        # until now only the first was refused.
        #
        # **Stale**: the anchor matches nothing, so the control proves nothing.
        # Caught since M1, and it is what found four controls that Phase 2 had
        # silently broken.
        #
        # That gate exists for slow decay, and it turns out to catch *fresh*
        # drift identically and for free: rewording one assertion's message
        # here refused its control on the next run, within a minute of the edit.
        # Worth saying because the gate reads as being about rot, so the next
        # person to change an assertion message will not expect it to fire - and
        # when it does, the control is stale rather than the code, exactly as the
        # message says. The same is true of `Marker`, which is checked against
        # the run output rather than against the source and so cannot be audited
        # by reading at all.
        #
        # **Ambiguous**: the anchor matches more than once, and `String.Replace`
        # rewrites *every* occurrence - so the control patches sites it does not
        # name. `transfer-ignores-its-destination` did exactly this: its
        # `map: map.clone(),` also matched inside a `#[cfg(test)]` helper four
        # hundred lines away, at deeper indentation, because a 16-space anchor is
        # a substring of a 20-space line. It detected anyway, and only for a
        # reason outside itself - that module is not compiled for an integration
        # target, so the corrupted second site was never seen. Point the same
        # control at `--lib` and it stops compiling, at which point the harness
        # discards the run as proving nothing.
        #
        # A verdict that depends on where the collateral damage happens to land
        # is not a verdict about the rule. Requiring exactly one occurrence turns
        # "this control patches the site I named" from a hope into a gate, and it
        # is the same shape as the staleness refusal one clause wider. Found by
        # the review session, which scanned all 43 anchors; this was the only one
        # that tripped it.
        $missing = $false
        $ambiguous = $null
        foreach ($edit in $control.Edits) {
            $file = if ($edit.File) { $edit.File } else { $control.File }
            if (-not $patched[$file].Contains($edit.F)) { $missing = $true; break }
            $occurrences = ([regex]::Matches($patched[$file], [regex]::Escape($edit.F))).Count
            if ($occurrences -ne 1) { $ambiguous = "$occurrences occurrences in $file"; break }
            $patched[$file] = $patched[$file].Replace($edit.F, $edit.R)
        }
        if ($missing) {
            $controlFailures += "$($control.Name): anchor no longer matches $($control.File); the control is stale, not the code"
            continue
        }
        if ($ambiguous) {
            $controlFailures += "$($control.Name): anchor is ambiguous - $ambiguous; the patch would rewrite sites the control does not name"
            continue
        }

        foreach ($file in $controlSources) {
            [IO.File]::WriteAllText((Join-Path $controlTree $file), $patched[$file], (New-Object Text.UTF8Encoding $false))
        }
        if ($control.Backdate) {
            # Self-test for the rebuild gate. Cargo decides freshness by
            # modification time, so sources that look older than the last build
            # are skipped and the previous binary runs with the mutation
            # compiled out entirely. A genuine instance of the failure mode that
            # gate catches, not a synthetic stand-in for it.
            $stale = (Get-Date).AddDays(-30)
            foreach ($file in $controlSources) {
                (Get-Item -LiteralPath (Join-Path $controlTree $file)).LastWriteTime = $stale
            }
        }
        $target = if ($control.Target) { $control.Target } else { $bindings }
        $arguments = @('test', '--manifest-path', $controlManifest)
        if ($Release) { $arguments += '--release' }
        $arguments += $target + @('--', $control.Test)
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
        # Read back before restoring. This establishes that the patch was still
        # in place when cargo exited, which is weaker than "cargo compiled it" -
        # a clobber reverted mid-run would pass - but there is no cheap way to
        # observe the file during a compile, and every instance observed so far
        # has been persistent.
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
            # control. Observed when two runs of this harness overlapped.
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
$arguments += $bindings
$output = & cargo @arguments 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { $controlFailures += 'the suite did not return to green after restoring' }
"`nrestored: " + (($output -split "`r?`n" | Where-Object { $_ -match 'test result' }) -join '')

Exit-ControlLock -Path $controlLock

if ((Get-SourceFingerprint -Repo $controlRepo -Files $controlSources) -ne $controlFingerprint) {
    $controlFailures += 'the working tree changed during the run; controls must only ever patch the copy'
}

# --- Gate: no control's marker may appear in another control's log -----------
#
# The three per-control gates all compare text, so a marker that is a *substring*
# of another message passes every one of them while witnessing a different rule:
# anchor matched, patch applied, marker present, verdict recorded, wrong rule
# named. That is what a `panic!("...: {error}")` wrapper produces, because its
# own sentence is what every failure of that fixture renders.
#
# Found twice by hand before it was mechanised - once at the transfer battery's
# wrapper, once at the argument battery's - and a class found twice by hand is
# the signal it wants a gate. Promoted from a sweep someone remembers to run.
#
# Declared exceptions rather than a filtered sweep, so an alias is either
# explained here or it fails. That is the difference between a known residual
# and an unnoticed one.
$allowedAliases = @{
    # One rule enforced in a shared helper and at a call site: two controls, one
    # marker, verbatim and by design.
    'call-budget-is-the-whole-ceiling'        = @('set-tile-budgeted-against-the-ceiling')
    'set-tile-budgeted-against-the-ceiling'   = @('call-budget-is-the-whole-ceiling')
    # Two genuinely different rules that this fixture cannot tell apart: under
    # either patch the ceiling is never reported, so both land identically.
    # Recorded rather than fixed - separating them needs a fixture where the
    # ceiling is enforced and the classification is wrong.
    'callback-work-unenforced'                = @('work-limit-catchable')
    'work-limit-catchable'                    = @('callback-work-unenforced')
    # The self-tests are built on a genuine control's edit deliberately, so they
    # render its marker by construction.
    'self-test-clobbered-source'              = @('array-metatable-accepted')
    'self-test-stale-binary'                  = @('array-metatable-accepted')
}
# There is deliberately **no** entry for `plain-unknown-names-in-sprite-options`
# rendering `dense-early-exit`'s marker. The sweep compares against *declared*
# markers, and the four redundant-by-design controls declare none - so that
# alias can never be reported and an exception for it would never fire.
#
# It had one, justified as "redundant by design and never panics", which is the
# reason the entry is **dead** stated as the reason it is **safe**. That is worse
# than a dormant assertion: a dormant assertion proves nothing, where a dormant
# exemption is a loaded permission. Give `dense-early-exit` a marker one day -
# converting a redundant control into a detecting one is an ordinary edit, and
# four controls were re-earned that way this phase - and the exception wakes up
# and permits a real alias, with nobody re-reading a justification written while
# it was inert. Found by the review session, who also corrected their own
# alias count downward-then-upward to get here.
#
# The guard below is the general form: an exemption naming a control that
# declares no marker is refused the same way a stale anchor is.
#
# **It checks the exemptions' values and not their keys, and that asymmetry is
# deliberate.** A bad value fails *silently* - the entry sits there looking
# justified while permitting an alias that can never occur, which is exactly the
# one just removed. A bad key fails *loudly*: the exemption simply never
# applies, so the alias it was meant to cover is reported on the next run and
# reveals itself. One direction needs a gate and the other is self-announcing,
# so a key check would be harmless and would add nothing. Recorded so the guard
# is not later "completed" out of symmetry, which is the same reason `Marker` is
# checked against run output rather than source: saying which half needs the
# treatment, and why, is what stops the next edit being cargo. The observation is
# the review session's.
foreach ($aliasing in $allowedAliases.Keys) {
    foreach ($aliased in $allowedAliases[$aliasing]) {
        if (-not ($controls | Where-Object { $_.Name -eq $aliased -and $_.Marker })) {
            $controlFailures +=
                "$aliasing's alias exception names $aliased, which declares no marker, so the " +
                'exception can never fire; delete it rather than leaving a permission that could wake up'
        }
    }
}
$declaredMarkers = @{}
foreach ($control in $controls) {
    if ($control.Marker) { $declaredMarkers[$control.Name] = $control.Marker }
}
foreach ($log in Get-ChildItem $controlLogs -Filter *.txt -ErrorAction SilentlyContinue) {
    $text = [IO.File]::ReadAllText($log.FullName)
    foreach ($other in $declaredMarkers.Keys) {
        if ($other -eq $log.BaseName) { continue }
        if (-not $text.Contains($declaredMarkers[$other])) { continue }
        if ($allowedAliases[$log.BaseName] -contains $other) { continue }
        $controlFailures +=
            "$($log.BaseName): its log renders $other's marker, so that control cannot " +
            'distinguish its rule from this one; give each a marker only its own assertion produces'
    }
}

if ($controlFailures.Count -gt 0) {
    "`nUNCOVERED GUARDS:"
    $controlFailures | ForEach-Object { "  $_" }
    "`nFull cargo output for each: $controlLogs"
    "A red result from a run that overlapped another cargo invocation is worth"
    "re-running serially before it is believed; see the header."
    exit 1
}
# Reported as its three parts rather than one total. A single number invites a
# doc to say "N controls, M of them redundancies" and get the arithmetic between
# them wrong, which is exactly what happened once here.
#
# `@(...)` around every filter: a pipeline that matches exactly one control
# returns that hashtable rather than a one-element array, and `.Count` on a
# hashtable is its number of keys. These three all match more than one today, so
# nothing here has ever misreported; the sample harness added in Phase 4 has a
# single redundancy and printed its field count instead.
$guards = @($controls | Where-Object { -not $_.Expect }).Count
$redundant = @($controls | Where-Object { $_.Passes }).Count
$selfTests = @($controls).Count - $guards
"`nAll $guards binding guards behaved as specified: $($guards - $redundant) detected with the" `
    + " rule removed and $redundant confirmed redundant. The harness refused all $selfTests self-tests."
