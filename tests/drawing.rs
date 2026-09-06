#![cfg(feature = "scripting")]

use protogine::{
    drawing::{DRAW_COMMAND_LIMIT, DrawCommand},
    input::{Button, InputSnapshot},
    kernel::Position,
    runtime::GameRuntime,
    scripting::{ScriptHost, ScriptLimits, ScriptState},
};
use std::{fs, path::Path};

fn host(source: &str) -> (tempfile::TempDir, ScriptHost) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), source).unwrap();
    let host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
    (root, host)
}

#[test]
fn commands_are_owned_ordered_scoped_and_replaced_each_draw() {
    let (_root, mut host) = host(
        r#"
        local old, frame = nil, 0
        return {
            init = function(ctx) assert(ctx.draw == nil) end,
            update = function(ctx) assert(ctx.draw == nil) end,
            draw = function(ctx)
                frame += 1
                if old then assert(not pcall(old, 1, 0, 0, 1)) end
                old = ctx.draw.clear
                assert(not pcall(function() ctx.draw.rect = nil end))
                assert(not pcall(function() ctx.draw = {} end))
                if frame == 1 then
                    ctx.draw.rect(1, 2, 3, 4, 1, 0, 0, 1)
                    ctx.draw.clear(0, 0, 1, 1)
                    ctx.draw.rect(10, -20, 30, 40, 0.25, 0.5, 0.75, 1)
                end
            end,
            shutdown = function(ctx)
                assert(ctx.draw == nil)
                assert(not pcall(old, 1, 0, 0, 1))
            end,
        }
    "#,
    );
    host.init().unwrap();
    host.update().unwrap();
    host.draw(0.0).unwrap();
    let owned = host.draw_commands().to_vec();
    assert_eq!(
        owned,
        [
            DrawCommand::Rect {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
                color: [1.0, 0.0, 0.0, 1.0]
            },
            DrawCommand::Clear([0.0, 0.0, 1.0, 1.0]),
            DrawCommand::Rect {
                x: 10.0,
                y: -20.0,
                width: 30.0,
                height: 40.0,
                color: [0.25, 0.5, 0.75, 1.0]
            },
        ]
    );
    host.draw(0.5).unwrap();
    assert!(host.draw_commands().is_empty());
    host.shutdown().unwrap();
    drop(host);
    assert_eq!(owned.len(), 3);
}

#[test]
fn rejected_draw_arguments_preserve_the_published_command_list() {
    let (_root, mut host) = host(
        r#"
        return {draw = function(ctx) ctx.draw.clear(0, 0.5, 1, 1) end}
    "#,
    );
    host.init().unwrap();
    host.draw(0.25).unwrap();
    let published = [DrawCommand::Clear([0.0, 0.5, 1.0, 1.0])];
    assert_eq!(host.draw_commands(), published);
    // A refused alpha is not a session fault, so it must not silently blank the
    // frame the caller already accepted.
    for alpha in [f64::NAN, -0.0001, 1.0, f64::INFINITY] {
        assert!(host.draw(alpha).is_err(), "alpha {alpha} must be rejected");
        assert_eq!(host.state(), ScriptState::Running);
        assert_eq!(host.draw_commands(), published);
    }
    host.draw(0.75).unwrap();
    assert_eq!(host.draw_commands(), published);
    // Terminal states still clear the list; refusing above cannot leak it.
    host.shutdown().unwrap();
    assert!(host.draw_commands().is_empty());
    assert!(host.draw(0.5).is_err());
    assert!(host.draw_commands().is_empty());
}

#[test]
fn numeric_validation_is_recoverable_and_never_publishes_invalid_commands() {
    let (_root, mut host) = host(
        r#"
        return {draw = function(ctx)
            local d = ctx.draw
            for _, n in {0/0, math.huge, -math.huge, 1000001, -1000001} do
                assert(not pcall(d.rect, n, 0, 1, 1, 1, 1, 1, 1))
                assert(not pcall(d.rect, 0, n, 1, 1, 1, 1, 1, 1))
                assert(not pcall(d.rect, 0, 0, n, 1, 1, 1, 1, 1))
                assert(not pcall(d.rect, 0, 0, 1, n, 1, 1, 1, 1))
            end
            assert(not pcall(d.rect, 0, 0, -1, 1, 1, 1, 1, 1))
            assert(not pcall(d.rect, 0, 0, 1, -1, 1, 1, 1, 1))
            for _, n in {0/0, math.huge, -0.1, 1.1} do
                for i = 1, 4 do
                    local color = {1,1,1,1}; color[i] = n
                    assert(not pcall(d.clear, unpack(color)))
                    assert(not pcall(d.rect, 0, 0, 1, 1, unpack(color)))
                end
            end
            assert(not pcall(d.rect, {}, 0, 1, 1, 1, 1, 1, 1))
            d.clear(0, 1, 0, 1)
            d.rect(-1000000, 1000000, 0, 1000000, 0, 1, 0, 1)
        end}
    "#,
    );
    host.init().unwrap();
    host.draw(0.0).unwrap();
    assert_eq!(host.state(), ScriptState::Running);
    assert_eq!(host.draw_commands().len(), 2);
    assert!(host.draw(f64::NAN).is_err());
    // A refused alpha is recoverable, so the accepted list stays published.
    assert_eq!(host.draw_commands().len(), 2);
    host.draw(0.0).unwrap();
    assert_eq!(host.draw_commands().len(), 2);
}

#[test]
fn command_cap_latches_and_failed_callbacks_discard_partial_and_previous_lists() {
    let (_root, mut host) = host(&format!(
        r#"
        local frame = 0
        return {{ draw = function(ctx)
            frame += 1
            for i = 1, {DRAW_COMMAND_LIMIT} do ctx.draw.clear(0,0,0,1) end
            if frame == 2 then assert(not pcall(ctx.draw.clear, 1,1,1,1)) end
        end }}
    "#
    ));
    host.init().unwrap();
    host.draw(0.0).unwrap();
    assert_eq!(host.draw_commands().len(), DRAW_COMMAND_LIMIT);
    let error = host.draw(0.0).unwrap_err();
    assert!(
        error.message.contains("draw command limit exceeded"),
        "{error}"
    );
    assert_eq!(host.state(), ScriptState::Faulted);
    assert!(host.draw_commands().is_empty());

    for fault in ["error('draw failed')", "return 5"] {
        let (_root, mut host) = self::host(&format!(
            "return {{draw = function(ctx) ctx.draw.clear(1,0,0,1); {fault} end}}"
        ));
        host.init().unwrap();
        assert!(host.draw(0.0).is_err());
        assert!(host.draw_commands().is_empty());
    }
}

#[test]
fn runtime_faults_and_normal_shutdown_clear_published_commands() {
    for failure in [
        "error('update failed')",
        "ctx.world.set_velocity(e, 1e308, 0)",
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("main.luau"),
            format!(
                r#"
            local e
            return {{
                init = function(ctx) e = ctx.world.spawn(1.7976931348623157e308, 0) end,
                update = function(ctx) {failure} end,
                draw = function(ctx) ctx.draw.clear(1,0,0,1) end,
            }}
        "#
            ),
        )
        .unwrap();
        let mut runtime = GameRuntime::load(root.path(), ScriptLimits::default()).unwrap();
        runtime.init().unwrap();
        runtime.draw(0.0).unwrap();
        assert_eq!(runtime.draw_commands().len(), 1);
        assert!(runtime.step(InputSnapshot::default()).is_err());
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert!(runtime.draw_commands().is_empty());
        assert!(runtime.kernel().snapshot().is_err());
    }
    let (_root, mut host) = host("return {draw=function(ctx) ctx.draw.clear(1,0,0,1) end}");
    host.init().unwrap();
    host.draw(0.0).unwrap();
    host.shutdown().unwrap();
    assert!(host.draw_commands().is_empty());
}

#[test]
fn sample_seeded_state_and_commands_repeat_and_input_changes_simulation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/games/tiles");
    let replay = |seed, input| {
        let mut runtime = GameRuntime::load_seeded(&root, ScriptLimits::default(), seed).unwrap();
        runtime.init().unwrap();
        for _ in 0..60 {
            runtime.step(input).unwrap();
            runtime.draw(0.0).unwrap();
        }
        assert_eq!(runtime.completed_ticks(), 60);
        let position = runtime.kernel().snapshot().unwrap()[0].position;
        (position, runtime.draw_commands().to_vec())
    };
    let first = replay(0, InputSnapshot::default());
    assert_eq!(first.0, Position { x: 124.0, y: 128.0 });
    assert_eq!(first, replay(0, InputSnapshot::default()));
    let other_seed = replay(1, InputSnapshot::default());
    assert_eq!(first.0, other_seed.0);
    assert_ne!(first.1, other_seed.1);
    let paused = replay(0, InputSnapshot::held([Button::Action]));
    assert_eq!(paused.0, Position { x: 64.0, y: 128.0 });
    let steered = replay(0, InputSnapshot::held([Button::Down]));
    assert_eq!(steered.0, Position { x: 64.0, y: 188.0 });
}
