#![cfg(feature = "scripting")]

//! The shipped sprite sample, driven through the same runtime the Player uses.
//!
//! These load `examples/games/sprites` itself rather than a fixture copy, so
//! the committed bundle is what is under test. Expected positions, frames and
//! sheet cells are stated here from the sample's documented layout, not read
//! back out of it.

use protogine::{
    assets::ImageId,
    drawing::{DrawCommand, Sprite},
    input::{Button, Buttons, InputSnapshot},
    kernel::FIXED_DT,
    runtime::GameRuntime,
    scripting::ScriptLimits,
};
use std::{collections::HashMap, path::PathBuf, time::Duration};

/// The sample's fixed-screen layout: a 30x17 grid of 16-pixel sheet cells drawn
/// at 2x, with a status band underneath.
const COLUMNS: u32 = 30;
const ROWS: u32 = 17;
const CELL: u32 = 16;
const SIZE: f32 = 32.0;
const SPAWN: (f32, f32) = (448.0, 256.0);
const SPEED: f32 = 2.0;
/// Walking right from the spawn stops here, against the fence at column 21.
const FENCE_X: f32 = 640.0;
/// The committed art. These are what tie an image ID back to a logical path,
/// so they are stated here and must differ from each other.
const TILES_SIZE: (u32, u32) = (784, 352);
const CHARACTER_SIZE: (u32, u32) = (32, 16);
/// The color the sample fills the character's square with before its sheet
/// arrives, which is also the tint it draws the sprite with afterwards.
const PLACEHOLDER_HERO: [f32; 4] = [0.98, 0.86, 0.42, 1.0];
/// Sheet cells the room's legend names, as (column, row) in `tiles.png`.
const WALL_CELL: (u32, u32) = (8, 5);
const FLOOR_CELL: (u32, u32) = (2, 0);
const LEGEND_ENTRIES: usize = 5;
/// One clear, one sprite per tile, the character, the band and two bars.
const READY_COMMANDS: usize = 1 + (COLUMNS * ROWS) as usize + 1 + 5;

/// Independent of any script deadline, as the preload contract requires.
const PRELOAD: Duration = Duration::from_secs(10);

fn bundle() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/games/sprites")
}

fn load() -> GameRuntime {
    GameRuntime::load(&bundle(), ScriptLimits::default()).expect("the sample bundle should load")
}

fn sprites(runtime: &GameRuntime) -> Vec<Sprite> {
    runtime
        .draw_commands()
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Sprite(sprite) => Some(*sprite),
            _ => None,
        })
        .collect()
}

/// The dimensions the sample logged for each logical path when it became ready.
///
/// The two committed PNGs are checked against the sizes stated here, so every
/// caller of `path_of` below rests on an expectation this file owns rather than
/// on whatever the sample happened to report.
fn logged_sizes(logs: &[String]) -> HashMap<String, (u32, u32)> {
    let mut sizes = HashMap::new();
    for line in logs {
        let Some(rest) = line.strip_prefix("ready ") else {
            continue;
        };
        let mut fields = rest.split(' ');
        let path = fields.next().expect("a logged path");
        let (width, height) = fields
            .next()
            .and_then(|size| size.split_once('x'))
            .expect("a logged WxH");
        sizes.insert(
            path.to_owned(),
            (width.parse().unwrap(), height.parse().unwrap()),
        );
    }
    for (path, expected) in [
        ("assets/tiles.png", TILES_SIZE),
        ("assets/character.png", CHARACTER_SIZE),
    ] {
        if let Some(logged) = sizes.get(path) {
            assert_eq!(*logged, expected, "{path} reported an unexpected size");
        }
    }
    sizes
}

/// Name the logical asset an image ID came from.
///
/// IDs intentionally differ between sessions, so nothing here may be a
/// constant. The sample reports the size it read once each image became ready,
/// and the two shipped PNGs have deliberately different dimensions, so
/// composing its path-to-size lines with the store's ID-to-size lookup names
/// every ID by the path that produced it. The uniqueness assertion is what
/// keeps that composition honest if the art is ever replaced.
fn path_of(runtime: &GameRuntime, sizes: &HashMap<String, (u32, u32)>, id: ImageId) -> String {
    let store = runtime.assets().expect("a live store");
    let size = store.size(id).expect("a live image");
    let mut named = sizes.iter().filter(|(_, logged)| **logged == size);
    let (path, _) = named.next().unwrap_or_else(|| {
        panic!("no shipped asset reported {size:?}; the sample logged {sizes:?}")
    });
    assert!(
        named.next().is_none(),
        "two shipped assets report {size:?}, so dimensions no longer identify them"
    );
    path.clone()
}

/// A key going down this tick: the edge the sample's Space handling reads.
fn tap(button: Button) -> InputSnapshot {
    InputSnapshot {
        held: Buttons::new([button]),
        pressed: Buttons::new([button]),
        released: Buttons::default(),
    }
}

/// Run one fixed tick and its draw, keeping every log line.
fn tick(runtime: &mut GameRuntime, input: InputSnapshot, logs: &mut Vec<String>) {
    runtime.step(input).expect("update should succeed");
    logs.extend(runtime.take_logs());
    runtime.draw(0.0).expect("draw should succeed");
    logs.extend(runtime.take_logs());
}

/// Reach the sample's update-time requests and settle them, which is the
/// readiness schedule capture mode and `script_host --preload` both use.
fn preload(runtime: &mut GameRuntime) -> Vec<String> {
    let mut logs = Vec::new();
    runtime.init().expect("init should succeed");
    logs.extend(runtime.take_logs());
    // Tick 1 issues the requests, the drain settles them, and tick 2's update
    // boundary is where the sample first sees them ready.
    for _ in 0..2 {
        runtime
            .frame(FIXED_DT, InputSnapshot::default())
            .expect("frame should succeed");
        logs.extend(runtime.take_logs());
        assert!(
            runtime.drain_assets(PRELOAD).expect("drain should succeed"),
            "the preload did not settle within its watchdog"
        );
        logs.extend(runtime.take_logs());
    }
    logs
}

#[test]
fn preloaded_sample_draws_its_room_and_character_from_two_separate_images() {
    let mut runtime = load();
    let logs = preload(&mut runtime);
    let sizes = logged_sizes(&logs);
    assert_eq!(
        sizes.len(),
        2,
        "both shipped images should report a size: {logs:?}"
    );
    assert_ne!(
        TILES_SIZE, CHARACTER_SIZE,
        "the shipped art must stay distinguishable by size"
    );
    assert_eq!(sizes["assets/tiles.png"], TILES_SIZE);
    assert_eq!(sizes["assets/character.png"], CHARACTER_SIZE);
    assert!(
        logs.iter()
            .any(|line| line == "requested assets/tiles.png at tick 1"),
        "the tileset should be requested from update, not init: {logs:?}"
    );

    let commands = runtime.draw_commands();
    assert_eq!(commands.len(), READY_COMMANDS);
    assert!(matches!(commands[0], DrawCommand::Clear(_)));
    let drawn = sprites(&runtime);
    assert_eq!(drawn.len(), (COLUMNS * ROWS) as usize + 1);

    // The room is one sprite per tile in row-major order, then the character.
    let (room, character) = drawn.split_at(drawn.len() - 1);
    let character = character[0];
    assert_eq!(
        path_of(&runtime, &sizes, character.image),
        "assets/character.png"
    );
    for (index, sprite) in room.iter().enumerate() {
        let column = index as u32 % COLUMNS;
        let row = index as u32 / COLUMNS;
        assert_eq!(
            path_of(&runtime, &sizes, sprite.image),
            "assets/tiles.png",
            "tile {column},{row}"
        );
        assert_eq!(
            (sprite.x, sprite.y),
            (column as f32 * SIZE, row as f32 * SIZE),
            "tile {column},{row} is out of row-major order"
        );
        assert_eq!((sprite.width, sprite.height), (SIZE, SIZE));
        assert_eq!((sprite.source.width, sprite.source.height), (CELL, CELL));
        assert!(
            !sprite.flip_x && !sprite.flip_y,
            "room tiles are never mirrored"
        );
    }

    // The border is wall and the spawn tile is floor, so a transposed grid or a
    // cell-to-pixel mistake cannot pass. Source coordinates are image pixels.
    let at = |column: u32, row: u32| room[(row * COLUMNS + column) as usize];
    let wall = (WALL_CELL.0 * CELL, WALL_CELL.1 * CELL);
    for column in 0..COLUMNS {
        for row in [0, ROWS - 1] {
            let source = at(column, row).source;
            assert_eq!((source.x, source.y), wall, "border tile {column},{row}");
        }
    }
    for row in 0..ROWS {
        for column in [0, COLUMNS - 1] {
            let source = at(column, row).source;
            assert_eq!((source.x, source.y), wall, "border tile {column},{row}");
        }
    }
    let spawn = at(SPAWN.0 as u32 / SIZE as u32, SPAWN.1 as u32 / SIZE as u32);
    assert_eq!(
        (spawn.source.x, spawn.source.y),
        (FLOOR_CELL.0 * CELL, FLOOR_CELL.1 * CELL)
    );
    let cells: std::collections::HashSet<_> = room
        .iter()
        .map(|sprite| (sprite.source.x, sprite.source.y))
        .collect();
    assert_eq!(cells.len(), LEGEND_ENTRIES, "the room used {cells:?}");

    // The character is idle at the spawn, so it shows its first frame unflipped.
    assert_eq!((character.x, character.y), SPAWN);
    assert_eq!((character.width, character.height), (SIZE, SIZE));
    assert_eq!(
        (character.source.x, character.source.y),
        (0, 0),
        "an idle character shows frame 0"
    );
    assert_eq!(
        (character.source.width, character.source.height),
        (CELL, CELL)
    );
    assert!(!character.flip_x);
    assert_ne!(
        character.image, room[0].image,
        "the room and the character are separate logical images"
    );
}

#[test]
fn image_identities_differ_between_sessions_but_name_the_same_assets() {
    let mut first = load();
    let first_logs = preload(&mut first);
    let mut second = load();
    let second_logs = preload(&mut second);

    let (first_sizes, second_sizes) = (logged_sizes(&first_logs), logged_sizes(&second_logs));
    assert_eq!(first_sizes, second_sizes);
    let first_hero = sprites(&first).pop().expect("a character sprite");
    let second_hero = sprites(&second).pop().expect("a character sprite");
    assert_eq!(
        path_of(&first, &first_sizes, first_hero.image),
        path_of(&second, &second_sizes, second_hero.image),
    );
    assert_ne!(
        first_hero.image.session(),
        second_hero.image.session(),
        "two live stores must not share a session number"
    );
}

#[test]
fn sample_replays_the_same_state_and_frame_at_30_60_and_144_fps() {
    // Movement is two pixels per tick and the animation advances every eight,
    // so 30 ticks right then 30 ticks down end 60 pixels right and 60 pixels
    // down of the spawn showing the second frame.
    let expected_position = (SPAWN.0 + 30.0 * SPEED, SPAWN.1 + 30.0 * SPEED);
    let expected_frame = CELL;
    let mut traces: Vec<HashMap<u64, (f32, f32, u32, bool)>> = Vec::new();
    for fps in [30, 60, 144] {
        let mut runtime = load();
        preload(&mut runtime);
        let settled = runtime.completed_ticks();
        let mut trace = HashMap::new();
        for frame in 0..fps {
            let button = if frame < fps / 2 {
                Button::Right
            } else {
                Button::Down
            };
            runtime
                .frame(1.0 / f64::from(fps), InputSnapshot::held([button]))
                .expect("frame should succeed");
            let hero = *sprites(&runtime).last().expect("a character sprite");
            trace.entry(runtime.completed_ticks() - settled).or_insert((
                hero.x,
                hero.y,
                hero.source.x,
                hero.flip_x,
            ));
        }
        assert_eq!(runtime.completed_ticks() - settled, 60, "{fps} FPS");
        let hero = *sprites(&runtime).last().expect("a character sprite");
        assert_eq!((hero.x, hero.y), expected_position, "{fps} FPS");
        assert_eq!(hero.source.x, expected_frame, "{fps} FPS");
        assert!(
            !hero.flip_x,
            "{fps} FPS: the last horizontal move was right"
        );
        traces.push(trace);
    }

    // Frame rates disagree about which ticks end a frame, so compare the ticks
    // they share. Sixty ticks at 30 FPS still leaves half of them in common.
    let (first, rest) = traces.split_first().expect("three traces");
    for other in rest {
        let mut shared = 0;
        for (ticks, state) in first {
            if let Some(theirs) = other.get(ticks) {
                assert_eq!(state, theirs, "tick {ticks}");
                shared += 1;
            }
        }
        assert!(shared >= 30, "only {shared} ticks were comparable");
    }
}

#[test]
fn the_character_becomes_drawable_before_the_room_while_movement_continues() {
    let mut runtime = load();
    let mut logs = Vec::new();
    runtime.init().expect("init should succeed");
    logs.extend(runtime.take_logs());
    assert_eq!(
        runtime.assets().expect("a store").counters().requests,
        0,
        "init requests nothing"
    );

    // Staged loading: every step runs exactly one bounded service pass, the
    // same one a frame grants, so the game keeps running while jobs progress.
    let mut character_ready = None;
    let mut room_ready = None;
    for step in 1..=400u32 {
        tick(
            &mut runtime,
            InputSnapshot::held([Button::Right]),
            &mut logs,
        );
        if step == 1 {
            assert_eq!(
                runtime.assets().expect("a store").counters().requests,
                2,
                "both images are requested from update at tick 1: {logs:?}"
            );
        }
        let drawn = sprites(&runtime);
        // The placeholder and the sprite share the character's position, so
        // movement is observable either way. How many ticks the tileset needs
        // depends on the build profile, so allow for the character reaching the
        // fence rather than capping the loop below that point.
        let expected_x = (SPAWN.0 + step as f32 * SPEED).min(FENCE_X);
        match drawn.len() {
            // Match the placeholder's own color too. The placeholder room draws
            // rectangles as well, and although none of its solid tiles can sit
            // at the character's position this early, that is an argument a
            // reader would otherwise have to reconstruct.
            0 => assert!(
                runtime.draw_commands().iter().any(|command| matches!(
                    command,
                    DrawCommand::Rect { x, y, color, .. }
                        if (*x, *y) == (expected_x, SPAWN.1) && *color == PLACEHOLDER_HERO
                )),
                "no character placeholder at {expected_x}, {}",
                SPAWN.1
            ),
            1 => {
                character_ready.get_or_insert(step);
                assert_eq!((drawn[0].x, drawn[0].y), (expected_x, SPAWN.1));
            }
            _ => {
                room_ready.get_or_insert(step);
                let hero = *drawn.last().expect("a character sprite");
                assert_eq!((hero.x, hero.y), (expected_x, SPAWN.1));
            }
        }
        if room_ready.is_some() {
            break;
        }
    }
    let character_ready = character_ready.expect("the character should become drawable");
    let room_ready = room_ready.expect("the room should become drawable");
    assert!(
        character_ready < room_ready,
        "one job runs at a time, so the small character precedes the tileset: \
         character at {character_ready}, room at {room_ready}"
    );
    assert!(
        logs.iter()
            .any(|line| line == "requested assets/character.png at tick 1"),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .any(|line| line.starts_with("ready assets/character.png 32x16 at tick")),
        "{logs:?}"
    );
}

#[test]
fn unloading_restores_the_placeholder_and_the_next_tick_requests_again() {
    let mut runtime = load();
    let logs = preload(&mut runtime);
    let first = sprites(&runtime).pop().expect("a character sprite");
    let sizes = logged_sizes(&logs);
    assert_eq!(
        path_of(&runtime, &sizes, first.image),
        "assets/character.png"
    );

    let mut logs = Vec::new();
    tick(&mut runtime, tap(Button::Action), &mut logs);
    assert!(sprites(&runtime).is_empty(), "unloaded images cannot draw");
    assert!(
        logs.iter().any(|line| line.starts_with("unloaded both")),
        "{logs:?}"
    );

    // The next update finds no handle and asks again, so the loading state is
    // reachable live rather than only on the first frames of a session.
    tick(&mut runtime, InputSnapshot::default(), &mut logs);
    assert_eq!(runtime.assets().expect("a store").counters().requests, 4);
    assert!(
        runtime.drain_assets(PRELOAD).expect("drain should succeed"),
        "the reload did not settle"
    );
    tick(&mut runtime, InputSnapshot::default(), &mut logs);
    let reloaded = sprites(&runtime).pop().expect("a character sprite");
    assert_eq!(
        path_of(&runtime, &logged_sizes(&logs), reloaded.image),
        "assets/character.png"
    );
    assert_ne!(
        reloaded.image, first.image,
        "a reload issues a new logical image"
    );
    assert_eq!(reloaded.image.session(), first.image.session());
}
