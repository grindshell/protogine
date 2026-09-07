#![cfg(feature = "player")]

use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn run_player(executable: &Path, cwd: &Path, capture: &Path, options: &[(&str, &str)]) -> Output {
    let mut child = Command::new(executable)
        .current_dir(cwd)
        .env_remove("PLAYER_WIDTH")
        .env_remove("PLAYER_HEIGHT")
        .env_remove("PLAYER_CAPTURE_FRAME")
        .env("PLAYER_CAPTURE", capture)
        .envs(options.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Player should launch");
    let deadline = Instant::now() + Duration::from_secs(15);
    while child
        .try_wait()
        .expect("Player status should be readable")
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "Player did not exit within 15 seconds: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The committed 2x3 fixture, whose pixels `tools/asset_fixtures.py` specifies
/// independently of this engine's decoder:
///
/// ```text
/// row 0: red opaque      | green at alpha 128
/// row 1: blue at alpha 0 | yellow opaque
/// row 2: (9,8,7) alpha 6 | white opaque
/// ```
const FIXTURE: &[u8] = include_bytes!("fixtures/assets/rgba.png");

/// Draw the whole fixture at 16x, so every source texel becomes a 16x16 block
/// whose interior can be sampled exactly under nearest filtering.
fn texel(image: &image::RgbaImage, sprite: u32, column: u32, row: u32) -> [u8; 4] {
    image
        .get_pixel(sprite * 48 + column * 16 + 8, row * 16 + 8)
        .0
}

/// Compare a sample's color channels with a one-byte rounding tolerance.
///
/// Two cases need it. A blended texel's channels depend on the backend's
/// compositing arithmetic, and a tinted one depends on how the backend turns a
/// normalized color into bytes: Macroquad truncates a `Color` to eight bits with
/// `as u8`, and the product of that byte with the texel is then rounded, so a
/// tinted expectation is not a single rounding of the authored value. Neither is
/// fixed by the sprite contract. Colors it does pin down, such as an untinted
/// opaque texel or a rectangle at 0 or 1, are asserted exactly instead.
///
/// Only the color channels are checked. Straight-alpha blending applies the
/// source alpha to the destination alpha as well, so a half-transparent texel
/// over an opaque clear leaves a framebuffer alpha of about 0.75 rather than
/// 1.0; that is the backend's compositing, not the sprite's content.
fn near(actual: [u8; 4], expected: [u8; 3], what: &str) {
    for (index, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert!(
            i16::from(*a).abs_diff(i16::from(*e)) <= 1,
            "{what} channel {index}: {actual:?} is not within 1 of {expected:?}"
        );
    }
}

#[test]
#[ignore = "requires a working graphics context and desktop; launches Player windows"]
fn captures_cropped_flipped_and_tinted_sprites_with_exact_pixels() {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const WHITE: [u8; 4] = [255; 4];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    // Green at alpha 128 composited over the opaque black clear.
    const HALF_GREEN: [u8; 3] = [0, 128, 0];

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let distribution = root.join("Exported Player");
    fs::create_dir_all(distribution.join("game/art")).unwrap();
    let executable = distribution.join(format!("protogine-player{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_protogine-player"), &executable).unwrap();
    // A shipped game needs its PNGs copied, not just its Luau source.
    fs::write(distribution.join("game/art/rgba.png"), FIXTURE).unwrap();
    let entry_point = distribution.join("game/main.luau");
    let cwd = root.join("unrelated-cwd");
    fs::create_dir_all(&cwd).unwrap();

    fs::write(
        &entry_point,
        r#"
        local image
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            draw = function(ctx)
                ctx.draw.clear(0, 0, 0, 1)
                if ctx.assets.status(image).state ~= "ready" then return end
                local whole = {width = 32, height = 48}
                ctx.draw.sprite(image, 0, 0, whole)
                ctx.draw.sprite(image, 48, 0, {width = 32, height = 48, flip_x = true})
                ctx.draw.sprite(image, 96, 0, {width = 32, height = 48, flip_y = true})
                ctx.draw.sprite(image, 144, 0, {width = 32, height = 48, flip_x = true, flip_y = true})
                -- An intervening rectangle, then one crop tinted over it.
                ctx.draw.rect(192, 0, 32, 32, 0, 0, 1, 1)
                ctx.draw.sprite(image, 192, 0, {
                    source = {x = 1, y = 2, width = 1, height = 1},
                    width = 32, height = 32,
                    tint = {r = 1, g = 0, b = 0, a = 1},
                })
            end,
        }
    "#,
    )
    .unwrap();

    let path = root.join("captures/sprites.png");
    let size = [("PLAYER_WIDTH", "256"), ("PLAYER_HEIGHT", "64")];
    let mut options = size.to_vec();
    // Frame 1 is the strict case: its readiness comes entirely from the drain
    // after init, with no earlier frame's upload service to fall back on.
    options.push(("PLAYER_CAPTURE_FRAME", "1"));
    assert_success(run_player(&executable, &cwd, &path, &options));
    let pixels = image::open(&path).unwrap().into_rgba8();
    assert_eq!(pixels.dimensions(), (256, 64));

    // Sprite 0: the source as authored. Transparent texels leave the black
    // background, and the half-alpha texel blends over it.
    assert_eq!(texel(&pixels, 0, 0, 0), RED);
    near(texel(&pixels, 0, 1, 0), HALF_GREEN, "half-alpha green");
    assert_eq!(texel(&pixels, 0, 0, 1), BLACK, "alpha 0 must not paint");
    assert_eq!(texel(&pixels, 0, 1, 1), YELLOW);
    near(texel(&pixels, 0, 0, 2), [0, 0, 0], "alpha 6 over black");
    assert_eq!(texel(&pixels, 0, 1, 2), WHITE);

    // Each flip mirrors the source inside the same destination footprint.
    assert_eq!(texel(&pixels, 1, 1, 0), RED, "flip_x swaps columns");
    assert_eq!(texel(&pixels, 1, 0, 2), WHITE);
    assert_eq!(texel(&pixels, 2, 0, 2), RED, "flip_y swaps rows");
    assert_eq!(texel(&pixels, 2, 1, 0), WHITE);
    assert_eq!(texel(&pixels, 3, 1, 2), RED, "both flips swap both axes");
    assert_eq!(texel(&pixels, 3, 0, 0), WHITE);

    // Nearest filtering at integer scale leaves no bleed across a texel edge.
    assert_eq!(pixels.get_pixel(15, 8).0, RED);
    near(
        pixels.get_pixel(17, 8).0,
        HALF_GREEN,
        "no bleed past the texel edge",
    );

    // The crop drew the white texel alone, tinted red, over the blue rectangle.
    assert_eq!(
        pixels.get_pixel(208, 16).0,
        RED,
        "tint multiplies the sample"
    );
    assert_eq!(
        pixels.get_pixel(208, 48).0,
        BLACK,
        "the crop must not cover the whole image"
    );

    // The same readiness schedule reproduces the same encoded bytes.
    let repeated = root.join("captures/sprites-repeat.png");
    assert_success(run_player(&executable, &cwd, &repeated, &options));
    assert_eq!(
        fs::read(&path).unwrap(),
        fs::read(&repeated).unwrap(),
        "repeated sprite capture bytes differ"
    );

    // Explicit eviction and reload: the image is unloaded and requested again
    // mid-run, and the later frame draws the freshly uploaded texture.
    fs::write(
        &entry_point,
        r#"
        local image, ticks = nil, 0
        return {
            init = function(ctx) image = ctx.assets.request_png("art/rgba.png") end,
            update = function(ctx)
                ticks += 1
                if ticks == 2 then
                    assert(ctx.assets.unload(image) == true)
                    image = ctx.assets.request_png("art/rgba.png")
                    ctx.log("reloaded at tick " .. ticks)
                end
            end,
            draw = function(ctx)
                ctx.draw.clear(0, 0, 0, 1)
                local status = ctx.assets.status(image)
                if status.state == "ready" and status.gpu == "resident" then
                    ctx.draw.sprite(image, 0, 0, {width = 32, height = 48})
                end
            end,
        }
    "#,
    )
    .unwrap();
    let reloaded = root.join("captures/reloaded.png");
    let mut options = size.to_vec();
    options.push(("PLAYER_CAPTURE_FRAME", "5"));
    let output = run_player(&executable, &cwd, &reloaded, &options);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("reloaded at tick 2"), "{stderr}");
    assert_success(output);
    let after = image::open(&reloaded).unwrap().into_rgba8();
    assert_eq!(
        texel(&after, 0, 0, 0),
        RED,
        "a reloaded image must upload and draw again"
    );
    assert_eq!(texel(&after, 0, 1, 2), WHITE);
}

#[test]
#[ignore = "requires a working graphics context and desktop; launches Player windows"]
fn captures_startup_states_with_repeatable_pixels_and_reliable_exit_codes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let distribution = root.join("Exported Player");
    fs::create_dir(&distribution).unwrap();
    let executable = distribution.join(format!("protogine-player{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_protogine-player"), &executable).unwrap();

    // A working-directory decoy must not affect the exported Player's discovery.
    let cwd = root.join("unrelated-cwd");
    fs::create_dir_all(cwd.join("game")).unwrap();
    fs::write(cwd.join("game/main.luau"), "-- decoy").unwrap();
    let missing_path = root.join("captures/missing.png");
    assert_success(run_player(&executable, &cwd, &missing_path, &[]));
    let missing = image::open(&missing_path).unwrap().into_rgba8();
    assert_eq!(missing.dimensions(), (960, 600));
    assert!(
        missing
            .pixels()
            .any(|pixel| pixel != missing.get_pixel(0, 0)),
        "capture must contain drawn content"
    );

    let repeated_path = root.join("captures/repeated.png");
    assert_success(run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "1")],
    ));
    assert!(
        missing == image::open(repeated_path).unwrap().into_rgba8(),
        "repeated capture pixels differ"
    );

    let small_path = root.join("captures/small.png");
    assert_success(run_player(
        &executable,
        &cwd,
        &small_path,
        &[("PLAYER_WIDTH", "400"), ("PLAYER_HEIGHT", "260")],
    ));
    assert_eq!(image::image_dimensions(small_path).unwrap(), (400, 260));

    let entry_point = distribution.join("game/main.luau");
    fs::create_dir(entry_point.parent().unwrap()).unwrap();
    fs::write(&entry_point, "return {draw=function(ctx) ctx.draw.clear(0,0,0,1); ctx.draw.rect(10,10,24,24,0,1,0,1) end}").unwrap();
    let detected_path = root.join("captures/detected.png");
    assert_success(run_player(&executable, &cwd, &detected_path, &[]));
    let detected = image::open(detected_path).unwrap().into_rgba8();
    assert!(
        missing != detected,
        "detected and missing screens must differ"
    );

    fs::remove_file(&entry_point).unwrap();
    fs::create_dir(&entry_point).unwrap();
    let invalid_path = root.join("captures/invalid.png");
    assert_success(run_player(&executable, &cwd, &invalid_path, &[]));
    let invalid = image::open(invalid_path).unwrap().into_rgba8();
    assert!(
        missing != invalid,
        "invalid and missing screens must differ"
    );
    assert!(
        detected != invalid,
        "invalid and detected screens must differ"
    );

    // An output directory cannot be overwritten with a PNG.
    let write_failure = run_player(&executable, &cwd, root, &[]);
    assert_eq!(write_failure.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&write_failure.stderr).contains("Player capture failed"));
    let bad_config_path = root.join("bad-config.png");
    let bad_config = run_player(
        &executable,
        &cwd,
        &bad_config_path,
        &[("PLAYER_WIDTH", "0")],
    );
    assert_eq!(bad_config.status.code(), Some(2));
    assert!(!bad_config_path.exists());

    fs::remove_dir(&entry_point).unwrap();
    fs::write(
        &entry_point,
        include_str!("../examples/games/tiles/main.luau"),
    )
    .unwrap();
    let palette = distribution.join("game/palette.luau");
    fs::write(
        &palette,
        include_str!("../examples/games/tiles/palette.luau"),
    )
    .unwrap();
    let sample_path = root.join("captures/sample.png");
    let output = run_player(
        &executable,
        &cwd,
        &sample_path,
        &[("PLAYER_CAPTURE_FRAME", "60")],
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Game capture: 60 completed ticks"));
    assert_success(output);
    let sample = image::open(&sample_path).unwrap().into_rgba8();
    let repeated_path = root.join("captures/sample-repeat.png");
    assert_success(run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "60")],
    ));
    assert_eq!(
        fs::read(&sample_path).unwrap(),
        fs::read(&repeated_path).unwrap(),
        "seeded PNG bytes differ"
    );
    assert_success(run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "1")],
    ));
    let first = image::open(&repeated_path).unwrap().into_rgba8();
    assert!(
        first != sample,
        "simulation should move the tile between captures"
    );
    // Headless replay places the tile at x=65 after tick 1 and x=124 after tick 60.
    let tile = sample.get_pixel(130, 145);
    assert!(tile.0[1] >= 200 && tile.0[2] >= 160);
    assert_eq!(first.get_pixel(70, 145), tile);
    assert_ne!(sample.get_pixel(70, 145), tile);

    // Edit shipped source and relaunch the same executable, without rebuilding.
    fs::write(&palette, "return {r=1,g=0,b=0}").unwrap();
    assert_success(run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "60")],
    ));
    let edited = image::open(&repeated_path).unwrap().into_rgba8();
    assert_eq!(edited.get_pixel(130, 145).0, [255, 0, 0, 255]);
    assert!(edited != sample);

    fs::write(
        &entry_point,
        r#"
        local frame = 0
        return {
            draw=function(ctx)
                frame += 1
                if frame == 1 then
                    ctx.draw.rect(0,0,30,30,1,0,0,1)
                    ctx.draw.clear(0,0,1,1)
                    ctx.draw.rect(4,4,8,8,0,1,0,0.5)
                end
            end,
            shutdown=function(ctx) ctx.log("shutdown once") end,
        }
    "#,
    )
    .unwrap();
    let output = run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "1")],
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("shutdown once")
            .count(),
        1
    );
    assert_success(output);
    let ordered = image::open(&repeated_path).unwrap().into_rgba8();
    assert_eq!(ordered.get_pixel(0, 0).0, [0, 0, 255, 255]);
    let blended = ordered.get_pixel(6, 6).0;
    assert_eq!(blended[0], 0);
    assert!((127..=128).contains(&blended[1]) && (127..=128).contains(&blended[2]));
    assert_success(run_player(
        &executable,
        &cwd,
        &repeated_path,
        &[("PLAYER_CAPTURE_FRAME", "2")],
    ));
    assert!(
        image::open(&repeated_path)
            .unwrap()
            .into_rgba8()
            .pixels()
            .all(|p| p.0 == [0, 0, 0, 255])
    );

    let mut fault_pixels = None;
    for (phase, source) in [
        ("load", "return {"),
        (
            "init",
            "return {init=function() error('init failed') end, shutdown=function(ctx) ctx.log('must skip shutdown') end}",
        ),
        (
            "update",
            "return {update=function() error('update failed') end, shutdown=function(ctx) ctx.log('must skip shutdown') end}",
        ),
        (
            "draw",
            "return {draw=function(ctx) ctx.draw.clear(1,0,0,1); ctx.draw.rect(1,1,960,600,1,1,1,1); error('draw failed') end}",
        ),
        (
            "systems",
            "return {init=function(ctx) local e=ctx.world.spawn(1.7976931348623157e308,0); ctx.world.set_velocity(e,1e308,0) end}",
        ),
        ("update", "return {update=function() while true do end end}"),
        (
            "shutdown",
            "return {draw=function(ctx) ctx.draw.clear(1,0,0,1) end, shutdown=function() error('shutdown failed') end}",
        ),
    ] {
        fs::write(&entry_point, source).unwrap();
        let output = run_player(
            &executable,
            &cwd,
            &repeated_path,
            &[("PLAYER_CAPTURE_FRAME", "1")],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(3), "{phase}: {stderr}");
        assert!(
            stderr.contains(&format!("Game error: {phase}:")),
            "{stderr}"
        );
        assert!(!stderr.contains("must skip shutdown"), "{stderr}");
        assert!(stderr.contains("Screenshot saved"), "{stderr}");
        if matches!(phase, "init" | "draw" | "shutdown") {
            assert!(
                stderr.contains("main.luau"),
                "traceback should identify game source: {stderr}"
            );
        }
        let pixels = image::open(&repeated_path).unwrap().into_rgba8();
        assert!(pixels != missing && pixels != sample);
        if let Some(previous) = &fault_pixels {
            assert!(
                previous == &pixels,
                "fault screen contains partial game rendering"
            );
        }
        fault_pixels = Some(pixels);
    }
    // A successful diagnostic PNG or a simultaneous write failure cannot mask a game fault.
    let output = run_player(&executable, &cwd, root, &[]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Game error: shutdown:") && stderr.contains("Player capture failed"),
        "{stderr}"
    );
}

/// The shipped sprite sample, exactly as `examples/games/sprites` commits it.
/// The PNGs are bytes: including them as source text would corrupt them.
const SAMPLE_MAIN: &str = include_str!("../examples/games/sprites/main.luau");
const SAMPLE_ROOM: &str = include_str!("../examples/games/sprites/room.luau");
const SAMPLE_TILES: &[u8] = include_bytes!("../examples/games/sprites/assets/tiles.png");
const SAMPLE_CHARACTER: &[u8] = include_bytes!("../examples/games/sprites/assets/character.png");

/// One opaque color, for replacing shipped art between launches. A flat image
/// makes the swap unmistakable: every tint lands on a single known product.
fn solid(path: &Path, width: u32, height: u32, color: [u8; 4]) {
    image::RgbaImage::from_pixel(width, height, image::Rgba(color))
        .save(path)
        .expect("the replacement PNG should be written");
}

#[test]
#[ignore = "requires a working graphics context and desktop; launches Player windows"]
fn captures_the_shipped_sprite_sample_from_a_copied_player() {
    // Screen samples. The room draws 16-pixel sheet cells at 2x from the top
    // left, and the character stands on its spawn tile at (448, 256), so these
    // are the sample's documented layout rather than measurements.
    const WALL_TILE: (u32, u32) = (16, 16); // Inside the corner tile (0, 0).
    // Inside the floor tile (2, 1). Only the loading frame asserts it: the
    // floor's sheet cell is transparent at this texel, so once the tileset
    // arrives the sample is correctly showing the clear color through it. The
    // headless suite covers every floor tile's source rectangle instead.
    const FLOOR_TILE: (u32, u32) = (80, 48);
    const HERO_BOTH: (u32, u32) = (459, 261); // Frame texel (5, 2), set in both frames.
    const HERO_FIRST: (u32, u32) = (453, 275); // Texel (2, 9), set only in frame 0.
    const HERO_SECOND: (u32, u32) = (453, 277); // Texel (2, 10), set only in frame 1.
    const HERO_CORNER: (u32, u32) = (449, 257); // Texel (0, 0), clear in both frames.
    const BAR: (u32, u32) = (244, 572); // Halfway along the character's status bar.
    const TILES_BAR: (u32, u32) = (716, 572); // And along the tileset's.

    // Expected colors. The sheet is exactly two colors, fully transparent and
    // (249, 250, 251), so a tinted texel is that white times the sample's tint;
    // a placeholder is the tint alone.
    const PLACEHOLDER_WALL: [u8; 3] = [51, 56, 71];
    const PLACEHOLDER_HERO: [u8; 3] = [249, 219, 107];
    const PLACEHOLDER_FLOOR: [u8; 3] = [25, 30, 40];
    const WALL: [u8; 3] = [100, 107, 125];
    const HERO: [u8; 3] = [243, 215, 105];
    const CLEAR: [u8; 3] = [10, 13, 20];
    const QUEUED_BAR: [u8; 3] = [35, 40, 53];
    const READY_BAR: [u8; 3] = [76, 198, 140];

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let distribution = root.join("Exported Player");
    fs::create_dir_all(distribution.join("game/assets")).unwrap();
    let executable = distribution.join(format!("protogine-player{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_protogine-player"), &executable).unwrap();
    fs::write(distribution.join("game/main.luau"), SAMPLE_MAIN).unwrap();
    fs::write(distribution.join("game/room.luau"), SAMPLE_ROOM).unwrap();
    let tiles = distribution.join("game/assets/tiles.png");
    let character = distribution.join("game/assets/character.png");
    fs::write(&tiles, SAMPLE_TILES).unwrap();
    fs::write(&character, SAMPLE_CHARACTER).unwrap();
    let cwd = root.join("unrelated-cwd");
    fs::create_dir_all(&cwd).unwrap();

    let sample = |pixels: &image::RgbaImage, (x, y): (u32, u32)| pixels.get_pixel(x, y).0;

    // Frame 1: the requests were issued from this frame's update, so the drain
    // that follows cannot have published them yet. This is the loading state.
    let loading_path = root.join("captures/loading.png");
    let output = run_player(
        &executable,
        &cwd,
        &loading_path,
        &[("PLAYER_CAPTURE_FRAME", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("requested assets/character.png at tick 1")
            && stderr.contains("requested assets/tiles.png at tick 1"),
        "both images are requested from update: {stderr}"
    );
    assert!(
        !stderr.contains("ready assets/"),
        "the loading capture must precede any readiness: {stderr}"
    );
    assert_success(output);
    let loading = image::open(&loading_path).unwrap().into_rgba8();
    assert_eq!(loading.dimensions(), (960, 600));
    near(
        sample(&loading, WALL_TILE),
        PLACEHOLDER_WALL,
        "loading wall",
    );
    near(
        sample(&loading, FLOOR_TILE),
        PLACEHOLDER_FLOOR,
        "loading floor",
    );
    for point in [HERO_BOTH, HERO_FIRST, HERO_SECOND, HERO_CORNER] {
        near(
            sample(&loading, point),
            PLACEHOLDER_HERO,
            "loading character",
        );
    }
    near(sample(&loading, BAR), QUEUED_BAR, "an unfilled bar");

    // Frame 2: the drain settled both jobs and their uploads, and this update
    // boundary published them, so the room and the character are drawn.
    let game_path = root.join("captures/game.png");
    let options = [("PLAYER_CAPTURE_FRAME", "2")];
    let output = run_player(&executable, &cwd, &game_path, &options);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("ready assets/tiles.png 784x352 at tick 2")
            && stderr.contains("ready assets/character.png 32x16 at tick 2"),
        "per-image completions should name their size: {stderr}"
    );
    assert!(
        stderr.contains("Game capture: 2 completed ticks"),
        "{stderr}"
    );
    assert_success(output);
    let game = image::open(&game_path).unwrap().into_rgba8();
    near(sample(&game, WALL_TILE), WALL, "a wall tile");
    near(sample(&game, HERO_BOTH), HERO, "a shared character texel");
    // Which of the two animation frames was cropped, asserted both ways.
    near(sample(&game, HERO_FIRST), HERO, "the idle frame's texel");
    near(
        sample(&game, HERO_SECOND),
        CLEAR,
        "the second frame's texel must not be drawn",
    );
    near(
        sample(&game, HERO_CORNER),
        CLEAR,
        "a transparent texel must not paint",
    );
    near(sample(&game, BAR), READY_BAR, "a filled bar");
    assert!(loading != game, "loading and loaded frames must differ");

    // The same readiness schedule reproduces the same encoded bytes.
    let repeat_path = root.join("captures/game-repeat.png");
    assert_success(run_player(&executable, &cwd, &repeat_path, &options));
    assert_eq!(
        fs::read(&game_path).unwrap(),
        fs::read(&repeat_path).unwrap(),
        "repeated sample capture bytes differ"
    );

    // Replace the spritesheet between launches, without rebuilding anything.
    // Magenta has no green, and the character's tint keeps none, so the swap
    // cannot be confused with the committed artwork.
    solid(&character, 32, 16, [255, 0, 255, 255]);
    let swapped_path = root.join("captures/swapped-character.png");
    let output = run_player(&executable, &cwd, &swapped_path, &options);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("ready assets/character.png 32x16 at tick 2"),
        "{stderr}"
    );
    assert_success(output);
    let swapped = image::open(&swapped_path).unwrap().into_rgba8();
    near(
        sample(&swapped, HERO_BOTH),
        [249, 0, 107],
        "the replaced character",
    );
    near(sample(&swapped, WALL_TILE), WALL, "the room is unchanged");

    // Replace the tileset too, at a different size. Only the cells the room
    // names have to exist, so 144x96 covers its highest cell, (8, 5).
    solid(&tiles, 144, 96, [255, 0, 255, 255]);
    let reskinned_path = root.join("captures/reskinned.png");
    let output = run_player(&executable, &cwd, &reskinned_path, &options);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("ready assets/tiles.png 144x96 at tick 2"),
        "the replaced tileset should report its own dimensions: {stderr}"
    );
    assert_success(output);
    let reskinned = image::open(&reskinned_path).unwrap().into_rgba8();
    near(
        sample(&reskinned, WALL_TILE),
        [102, 0, 127],
        "the replaced wall",
    );
    near(
        sample(&reskinned, HERO_BOTH),
        [249, 0, 107],
        "the character kept its replacement",
    );
    assert!(reskinned != game && reskinned != swapped);

    // Art that resolves but cannot decode is an inspectable failure, not a
    // session fault: the game keeps running with its placeholder and its bar
    // turns red, while the tileset beside it still loads.
    fs::write(&character, b"not a portable network graphic").unwrap();
    let failed_path = root.join("captures/failed-art.png");
    let output = run_player(&executable, &cwd, &failed_path, &options);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!stderr.contains("Game error:"), "{stderr}");
    assert!(
        !stderr.contains("ready assets/character.png"),
        "a failed job never becomes ready: {stderr}"
    );
    assert_success(output);
    let failed = image::open(&failed_path).unwrap().into_rgba8();
    near(
        sample(&failed, HERO_BOTH),
        PLACEHOLDER_HERO,
        "a failed image keeps its placeholder",
    );
    near(sample(&failed, BAR), [224, 81, 81], "a failed bar");
    near(
        sample(&failed, TILES_BAR),
        READY_BAR,
        "the tileset still loads",
    );
    near(
        sample(&failed, WALL_TILE),
        [102, 0, 127],
        "the room still draws",
    );

    // Copying the Luau without assets/ is the mistake the sample's header warns
    // about. Resolution is synchronous, so the request itself refuses and the
    // uncaught error faults the session with the logical path in the message.
    fs::remove_file(&character).unwrap();
    let output = run_player(
        &executable,
        &cwd,
        &root.join("captures/absent.png"),
        &options,
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("Game error: update:") && stderr.contains("assets/character.png"),
        "the fault should name the missing asset: {stderr}"
    );
}
