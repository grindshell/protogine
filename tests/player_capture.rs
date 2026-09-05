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
