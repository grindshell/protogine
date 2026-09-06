#![cfg(feature = "scripting")]

use protogine::{
    input::{Button, Buttons, InputSnapshot},
    kernel::{FIXED_DT, KernelError, Position, Velocity},
    runtime::GameRuntime,
    scripting::{ScriptLimits, ScriptState},
};
use std::{fs, time::Duration};
use tempfile::TempDir;

fn game(source: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), source).unwrap();
    root
}

fn load(root: &TempDir) -> GameRuntime {
    GameRuntime::load(root.path(), ScriptLimits::default()).unwrap()
}

fn state(runtime: &GameRuntime) -> Vec<(Position, Velocity)> {
    runtime
        .kernel()
        .snapshot()
        .unwrap()
        .into_iter()
        .map(|e| (e.position, e.velocity))
        .collect()
}

#[test]
fn immediate_world_operations_then_systems_and_owned_snapshots() {
    let root = game(
        r#"
        local e, tick = nil, 0
        return {
            init = function(ctx)
                local w = ctx.world
                e = w.spawn(10, 20)
                assert(type(e) == 'userdata' and w.entities()[1] == e)
                assert(not pcall(function() e.x = 100 end))
                assert(w.velocity(e).x == 0)
                w.set_position(e, 30, 40)
                assert(w.position(e).x == 30)
                w.set_velocity(e, 60, -120)
                local value = w.position(e); value.x = -1
                assert(w.position(e).x == 30)
                for _, handle in w.entities() do
                    local other = w.spawn(0, 0)
                    w.despawn(other)
                    assert(not pcall(w.position, other))
                    assert(w.position(handle).x == 30)
                end
            end,
            update = function(ctx, dt)
                assert(dt == 1/60)
                assert(ctx.world.position(e).x == 30 + tick)
                assert(ctx.world.position(e).y == 40 - tick * 2)
                tick += 1
            end,
            draw = function(ctx) assert(ctx.world.position(e).x == 30 + tick) end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    let snapshot = runtime.kernel().snapshot().unwrap();
    for _ in 0..3 {
        runtime.step(InputSnapshot::default()).unwrap();
        runtime.draw(0.0).unwrap();
    }
    assert_eq!(runtime.completed_ticks(), 3);
    assert_eq!(
        state(&runtime),
        [(
            Position { x: 33.0, y: 34.0 },
            Velocity { x: 60.0, y: -120.0 }
        )]
    );
    assert_eq!(snapshot[0].position.x, 30.0);
}

#[test]
fn permissions_expired_functions_and_validation_are_catchable() {
    let root = game(
        r#"
        local e, stale, old_input
        local function readonly(ctx)
            local w = ctx.world
            assert(not pcall(w.spawn, 0, 0))
            assert(not pcall(w.despawn, e))
            assert(not pcall(w.set_position, e, 1, 1))
            assert(not pcall(w.set_velocity, e, 1, 1))
            assert(w.position(e).x == 9)
        end
        return {
            init = function(ctx)
                e = ctx.world.spawn(9, 0)
                stale = ctx.world; old_input = ctx.input.held
                assert(not ctx.input.held('right'))
            end,
            update = function(ctx)
                local w = ctx.world
                for _, f in stale do assert(not pcall(f, e, 0, 0)) end
                assert(not pcall(old_input, 'right'))
                assert(not pcall(ctx.input.held, 'unknown'))
                assert(not pcall(w.position, 1))
                assert(not pcall(w.position, ctx.data.null))
                for _, n in {0/0, math.huge, -math.huge} do
                    assert(not pcall(w.spawn, n, 0))
                    assert(not pcall(w.set_position, e, 0, n))
                    assert(not pcall(w.set_velocity, e, n, 0))
                end
                local old = w.spawn(1, 1); w.despawn(old)
                local fresh = w.spawn(2, 2)
                assert(old ~= fresh)
                assert(not pcall(w.despawn, old))
                assert(not pcall(w.set_position, old, 99, 99))
                assert(w.position(fresh).x == 2)
                w.despawn(fresh)
                assert(#w.entities() == 1)
            end,
            draw = readonly,
            shutdown = function(ctx)
                readonly(ctx)
                assert(not ctx.input.held('right') and not ctx.input.pressed('right'))
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    runtime.step(InputSnapshot::held([Button::Right])).unwrap();
    runtime.draw(0.0).unwrap();
    let entity = runtime.kernel().entities().unwrap().remove(0);
    runtime.shutdown().unwrap();
    assert_eq!(runtime.state(), ScriptState::Stopped);
    assert_eq!(
        runtime.kernel().position(&entity),
        Err(KernelError::Inactive)
    );
    assert!(runtime.step(InputSnapshot::default()).is_err());
    runtime.shutdown().unwrap();
}

#[test]
fn malformed_world_arguments_count_toward_callback_limit() {
    for invalid in [
        "w.spawn(0, {})",
        "w.spawn()",
        "w.despawn({})",
        "w.despawn()",
        "w.position({})",
        "w.position()",
        "w.velocity({})",
        "w.velocity()",
        "w.set_position(e, 0, {})",
        "w.set_position()",
        "w.set_velocity(e, {}, 0)",
        "w.set_velocity()",
    ] {
        for exceed in [false, true] {
            let root = game(&format!(
                r#"local e
                return {{
                    init = function(ctx)
                        e = ctx.world.spawn(0, 0)
                        ctx.world.set_velocity(e, 60, 0)
                    end,
                    update = function(ctx)
                        local w = ctx.world
                        assert(not pcall(function() {invalid} end))
                        for i = 1, 4094 do w.position(e) end
                        if {exceed} then w.position(e) end
                        local ok = pcall(w.set_position, e, 10, 20)
                        if {exceed} then
                            pcall(ctx.fs.write, 'progress.tot', 'overwritten')
                        else
                            assert(ok)
                        end
                    end,
                }}"#,
            ));
            let data = tempfile::tempdir().unwrap();
            let path = data.path().join("progress.tot");
            fs::write(&path, "original").unwrap();
            let mut runtime = GameRuntime::load_with_data_root(
                root.path(),
                data.path(),
                ScriptLimits {
                    callback_timeout: Duration::from_secs(2),
                    ..ScriptLimits::default()
                },
            )
            .unwrap();
            runtime.init().unwrap();
            let result = runtime.step(InputSnapshot::default());
            assert_eq!(fs::read_to_string(&path).unwrap(), "original", "{invalid}");
            if exceed {
                let error = result.expect_err(invalid);
                assert!(
                    error.message.contains("world operation limit exceeded"),
                    "{invalid}: {error}"
                );
                assert_eq!(runtime.state(), ScriptState::Faulted);
                assert_eq!(runtime.completed_ticks(), 0);
                assert_eq!(runtime.kernel().snapshot(), Err(KernelError::Inactive));
            } else {
                result.unwrap_or_else(|error| panic!("{invalid}: {error}"));
                assert_eq!(state(&runtime)[0].0, Position { x: 11.0, y: 20.0 });
                // The caught argument error counts once, the 4096th operation
                // succeeds, and another callback starts with a fresh budget.
                runtime.step(InputSnapshot::default()).unwrap();
                assert_eq!(runtime.completed_ticks(), 2);
                assert_eq!(state(&runtime)[0].0, Position { x: 11.0, y: 20.0 });
            }
        }
    }
}

#[test]
fn resource_limits_fault_even_inside_protected_calls() {
    for (source, updates, expected) in [
        (
            "return {init = function(ctx) for i=1,4097 do pcall(ctx.world.entities) end end}",
            0,
            "world operation limit exceeded",
        ),
        (
            "return {update = function(ctx) for i=1,4096 do pcall(ctx.world.spawn, 0, 0) end end}",
            4,
            "entity limit exceeded",
        ),
    ] {
        let root = game(source);
        let mut runtime = GameRuntime::load(
            root.path(),
            ScriptLimits {
                callback_timeout: Duration::from_secs(2),
                ..ScriptLimits::default()
            },
        )
        .unwrap();
        let error = if updates == 0 {
            runtime.init().unwrap_err()
        } else {
            runtime.init().unwrap();
            for _ in 0..updates {
                runtime.step(InputSnapshot::default()).unwrap();
            }
            runtime.step(InputSnapshot::default()).unwrap_err()
        };
        assert!(error.message.contains(expected), "{error}");
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert_eq!(runtime.completed_ticks(), updates);
    }
}

#[test]
fn update_or_system_failure_stops_catch_up_and_draw() {
    for fail_update in [true, false] {
        let root = game(&format!(
            r#"
            return {{
                init = function(ctx)
                    local e = ctx.world.spawn(1.7976931348623157e308, 0)
                    ctx.world.set_velocity(e, 1.7976931348623157e308, 0)
                end,
                update = function(ctx)
                    ctx.log('update')
                    if {fail_update} then error('update failed') end
                end,
                draw = function() error('draw must not run') end,
                shutdown = function() error('shutdown must not run') end,
            }}
        "#
        ));
        let mut runtime = load(&root);
        runtime.init().unwrap();
        let entity = runtime.kernel().entities().unwrap().remove(0);
        let error = runtime
            .frame(FIXED_DT * 3.0, InputSnapshot::default())
            .unwrap_err();
        assert_eq!(error.phase, if fail_update { "update" } else { "systems" });
        assert_eq!(runtime.completed_ticks(), 0);
        assert_eq!(runtime.take_logs(), ["update"]);
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert_eq!(
            runtime.kernel().position(&entity),
            Err(KernelError::Inactive)
        );
        runtime.shutdown().unwrap();
        assert_eq!(runtime.last_error().unwrap().phase, error.phase);
    }
}

const INPUT_GAME: &str = r#"
    local function report(ctx, phase)
        local i = ctx.input
        ctx.log(phase .. ':' .. tostring(i.held('right')) .. ':' .. tostring(i.pressed('right')) .. ':' .. tostring(i.released('right')))
    end
    return { update = function(ctx) report(ctx, 'U') end, draw = function(ctx) report(ctx, 'D') end }
"#;

#[test]
fn lifecycle_failures_invalidate_worlds_and_completed_ticks_remain_accurate() {
    for (phase, expected_ticks) in [("init", 0), ("update", 1), ("draw", 2), ("shutdown", 2)] {
        let root = game(&format!(
            r#"
            local phase = '{phase}'
            local count = 0
            return {{
                init = function(ctx)
                    ctx.world.spawn(0, 0)
                    if phase == 'init' then error('init failure') end
                end,
                update = function(ctx)
                    count += 1
                    if phase == 'update' and count == 2 then error('update failure') end
                end,
                draw = function(ctx)
                    if phase == 'draw' then error('draw failure') end
                end,
                shutdown = function(ctx)
                    if phase == 'shutdown' then error('shutdown failure') end
                end,
            }}
        "#
        ));
        let mut runtime = load(&root);
        let result = runtime
            .init()
            .and_then(|()| {
                runtime
                    .frame(2.0 * FIXED_DT, InputSnapshot::default())
                    .map(|_| ())
            })
            .and_then(|()| runtime.shutdown());
        assert_eq!(result.unwrap_err().phase, phase);
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert_eq!(runtime.completed_ticks(), expected_ticks);
        assert_eq!(runtime.kernel().entities(), Err(KernelError::Inactive));
        assert!(runtime.init().is_err());
        assert!(runtime.frame(0.0, InputSnapshot::default()).is_err());
        runtime.shutdown().unwrap();
        assert_eq!(runtime.state(), ScriptState::Faulted);
    }
    let root = game(
        "return {init = function() error('must not run') end, shutdown = function() error('must not run') end}",
    );
    let mut runtime = load(&root);
    runtime.shutdown().unwrap();
    assert_eq!(runtime.state(), ScriptState::Stopped);
    assert_eq!(runtime.kernel().entities(), Err(KernelError::Inactive));
}

#[test]
fn pending_edges_survive_zero_tick_frames_and_only_reach_first_catch_up_tick() {
    let root = game(INPUT_GAME);
    let mut runtime = load(&root);
    runtime.init().unwrap();
    let first = runtime
        .frame(FIXED_DT / 2.0, InputSnapshot::held([Button::Right]))
        .unwrap();
    assert_eq!((first.ticks, first.alpha), (0, 0.5));
    assert_eq!(runtime.take_logs(), ["D:true:true:false"]);
    runtime.frame(0.0, InputSnapshot::default()).unwrap();
    assert_eq!(runtime.take_logs(), ["D:false:false:true"]);
    let catch_up = runtime
        .frame(FIXED_DT * 2.5, InputSnapshot::default())
        .unwrap();
    assert_eq!(catch_up.ticks, 3);
    assert_eq!(
        runtime.take_logs(),
        [
            "U:false:true:true",
            "U:false:false:false",
            "U:false:false:false",
            "D:false:false:false"
        ]
    );
    runtime
        .frame(FIXED_DT * 2.0, InputSnapshot::held([Button::Right]))
        .unwrap();
    assert_eq!(
        runtime.take_logs(),
        [
            "U:true:true:false",
            "U:true:false:false",
            "D:true:true:false"
        ]
    );
    runtime
        .frame(FIXED_DT, InputSnapshot::held([Button::Right]))
        .unwrap();
    assert_eq!(
        runtime.take_logs(),
        ["U:true:false:false", "D:true:false:false"]
    );
}

#[test]
fn explicit_taps_survive_unchanged_held_state() {
    let root = game(INPUT_GAME);
    let mut runtime = load(&root);
    runtime.init().unwrap();
    runtime
        .frame(
            0.0,
            InputSnapshot {
                pressed: Buttons::new([Button::Right]),
                released: Buttons::new([Button::Right]),
                ..InputSnapshot::default()
            },
        )
        .unwrap();
    assert_eq!(runtime.take_logs(), ["D:false:true:true"]);
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(runtime.take_logs(), ["U:false:true:true"]);
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(runtime.take_logs(), ["U:false:false:false"]);
}

#[test]
fn frame_clamp_overload_remainder_and_invalid_time() {
    let root = game(INPUT_GAME);
    let mut runtime = load(&root);
    assert!(runtime.frame(FIXED_DT, InputSnapshot::default()).is_err());
    runtime.init().unwrap();
    let first = runtime
        .frame(FIXED_DT * 1.5, InputSnapshot::default())
        .unwrap();
    assert_eq!((first.ticks, first.alpha), (1, 0.5));
    let overload = runtime.frame(1.0, InputSnapshot::default()).unwrap();
    assert_eq!((overload.ticks, overload.dropped_ticks), (5, 10));
    assert_eq!(overload.clamped_seconds, 0.75);
    assert_eq!(overload.alpha, 0.5);
    assert_eq!(runtime.overloads(), 1);
    for elapsed in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(
            runtime
                .frame(elapsed, InputSnapshot::held([Button::Right]))
                .is_err()
        );
    }
    assert!(runtime.draw(f64::NAN).is_err());
    assert_eq!(runtime.state(), ScriptState::Running);
    assert_eq!(runtime.completed_ticks(), 6);
    let next = runtime
        .frame(FIXED_DT / 2.0, InputSnapshot::default())
        .unwrap();
    assert_eq!((next.ticks, next.alpha), (1, 0.0));
    assert_eq!(
        runtime.take_logs(),
        ["U:false:false:false", "D:false:false:false"]
    );
    assert_eq!(runtime.overloads(), 1);
}

#[test]
fn exact_steps_preserve_frame_fraction_and_callbacks_can_be_absent() {
    let root = game(
        "return { init = function(ctx) local e = ctx.world.spawn(0,0); ctx.world.set_velocity(e,60,0) end }",
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    runtime
        .frame(FIXED_DT / 2.0, InputSnapshot::default())
        .unwrap();
    runtime.step(InputSnapshot::default()).unwrap();
    let next = runtime
        .frame(FIXED_DT / 2.0, InputSnapshot::default())
        .unwrap();
    assert_eq!((next.ticks, next.alpha), (1, 0.0));
    assert_eq!(runtime.completed_ticks(), 2);
    assert_eq!(state(&runtime)[0].0.x, 2.0);
}

#[test]
fn fixed_input_replay_matches_across_frame_rates() {
    let root = game(include_str!("../examples/games/movement/main.luau"));
    let mut expected = None;
    for fps in [30, 60, 100, 120, 144, 180, 240, 30] {
        let mut runtime = load(&root);
        runtime.init().unwrap();
        for frame in 0..fps {
            let button = if frame < fps / 2 {
                Button::Right
            } else {
                Button::Down
            };
            runtime
                .frame(1.0 / f64::from(fps), InputSnapshot::held([button]))
                .unwrap();
        }
        assert_eq!(runtime.completed_ticks(), 60, "{fps} FPS");
        let actual = state(&runtime);
        assert_eq!(actual[0].0, Position { x: 46.0, y: 46.0 });
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn entity_handles_remain_table_keys_across_enumeration_and_slot_reuse() {
    let root = game(
        r#"
        local saved, old
        local state = {}
        return {
            init = function(ctx)
                saved = ctx.world.spawn(0, 0)
                state[saved] = 'player'
                local again = ctx.world.entities()[1]
                assert(rawequal(saved, again))
                assert(state[again] == 'player')
            end,
            update = function(ctx)
                local again = ctx.world.entities()[1]
                assert(rawequal(saved, again) and state[again] == 'player')
                state[again] = 'player'
                local count = 0
                for _ in state do count += 1 end
                assert(count == (if old then 2 else 1))
                if not old then
                    old = saved
                    ctx.world.despawn(old)
                    saved = ctx.world.spawn(1, 1)
                    assert(not rawequal(old, saved) and old ~= saved)
                    assert(state[saved] == nil and state[old] == 'player')
                    assert(not pcall(ctx.world.position, old))
                    state[saved] = 'player'
                end
            end,
        }
    "#,
    );
    let mut runtime = load(&root);
    runtime.init().unwrap();
    for _ in 0..3 {
        runtime.step(InputSnapshot::default()).unwrap();
    }
    assert_eq!(runtime.kernel().entities().unwrap().len(), 1);
}

#[test]
fn clock_normalization_preserves_meaningful_fractions_and_input_edges() {
    let root = game(INPUT_GAME);
    for offset in [-1e-10, 1e-10] {
        let mut runtime = load(&root);
        runtime.init().unwrap();
        let report = runtime
            .frame(
                FIXED_DT * (1.0 + offset),
                InputSnapshot::held([Button::Right]),
            )
            .unwrap();
        if offset < 0.0 {
            assert_eq!(report.ticks, 0);
            assert!(report.alpha < 1.0 && report.alpha > 1.0 - 2e-10);
            assert_eq!(runtime.take_logs(), ["D:true:true:false"]);
            let next = runtime
                .frame(FIXED_DT * 2e-10, InputSnapshot::held([Button::Right]))
                .unwrap();
            assert_eq!(next.ticks, 1);
            assert_eq!(
                runtime.take_logs(),
                ["U:true:true:false", "D:true:false:false"]
            );
        } else {
            assert_eq!(report.ticks, 1);
            assert!(report.alpha > 0.0 && report.alpha < 2e-10);
        }
    }
    let mut runtime = load(&root);
    runtime.init().unwrap();
    for _ in 0..20 {
        assert_eq!(
            runtime
                .frame(FIXED_DT * 1e-13, InputSnapshot::default())
                .unwrap()
                .ticks,
            0
        );
    }
    // Tiny elapsed inputs must accumulate rather than being individually zeroed.
    assert!(runtime.frame(0.0, InputSnapshot::default()).unwrap().alpha > 1e-12);
}

#[test]
fn caught_spawn_allocation_failure_leaves_no_unreturned_entity() {
    let root = game(include_str!("fixtures/spawn_allocation.luau"));
    for memory_bytes in [1024 * 1024, 64 * 1024 * 1024] {
        let mut runtime = GameRuntime::load(
            root.path(),
            ScriptLimits {
                memory_bytes,
                startup_timeout: Duration::from_secs(2),
                ..ScriptLimits::default()
            },
        )
        .unwrap();
        runtime.init().unwrap();
        let logs = runtime.take_logs();
        assert_eq!(logs[0], "spawn failed");
        assert!(logs[1].contains("memory"), "{logs:?}");
        let successes: usize = logs[2].parse().unwrap();
        assert!(successes > 0 && successes < 1024);
        assert_eq!(
            runtime.kernel().entities().unwrap().len(),
            successes,
            "heap limit {memory_bytes}"
        );
        runtime.step(InputSnapshot::default()).unwrap();
        assert_eq!(runtime.state(), ScriptState::Running);
    }
}
