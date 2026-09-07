#![cfg(feature = "scripting")]

//! Phase 2 behavior for `ctx.assets` and `ctx.draw.sprite`: request, status,
//! size and unload contracts, canonical wrappers, per-callback budgets, the
//! publication boundary, owned sprite commands and terminal cleanup.
//!
//! No graphics context is involved. GPU residency is `unavailable` throughout
//! and rendered pixels are Phase 3; these tests cover the data and the lifetime
//! the renderer will consume. Foreign handles and rolled back publication need
//! a harness the VM cannot reach and live in `src/scripting/assets.rs`.

use protogine::{
    assets::{GpuResidency, ImageState, UploadAck},
    drawing::{DRAW_COMMAND_LIMIT, DrawCommand, SourceRect, Sprite},
    input::InputSnapshot,
    kernel::FIXED_DT,
    runtime::GameRuntime,
    scripting::{ScriptHost, ScriptLimits, ScriptState},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Independent of any script deadline, as the frozen preload contract requires.
const WATCHDOG: Duration = Duration::from_secs(10);

fn source(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// A bundle whose PNGs are copied from the committed Phase 1 fixtures, whose
/// pixels and framing are specified independently of this engine's decoder.
fn bundle(main: &str, images: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), main).unwrap();
    for (relative, fixture) in images {
        let destination = root.path().join(relative);
        fs::create_dir_all(destination.parent().expect("relative path has a parent")).unwrap();
        let from = match *fixture {
            "kenney" => source("examples/kenney_1-bit-pack_transparent-packed.png"),
            name => source(&format!("tests/fixtures/assets/{name}.png")),
        };
        fs::copy(from, destination).unwrap();
    }
    root
}

fn host(main: &str, images: &[(&str, &str)]) -> (tempfile::TempDir, ScriptHost) {
    host_with(main, images, ScriptLimits::default())
}

fn host_with(
    main: &str,
    images: &[(&str, &str)],
    limits: ScriptLimits,
) -> (tempfile::TempDir, ScriptHost) {
    let root = bundle(main, images);
    let host = ScriptHost::load(root.path(), limits).unwrap();
    (root, host)
}

fn runtime(main: &str, images: &[(&str, &str)]) -> (tempfile::TempDir, GameRuntime) {
    let root = bundle(main, images);
    let runtime = GameRuntime::load(root.path(), ScriptLimits::default()).unwrap();
    (root, runtime)
}

fn sprites(commands: &[DrawCommand]) -> Vec<Sprite> {
    commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Sprite(sprite) => Some(*sprite),
            _ => None,
        })
        .collect()
}

/// A standalone host whose 2x3 `art/rgba.png` is already ready, preloaded
/// through the same service a frame uses.
fn ready_host(main: &str) -> (tempfile::TempDir, ScriptHost) {
    let (root, mut host) = host(main, &[("art/rgba.png", "rgba")]);
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap(), "preload timed out");
    (root, host)
}

#[test]
fn retained_oversized_path_errors_are_bounded_and_recoverable() {
    let (_root, mut host) = host_with(
        r#"
        local path, errors
        return {
            init = function(ctx)
                path = string.rep("x", 1024 * 1024)
                errors = {}
            end,
            update = function(ctx)
                local ok, err = pcall(ctx.assets.request_png, path)
                assert(not ok)
                table.insert(errors, err)
                -- Keep the external errors across callbacks, then format all
                -- of them: none may carry the rejected megabyte of input.
                for _, saved in ipairs(errors) do
                    local message = tostring(saved)
                    assert(#message < 8192)
                    assert(string.find(message, "4096", 1, true))
                end
                local normal, missing = pcall(ctx.assets.request_png, "absent.png")
                assert(not normal and string.find(tostring(missing), "absent.png", 1, true))
            end,
        }
        "#,
        &[],
        ScriptLimits {
            memory_bytes: 8 * 1024 * 1024,
            ..ScriptLimits::default()
        },
    );
    host.init().unwrap();
    for _ in 0..24 {
        host.update().unwrap();
    }
    assert_eq!(host.state(), ScriptState::Running);
    assert_eq!(host.assets().counters().admitted, 0);
}

#[test]
fn a_request_publishes_a_pending_handle_and_gameplay_continues_while_loading() {
    let (_root, mut host) = host(
        r#"
        local sheet
        local states = {}
        return {
            init = function(ctx)
                sheet = ctx.assets.request_png("assets/sheet.png")
                local status = ctx.assets.status(sheet)
                -- The handle is published before any content is read.
                assert(status.state == "queued", status.state)
                assert(status.stage == "waiting" and status.bytes_read == 0)
                assert(status.gpu == "unavailable" and status.error == nil)
                assert(status.width == nil and status.height == nil)
                -- Dimensions are refused until the header is validated.
                assert(not pcall(ctx.assets.size, sheet))
            end,
            update = function(ctx)
                local status = ctx.assets.status(sheet)
                if states[#states] ~= status.state then
                    states[#states + 1] = status.state
                end
                if status.state == "ready" then
                    local size = ctx.assets.size(sheet)
                    assert(size.width == 784 and size.height == 352)
                    assert(status.width == 784 and status.height == 352)
                    assert(status.stage == "complete" and status.bytes_read == 17497)
                end
            end,
            shutdown = function(ctx) ctx.log(table.concat(states, ",")) end,
        }
    "#,
        &[("assets/sheet.png", "kenney")],
    );
    host.init().unwrap();
    // One pass cannot finish a sheet that needs several grants, so the first
    // update must observe an image still loading rather than a completed one.
    host.update().unwrap();
    assert_eq!(
        host.assets().counters().completed,
        0,
        "the first update completed the sheet"
    );
    let mut updates = 1;
    while host.assets().counters().completed == 0 {
        assert!(updates < 500, "the sheet never completed through updates");
        host.update().unwrap();
        updates += 1;
    }
    // Loading really was spread across callbacks rather than blocking one.
    assert!(updates > 1, "loading completed inside a single update");
    host.update().unwrap();
    host.shutdown().unwrap();
    assert_eq!(host.take_logs(), ["loading,ready"]);
}

#[test]
fn coalesced_and_cached_requests_share_one_canonical_handle() {
    let (_root, mut host) = host(
        r#"
        local first, keyed
        return {
            init = function(ctx)
                first = ctx.assets.request_png("art/rgba.png")
                local again = ctx.assets.request_png("art/rgba.png")
                -- A repeat request for a pending image is the same userdata, so
                -- it satisfies rawequal and works as a table key.
                assert(rawequal(first, again))
                keyed = {}
                keyed[first] = "player"
                assert(keyed[again] == "player")
                local other = ctx.assets.request_png("art/rgb.png")
                assert(not rawequal(first, other))
                assert(keyed[other] == nil)
            end,
            update = function(ctx)
                if ctx.assets.status(first).state ~= "ready" then return end
                -- A ready cache hit returns the same wrapper too.
                local hit = ctx.assets.request_png("art/rgba.png")
                assert(rawequal(first, hit))
                assert(keyed[hit] == "player")
                assert(ctx.assets.size(hit).width == 2)
            end,
        }
    "#,
        &[("art/rgba.png", "rgba"), ("art/rgb.png", "rgb")],
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    let counters = host.assets().counters();
    assert_eq!(counters.requests, 4);
    assert_eq!(counters.admitted, 2);
    // Two requests reused a live image. Whether that is the memoized spelling
    // or the canonical path depends on the spelling used, and a distinct
    // spelling for one file is platform-dependent; both return one wrapper.
    assert_eq!(counters.spelling_hits + counters.coalesced, 2);
    assert_eq!(counters.completed, 2);
}

#[test]
fn requests_and_unload_require_init_or_update_while_queries_do_not() {
    let (_root, mut host) = host(
        r#"
        local image, stale
        return {
            init = function(ctx)
                image = ctx.assets.request_png("art/rgba.png")
                stale = ctx.assets.status
                assert(not pcall(function() ctx.assets.request_png = nil end))
                assert(not pcall(function() ctx.assets = {} end))
            end,
            update = function(ctx)
                -- A binding retained from a previous callback has expired.
                assert(not pcall(stale, image))
            end,
            draw = function(ctx)
                assert(ctx.assets ~= nil)
                local ok, err = pcall(ctx.assets.request_png, "art/rgb.png")
                assert(not ok and string.find(tostring(err), "requires? init or update"))
                ok, err = pcall(ctx.assets.unload, image)
                assert(not ok and string.find(tostring(err), "requires? init or update"))
                assert(ctx.assets.status(image).state == "ready")
                assert(ctx.assets.size(image).height == 3)
            end,
            shutdown = function(ctx)
                assert(not pcall(ctx.assets.request_png, "art/rgb.png"))
                assert(not pcall(ctx.assets.unload, image))
                -- Ready metadata stays inspectable during normal shutdown.
                assert(ctx.assets.status(image).state == "ready")
                assert(ctx.assets.size(image).width == 2)
                ctx.log("shutdown queried")
            end,
        }
    "#,
        &[("art/rgba.png", "rgba"), ("art/rgb.png", "rgb")],
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    host.draw(0.0).unwrap();
    host.shutdown().unwrap();
    assert_eq!(host.take_logs(), ["shutdown queried"]);
    // The refused draw and shutdown requests never reached the store.
    let counters = host.assets().counters();
    assert_eq!((counters.requests, counters.admitted), (1, 1));
}

#[test]
fn a_failed_job_stays_inspectable_and_stops_no_gameplay() {
    let (_root, mut host) = host(
        r#"
        local broken, ticks = nil, 0
        return {
            init = function(ctx) broken = ctx.assets.request_png("art/broken.png") end,
            update = function(ctx)
                ticks += 1
                local status = ctx.assets.status(broken)
                if status.state == "failed" then
                    assert(status.error.code == "format", status.error.code)
                    assert(status.error.path == "art/broken.png")
                    assert(#status.error.message > 0)
                    assert(status.width == nil and status.height == nil)
                    assert(not pcall(ctx.assets.size, broken))
                    ctx.log("failed after " .. ticks .. " ticks")
                end
            end,
        }
    "#,
        &[("art/broken.png", "wrong_format")],
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    assert_eq!(host.state(), ScriptState::Running);
    assert!(host.assets().service_fault().is_none());
    assert_eq!(host.assets().counters().failed, 1);
    assert_eq!(host.take_logs().len(), 1);
}

#[test]
fn settled_transitions_publish_at_the_first_update_boundary() {
    let (_root, mut runtime) = runtime(
        r#"
        local image, ticks = nil, 0
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            update = function(ctx)
                ticks += 1
                if ticks == 2 then
                    assert(ctx.assets.unload(image) == true)
                    assert(ctx.assets.unload(image) == false)
                end
            end,
            draw = function(ctx)
                if ctx.assets.status(image).state == "ready" then
                    ctx.draw.sprite(image, 0, 0)
                end
            end,
            shutdown = function(ctx)
                -- The registry slot is gone, yet the handle still reports why.
                local status = ctx.assets.status(image)
                assert(status.error == nil and not pcall(ctx.assets.size, image))
                ctx.log("retained " .. status.state .. "/" .. status.stage)
            end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    runtime.step(InputSnapshot::default()).unwrap();
    runtime.draw(0.0).unwrap();
    let id = sprites(runtime.draw_commands())[0].image;
    assert_eq!(
        runtime.assets().unwrap().status(id).unwrap().state,
        ImageState::Ready
    );

    // The second update evicts. Its transition settles after this frame's
    // commit already ran, so the entry is terminal but still holds its slot.
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(
        runtime.assets().unwrap().status(id).map(|s| s.state),
        Some(ImageState::Unloaded)
    );

    // A zero-tick frame may advance work but publishes no new snapshot.
    runtime.frame(0.0, InputSnapshot::default()).unwrap();
    assert_eq!(
        runtime.assets().unwrap().status(id).map(|s| s.state),
        Some(ImageState::Unloaded)
    );

    // The next update boundary publishes it and releases the slot.
    runtime.step(InputSnapshot::default()).unwrap();
    assert!(runtime.assets().unwrap().status(id).is_none());
    runtime.draw(0.0).unwrap();
    assert!(sprites(runtime.draw_commands()).is_empty());
    runtime.shutdown().unwrap();
    assert_eq!(runtime.take_logs(), ["retained unloaded/complete"]);
}

#[test]
fn zero_tick_frames_hold_new_snapshots_until_the_next_update() {
    let (_root, mut runtime) = runtime(
        r#"
        local image, seen = nil, {}
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                local status = ctx.assets.status(image)
                local mark = status.state .. "/" .. status.bytes_read
                if seen[#seen] ~= mark then seen[#seen + 1] = mark end
                -- `size` reads the same published view, so it stays refused for
                -- exactly as long as the status is withheld.
                assert(pcall(ctx.assets.size, image) == (status.state == "ready"))
                if status.state == "ready" then ctx.draw.sprite(image, 0, 0) end
            end,
            shutdown = function(ctx) ctx.log(table.concat(seen, ",")) end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    // Zero-tick frames advance the service but cross no update boundary, so the
    // script-visible snapshot must not move even after the store completes.
    for frame in 1..=200 {
        let report = runtime.frame(0.0, InputSnapshot::default()).unwrap();
        assert_eq!(report.ticks, 0, "frame {frame} made a tick due");
        assert!(
            sprites(runtime.draw_commands()).is_empty(),
            "frame {frame} drew an image the next update had not published"
        );
    }
    assert_eq!(
        runtime.assets().unwrap().counters().completed,
        1,
        "the store never finished loading, so nothing was withheld"
    );

    // One update boundary publishes it, and only then can draw use it.
    runtime.step(InputSnapshot::default()).unwrap();
    runtime.draw(0.0).unwrap();
    assert_eq!(sprites(runtime.draw_commands()).len(), 1);
    runtime.shutdown().unwrap();
    // Every withheld frame observed one identical snapshot, so `bytes_read` and
    // `stage` advance at the tick rate rather than the frame rate.
    assert_eq!(runtime.take_logs(), ["queued/0,ready/87"]);
}

/// The withheld case above never ticks at all. A presentation rate above the
/// fixed rate is the case a replay claim actually rests on: readiness must
/// surface on a ticking frame, never on one of the zero-tick frames between.
#[test]
fn readiness_surfaces_only_on_a_ticking_frame_at_a_higher_presentation_rate() {
    let (_root, mut runtime) = runtime(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                if ctx.assets.status(image).state == "ready" then
                    ctx.draw.sprite(image, 0, 0)
                end
            end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    // Four presentation frames per fixed tick.
    let mut drew_on = None;
    for frame in 1..=800 {
        let report = runtime
            .frame(FIXED_DT / 4.0, InputSnapshot::default())
            .unwrap();
        if !sprites(runtime.draw_commands()).is_empty() {
            drew_on = Some((frame, report.ticks));
            break;
        }
    }
    let (frame, ticks) = drew_on.expect("the image never became drawable");
    assert_eq!(ticks, 1, "frame {frame} drew without running an update");
    assert_eq!(frame % 4, 0, "frame {frame} was not a ticking frame");
}

/// The registry-slot ceiling is a per-boundary consequence of the frozen
/// terminal-slot rule, not a slow leak: releasing at each boundary must let an
/// unload-and-re-request cycle run indefinitely.
#[test]
fn slots_recycle_across_update_boundaries_without_accumulating() {
    const CYCLES: u64 = 200;
    let (_root, mut runtime) = runtime(
        r#"
        return {update = function(ctx)
            local image = ctx.assets.request_png("art/rgba.png")
            assert(ctx.assets.status(image).state == "queued")
            assert(ctx.assets.unload(image) == true)
        end}
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    for cycle in 1..=CYCLES {
        runtime
            .step(InputSnapshot::default())
            .unwrap_or_else(|error| panic!("cycle {cycle}: {error}"));
    }
    let counters = runtime.assets().unwrap().counters();
    assert_eq!(counters.admitted, CYCLES);
    assert_eq!(counters.refused, 0, "a cycle was refused for capacity");
    assert_eq!(counters.cancelled, CYCLES);
    // Each cycle burned an identity, and none of them stayed resident.
    assert_eq!(runtime.assets().unwrap().resident_bytes(), 0);
    assert!(runtime.assets().unwrap().memoized_spellings() <= 1);
}

#[test]
fn a_published_snapshot_survives_a_rejected_alpha_and_dies_with_an_uncaught_error() {
    let (_root, mut host) = ready_host(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx) ctx.draw.sprite(image, 4, 5) end,
        }
    "#,
    );
    host.draw(0.25).unwrap();
    assert_eq!(sprites(host.draw_commands()).len(), 1);
    // A refused alpha is recoverable and must not blank the accepted frame.
    for alpha in [f64::NAN, -0.0001, 1.0, f64::INFINITY] {
        assert!(host.draw(alpha).is_err(), "alpha {alpha} must be rejected");
        assert_eq!(host.state(), ScriptState::Running);
        assert_eq!(sprites(host.draw_commands()).len(), 1);
    }
    host.draw(0.75).unwrap();
    assert_eq!(sprites(host.draw_commands()).len(), 1);

    // An uncaught error discards the whole pending frame and the published list.
    let (_root, mut host) = ready_host(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                ctx.draw.sprite(image, 0, 0)
                error("draw failed")
            end,
        }
    "#,
    );
    assert!(host.draw(0.0).is_err());
    assert!(host.draw_commands().is_empty());
}

#[test]
fn requests_from_a_required_submodule_resolve_against_the_bundle_root() {
    // The decoy sits where a module-relative resolution would find it, and is a
    // different size, so a wrong root changes the answer rather than hiding.
    let root = bundle(
        r#"
        local room = require("./rooms/room")
        local image
        return {
            init = function(ctx) image = room.request(ctx) end,
            update = function(ctx)
                if ctx.assets.status(image).state ~= "ready" then return end
                local size = ctx.assets.size(image)
                ctx.log(size.width .. "x" .. size.height)
            end,
        }
    "#,
        &[
            ("art/rgba.png", "rgba"),
            ("rooms/art/rgba.png", "rgb"),
            ("rgba.png", "rgb"),
        ],
    );
    fs::write(
        root.path().join("rooms/room.luau"),
        r#"
        return {request = function(ctx)
            -- The bundle root, never the module directory, the working
            -- directory, the executable directory or the data root.
            return ctx.assets.request_png("art/rgba.png")
        end}
    "#,
    )
    .unwrap();
    let mut host = ScriptHost::load(root.path(), ScriptLimits::default()).unwrap();
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    // The bundle-root 2x3 image, not either 3x1 decoy.
    assert_eq!(host.take_logs(), ["2x3"]);
}

#[test]
fn unload_invalidates_every_alias_and_a_retry_receives_a_new_identity() {
    let (_root, mut host) = host(
        r#"
        local first, alias, second
        return {
            init = function(ctx)
                first = ctx.assets.request_png("art/rgba.png")
                alias = ctx.assets.request_png("art/rgba.png")
                assert(rawequal(first, alias))
            end,
            update = function(ctx)
                if ctx.assets.status(first).state ~= "ready" or second then return end
                assert(ctx.assets.unload(first) == true)
                -- Every alias of the logical image is invalid at once.
                assert(ctx.assets.status(alias).state == "unloaded")
                assert(not pcall(ctx.assets.size, alias))
                assert(ctx.assets.unload(alias) == false)
                -- A later request is a fresh identity, not the old handle.
                second = ctx.assets.request_png("art/rgba.png")
                assert(not rawequal(first, second))
                assert(ctx.assets.status(second).state == "queued")
                ctx.log("reloaded")
            end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    assert_eq!(host.take_logs(), ["reloaded"]);
    assert!(host.drain_assets(WATCHDOG).unwrap());
    let counters = host.assets().counters();
    assert_eq!(counters.admitted, 2);
    assert_eq!(counters.completed, 2);
    assert_eq!(counters.cancelled, 1);
    // The 87-byte file was read again, because the retry is a new image.
    assert_eq!(counters.bytes_read, 2 * 87);
}

#[test]
fn the_asset_budget_is_separate_from_the_utility_budget_and_latches() {
    let (_root, mut host) = host(
        r#"
        return {init = function(ctx)
            local image = ctx.assets.request_png("art/rgba.png")
            -- 255 further attempts reach the 256-call ceiling exactly.
            for _ = 1, 255 do ctx.assets.status(image) end
            -- Metadata queries never touched the 128-call utility budget.
            for _ = 1, 128 do ctx.data.kind(1) end
            -- The asset ceiling is latched outside protected calls.
            pcall(ctx.assets.status, image)
            error("unreachable")
        end}
    "#,
        &[("art/rgba.png", "rgba")],
    );
    let error = host.init().unwrap_err();
    assert!(
        error.message.contains("asset call limit exceeded"),
        "{error}"
    );
    assert_eq!(host.state(), ScriptState::Faulted);

    // Refusals and malformed calls count as attempts too.
    let (_root, mut host) = self::host(
        r#"
        return {init = function(ctx)
            for _ = 1, 256 do pcall(ctx.assets.request_png, "art/missing.png") end
            pcall(ctx.assets.status)
            error("unreachable")
        end}
    "#,
        &[("art/rgba.png", "rgba")],
    );
    let error = host.init().unwrap_err();
    assert!(
        error.message.contains("asset call limit exceeded"),
        "{error}"
    );
}

#[test]
fn sprite_defaults_and_explicit_options_publish_owned_commands() {
    let (_root, mut host) = ready_host(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                local options = {source = {x = 1, y = 1, width = 1, height = 2}}
                -- Omitted options use the whole image at its natural size.
                ctx.draw.sprite(image, 10, 20)
                -- An explicit source supplies the destination size by default.
                ctx.draw.sprite(image, 0, 0, options)
                ctx.draw.sprite(image, -5.5, 7.25, {
                    source = {x = 0, y = 2, width = 2, height = 1},
                    width = 8, height = 4,
                    flip_x = true, flip_y = true,
                    tint = {r = 0.25, g = 0.5, b = 0.75, a = 1},
                })
                -- Zero destination area is a valid counted no-op.
                ctx.draw.sprite(image, 1, 2, {width = 0, height = 0})
                -- Mutating the option tables afterwards cannot change a
                -- published command: values were copied during the call.
                options.source.x = 999
                options.width = 999
            end,
        }
    "#,
    );
    host.draw(0.0).unwrap();
    let drawn = sprites(host.draw_commands());
    assert_eq!(drawn.len(), 4);
    let image = drawn[0].image;
    assert!(drawn.iter().all(|sprite| sprite.image == image));
    assert_eq!(
        drawn[0],
        Sprite {
            image,
            source: SourceRect {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            },
            x: 10.0,
            y: 20.0,
            width: 2.0,
            height: 3.0,
            flip_x: false,
            flip_y: false,
            tint: [1.0; 4],
        }
    );
    assert_eq!(
        drawn[1],
        Sprite {
            image,
            source: SourceRect {
                x: 1,
                y: 1,
                width: 1,
                height: 2,
            },
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 2.0,
            flip_x: false,
            flip_y: false,
            tint: [1.0; 4],
        }
    );
    assert_eq!(
        drawn[2],
        Sprite {
            image,
            source: SourceRect {
                x: 0,
                y: 2,
                width: 2,
                height: 1,
            },
            x: -5.5,
            y: 7.25,
            width: 8.0,
            height: 4.0,
            flip_x: true,
            flip_y: true,
            tint: [0.25, 0.5, 0.75, 1.0],
        }
    );
    assert_eq!((drawn[3].width, drawn[3].height), (0.0, 0.0));
    assert!(drawn.iter().all(|sprite| sprite.check().is_ok()));
}

#[test]
fn sprite_options_reject_metatables_unknown_fields_and_coercions() {
    let (_root, mut host) = ready_host(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                local d = ctx.draw
                local function refused(options)
                    assert(not pcall(d.sprite, image, 0, 0, options), "accepted an invalid option")
                end
                refused(setmetatable({}, {__index = function() return 1 end}))
                refused({unknown = 1})
                refused({[1] = 2})
                refused({width = "8"})
                refused({flip_x = 1})
                refused({flip_y = "true"})
                refused({source = 1})
                refused({source = {x = 0, y = 0, width = 2}})
                refused({source = {x = 0, y = 0, width = 2, height = 3, extra = 1}})
                refused({source = setmetatable({x = 0, y = 0, width = 2, height = 3}, {})})
                refused({source = {x = 0.5, y = 0, width = 2, height = 3}})
                refused({source = {x = -1, y = 0, width = 2, height = 3}})
                refused({source = {x = 0, y = 0, width = 0, height = 3}})
                -- Half-open extents must fit the 2x3 image.
                refused({source = {x = 1, y = 0, width = 2, height = 3}})
                refused({source = {x = 0, y = 1, width = 2, height = 3}})
                refused({tint = {r = 1, g = 1, b = 1}})
                refused({tint = {r = 1, g = 1, b = 1, a = 1.5}})
                refused({tint = {r = 1, g = 1, b = 1, a = 0/0}})
                refused({width = -1})
                refused({height = math.huge})
                refused({width = 1000001})
                assert(not pcall(d.sprite, image, 1000001, 0))
                assert(not pcall(d.sprite, image, 0, 0/0))
                assert(not pcall(d.sprite, image))
                assert(not pcall(d.sprite, "art/rgba.png", 0, 0))
                -- Every refusal above left the pending list untouched.
                d.sprite(image, 0, 0)
                -- The far edges of the source rectangle remain valid.
                d.sprite(image, 0, 0, {source = {x = 1, y = 2, width = 1, height = 1}})
            end,
        }
    "#,
    );
    host.draw(0.0).unwrap();
    let drawn = sprites(host.draw_commands());
    assert_eq!(drawn.len(), 2);
    assert_eq!(
        drawn[1].source,
        SourceRect {
            x: 1,
            y: 2,
            width: 1,
            height: 1,
        }
    );
    assert_eq!(host.state(), ScriptState::Running);
}

#[test]
fn sprites_refuse_images_that_are_not_ready() {
    let (_root, mut host) = host(
        r#"
        local pending, broken, unloaded
        return {
            init = function(ctx)
                broken = ctx.assets.request_png("art/broken.png")
                unloaded = ctx.assets.request_png("art/rgba.png")
            end,
            update = function(ctx)
                if not pending then pending = ctx.assets.request_png("art/sheet.png") end
                if ctx.assets.status(unloaded).state == "ready" then
                    ctx.assets.unload(unloaded)
                end
            end,
            draw = function(ctx)
                for _, image in {pending, broken, unloaded} do
                    local ok, err = pcall(ctx.draw.sprite, image, 0, 0)
                    assert(not ok, "a sprite accepted an image that is not ready")
                    assert(string.find(tostring(err), "requires a ready image"), tostring(err))
                end
                ctx.draw.clear(0, 0, 0, 1)
            end,
        }
    "#,
        &[
            ("art/broken.png", "wrong_format"),
            ("art/rgba.png", "rgba"),
            ("art/sheet.png", "kenney"),
        ],
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    host.update().unwrap();
    host.draw(0.0).unwrap();
    assert!(sprites(host.draw_commands()).is_empty());
    assert_eq!(host.draw_commands().len(), 1);
    // Drawing refused the pending sheet without advancing or completing it.
    let counters = host.assets().counters();
    assert_eq!((counters.completed, counters.failed), (1, 1));
    assert_eq!(host.assets().pending_jobs(), 1);
    assert_eq!(host.state(), ScriptState::Running);
}

#[test]
fn the_sprite_attempt_budget_is_separate_from_the_asset_budget_and_latches() {
    // Ten thousand protected refusals are far slower than the drawing they
    // bound, so raise the deadline: the ceiling under test is the attempt
    // count, not the callback budget.
    let limits = ScriptLimits {
        callback_timeout: Duration::from_secs(30),
        ..ScriptLimits::default()
    };
    let refused = 1_000;
    let accepted = DRAW_COMMAND_LIMIT - refused;
    let (_root, mut host) = host_with(
        &format!(
            r#"
        local image
        return {{
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                -- Refused attempts count toward the ceiling: without them the
                -- {accepted} accepted sprites below would leave room to spare.
                for _ = 1, {refused} do pcall(ctx.draw.sprite) end
                for _ = 1, {accepted} do ctx.draw.sprite(image, 0, 0) end
                -- The binding's own dimension lookups do not come out of the
                -- 256-call asset budget, so this query still succeeds.
                ctx.assets.status(image)
                pcall(ctx.draw.sprite, image, 0, 0)
                error("unreachable")
            end,
        }}
    "#
        ),
        &[("art/rgba.png", "rgba")],
        limits,
    );
    host.init().unwrap();
    assert!(host.drain_assets(WATCHDOG).unwrap());
    let error = host.draw(0.0).unwrap_err();
    assert!(
        error.message.contains("sprite call limit exceeded"),
        "{error}"
    );
    assert!(host.draw_commands().is_empty());
}

#[test]
fn clears_rectangles_and_sprites_keep_one_order_and_one_command_cap() {
    let (_root, mut host) = ready_host(
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                ctx.draw.rect(0, 0, 4, 4, 1, 0, 0, 1)
                ctx.draw.sprite(image, 1, 1)
                ctx.draw.clear(0, 0, 1, 1)
                ctx.draw.sprite(image, 2, 2)
                ctx.draw.rect(3, 3, 4, 4, 0, 1, 0, 1)
            end,
        }
    "#,
    );
    host.draw(0.0).unwrap();
    let kinds: Vec<&str> = host
        .draw_commands()
        .iter()
        .map(|command| match command {
            DrawCommand::Clear(_) => "clear",
            DrawCommand::Rect { .. } => "rect",
            DrawCommand::Sprite(_) => "sprite",
        })
        .collect();
    assert_eq!(kinds, ["rect", "sprite", "clear", "sprite", "rect"]);

    let half = DRAW_COMMAND_LIMIT / 2;
    let (_root, mut host) = ready_host(&format!(
        r#"
        local image
        return {{
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                for _ = 1, {half} do ctx.draw.sprite(image, 0, 0) end
                for _ = 1, {half} do ctx.draw.clear(0, 0, 0, 1) end
                assert(not pcall(ctx.draw.rect, 0, 0, 1, 1, 1, 1, 1, 1))
            end,
        }}
    "#
    ));
    let error = host.draw(0.0).unwrap_err();
    assert!(
        error.message.contains("draw command limit exceeded"),
        "{error}"
    );
    assert!(host.draw_commands().is_empty());
}

#[test]
fn one_service_pass_runs_per_frame_and_drawing_advances_no_work() {
    let (_root, mut runtime) = runtime(
        r#"
        local sheet
        return {
            init = function(ctx) sheet = ctx.assets.request_png("art/sheet.png") end,
            update = function(ctx) ctx.assets.status(sheet) end,
            draw = function(ctx) ctx.assets.status(sheet) end,
        }
    "#,
        &[("art/sheet.png", "kenney")],
    );
    runtime.init().unwrap();
    let passes = |runtime: &GameRuntime| runtime.assets().unwrap().counters().service_passes;
    let work = |runtime: &GameRuntime| {
        let counters = runtime.assets().unwrap().counters();
        (
            counters.read_calls,
            counters.bytes_read,
            counters.decode_bands,
        )
    };
    assert_eq!(passes(&runtime), 0, "init grants no work");

    // Five catch-up ticks still grant exactly one pass.
    let report = runtime
        .frame(FIXED_DT * 5.0, InputSnapshot::default())
        .unwrap();
    assert_eq!(report.ticks, 5);
    assert_eq!(passes(&runtime), 1);

    // A zero-tick frame is still one pass; drawing alone is none.
    runtime.frame(0.0, InputSnapshot::default()).unwrap();
    assert_eq!(passes(&runtime), 2);
    let before = work(&runtime);
    runtime.draw(0.0).unwrap();
    assert_eq!(passes(&runtime), 2);
    assert_eq!(work(&runtime), before);

    // A refused frame grants nothing and stays recoverable.
    assert!(runtime.frame(f64::NAN, InputSnapshot::default()).is_err());
    assert_eq!(passes(&runtime), 2);
    assert_eq!(runtime.state(), ScriptState::Running);

    // One step is one pass.
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(passes(&runtime), 3);
}

#[test]
fn preloading_drains_through_the_same_service_and_never_reloads() {
    let (_root, mut runtime) = runtime(
        r#"
        local sheet
        return {
            init = function(ctx) sheet = ctx.assets.request_png("art/sheet.png") end,
            update = function(ctx)
                local status = ctx.assets.status(sheet)
                assert(status.state == "ready", status.state)
            end,
            draw = function(ctx)
                ctx.draw.sprite(sheet, 0, 0, {source = {x = 16, y = 0, width = 16, height = 16}})
            end,
        }
    "#,
        &[("art/sheet.png", "kenney")],
    );
    runtime.init().unwrap();
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    let loaded = runtime.assets().unwrap().counters();
    assert_eq!((loaded.completed, loaded.failed), (1, 0));
    assert_eq!(loaded.bytes_read, 17_497);
    assert_eq!(loaded.decode_bands, 36);
    assert!(loaded.service_passes > 1, "{loaded:?}");

    // The measured run starts with assets ready, and a completed image is
    // never read or decoded again.
    for _ in 0..3 {
        runtime.step(InputSnapshot::default()).unwrap();
        runtime.draw(0.0).unwrap();
    }
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    let after = runtime.assets().unwrap().counters();
    assert_eq!(
        (after.read_calls, after.bytes_read, after.decode_bands),
        (loaded.read_calls, loaded.bytes_read, loaded.decode_bands)
    );
    let drawn = sprites(runtime.draw_commands());
    assert_eq!(drawn.len(), 1);
    assert_eq!((drawn[0].width, drawn[0].height), (16.0, 16.0));
}

#[test]
fn ready_transitions_reach_a_renderer_exactly_once() {
    let (_root, mut runtime) = runtime(
        r#"
        local a, b
        return {
            init = function(ctx)
                a = ctx.assets.request_png("art/rgba.png")
                b = ctx.assets.request_png("art/rgb.png")
            end,
            update = function(ctx) ctx.assets.status(a) end,
        }
    "#,
        &[("art/rgba.png", "rgba"), ("art/rgb.png", "rgb")],
    );
    runtime.init().unwrap();
    // Nothing is published before a boundary, so nothing is offered for upload.
    assert!(runtime.take_ready_images().is_empty());

    // The queue is a renderer's inbox: a host without one accumulates nothing.
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    assert!(
        runtime.take_ready_images().is_empty(),
        "a host with no renderer must not queue upload work"
    );

    // Attaching after the images are already ready still offers all of them,
    // so the order a driver attaches in is not load-bearing.
    runtime.attach_gpu();
    let ready = runtime.take_ready_images();
    assert_eq!(ready.len(), 2, "both images became available");
    let session = runtime.assets().unwrap().session();
    assert!(ready.iter().all(|id| id.session() == session));
    // Taking them hands ownership to the renderer; they are never re-offered.
    assert!(runtime.take_ready_images().is_empty());
    runtime.step(InputSnapshot::default()).unwrap();
    assert!(
        runtime.take_ready_images().is_empty(),
        "a later boundary must not re-offer an already uploaded image"
    );
}

#[test]
fn acknowledgements_publish_gpu_residency_at_the_boundary() {
    let (_root, mut runtime) = runtime(
        r#"
        local image, seen = nil, {}
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            update = function(ctx)
                local status = ctx.assets.status(image)
                local mark = status.state .. "/" .. status.gpu
                if seen[#seen] ~= mark then seen[#seen + 1] = mark end
            end,
            shutdown = function(ctx) ctx.log(table.concat(seen, ",")) end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    // Without a renderer, residency is unavailable rather than pending.
    runtime.step(InputSnapshot::default()).unwrap();

    // An attached renderer that has acknowledged nothing yet reports pending.
    runtime.attach_gpu();
    runtime.step(InputSnapshot::default()).unwrap();

    let id = runtime.take_ready_images()[0];
    runtime.acknowledge_uploads(vec![UploadAck {
        id,
        residency: GpuResidency::Resident,
    }]);
    // The acknowledgement is queued, not applied: it lands with the worker
    // results at the next boundary, so one callback sees one snapshot.
    runtime.step(InputSnapshot::default()).unwrap();
    runtime.shutdown().unwrap();
    assert_eq!(
        runtime.take_logs(),
        ["ready/unavailable,ready/pending,ready/resident"]
    );
}

#[test]
fn a_late_acknowledgement_is_discarded_and_a_foreign_one_faults() {
    let (_root, mut runtime) = runtime(
        r#"
        local image, ticks = nil, 0
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            update = function(ctx)
                ticks += 1
                if ticks == 1 then assert(ctx.assets.unload(image) == true) end
            end,
            shutdown = function(ctx) ctx.log(ctx.assets.status(image).gpu) end,
        }
    "#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    assert!(runtime.drain_assets(WATCHDOG).unwrap());
    runtime.attach_gpu();
    let id = runtime.take_ready_images()[0];
    // The first update unloads it, so this acknowledgement names an image the
    // store has already forgotten by the time it is applied.
    runtime.step(InputSnapshot::default()).unwrap();
    runtime.step(InputSnapshot::default()).unwrap();
    runtime.acknowledge_uploads(vec![UploadAck {
        id,
        residency: GpuResidency::Resident,
    }]);
    runtime.step(InputSnapshot::default()).unwrap();
    assert_eq!(runtime.state(), ScriptState::Running);
    runtime.shutdown().unwrap();
    // The retained handle reports the release, not the late acknowledgement.
    assert_eq!(runtime.take_logs(), ["released"]);

    // An acknowledgement from another session means the renderer and the store
    // disagree about which session is live, which is a service fault.
    let (_other_root, mut other) = self::runtime(
        r#"return {init = function(ctx) ctx.assets.request_png("art/rgba.png") end}"#,
        &[("art/rgba.png", "rgba")],
    );
    other.attach_gpu();
    other.init().unwrap();
    assert!(other.drain_assets(WATCHDOG).unwrap());
    let foreign = other.take_ready_images()[0];

    let (_root, mut runtime) = self::runtime(
        r#"return {init = function(ctx) ctx.assets.request_png("art/rgba.png") end}"#,
        &[("art/rgba.png", "rgba")],
    );
    runtime.init().unwrap();
    runtime.acknowledge_uploads(vec![UploadAck {
        id: foreign,
        residency: GpuResidency::Resident,
    }]);
    let error = runtime.step(InputSnapshot::default()).unwrap_err();
    assert!(error.message.contains("another session"), "{error}");
    assert_eq!(runtime.state(), ScriptState::Faulted);
    assert!(runtime.assets().is_none());
}

#[test]
fn a_presentation_fault_tears_the_session_down_once() {
    let (_root, mut runtime) = runtime(
        r#"
        return {
            init = function(ctx)
                ctx.world.spawn(1, 2)
                ctx.assets.request_png("art/sheet.png")
            end,
            draw = function(ctx) ctx.draw.clear(1, 0, 0, 1) end,
            shutdown = function(ctx) ctx.log("shutdown ran") end,
        }
    "#,
        &[("art/sheet.png", "kenney")],
    );
    runtime.init().unwrap();
    runtime.draw(0.0).unwrap();
    assert_eq!(runtime.draw_commands().len(), 1);
    assert_eq!(runtime.assets().unwrap().pending_jobs(), 1);

    let error = runtime.presentation_fault("image 7 has no drawable content");
    assert_eq!(error.phase, "presentation");
    assert!(error.message.contains("no drawable content"), "{error}");
    assert_eq!(runtime.state(), ScriptState::Faulted);
    // Commands, assets, the kernel and the host are all released, and no Luau
    // shutdown ran.
    assert!(runtime.draw_commands().is_empty());
    assert!(runtime.assets().is_none());
    assert!(runtime.kernel().snapshot().is_err());
    assert!(!runtime.take_logs().iter().any(|log| log == "shutdown ran"));

    // Repeated notification preserves the first failure and tears down nothing
    // a second time.
    let again = runtime.presentation_fault("a later failure");
    assert_eq!(again.message, error.message);
    assert_eq!(runtime.state(), ScriptState::Faulted);
    assert!(runtime.take_logs().is_empty());
}

#[test]
fn stop_fault_and_drop_release_the_asset_service() {
    let (_root, mut host) = ready_host(
        r#"
        return {init = function(ctx) ctx.assets.request_png("art/rgba.png") end}
    "#,
    );
    assert_eq!(host.assets().resident_bytes(), 2 * 3 * 4);
    host.shutdown().unwrap();
    let store = host.assets();
    assert_eq!(store.pending_jobs(), 0);
    assert_eq!(store.resident_bytes(), 0);
    assert_eq!(store.staged_bytes(), 0);
    assert_eq!(store.scratch_bytes(), 0);

    // A fault invalidates immediately and runs no Luau shutdown.
    let (_root, mut runtime) = runtime(
        r#"
        return {
            init = function(ctx) ctx.assets.request_png("art/sheet.png") end,
            update = function(ctx) error("update failed") end,
            shutdown = function(ctx) ctx.log("shutdown ran") end,
        }
    "#,
        &[("art/sheet.png", "kenney")],
    );
    runtime.init().unwrap();
    assert_eq!(runtime.assets().unwrap().pending_jobs(), 1);
    assert!(runtime.step(InputSnapshot::default()).is_err());
    assert_eq!(runtime.state(), ScriptState::Faulted);
    // The host, and with it the store and its worker, are already released.
    assert!(runtime.assets().is_none());
    assert!(!runtime.take_logs().iter().any(|log| log == "shutdown ran"));

    // Dropping a host mid-load releases everything without running scripts.
    let (_root, mut host) = self::host(
        r#"
        return {
            init = function(ctx) ctx.assets.request_png("art/sheet.png") end,
            shutdown = function(ctx) error("shutdown must not run on drop") end,
        }
    "#,
        &[("art/sheet.png", "kenney")],
    );
    host.init().unwrap();
    host.update().unwrap();
    assert_eq!(host.assets().pending_jobs(), 1);
    drop(host);
}
