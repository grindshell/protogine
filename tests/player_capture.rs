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
    fs::write(&entry_point, "-- detected").unwrap();
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
}
