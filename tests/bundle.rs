use std::{fs, io, path::Path};

use protogine::bundle::discover_bundle;
use tempfile::tempdir;

#[test]
fn absent_bundle_and_empty_bundle_directory_are_missing() -> io::Result<()> {
    let directory = tempdir()?;
    let executable = directory.path().join("protogine-player.exe");
    assert_eq!(discover_bundle(&executable)?, None);

    fs::create_dir(directory.path().join("game"))?;
    assert_eq!(discover_bundle(&executable)?, None);
    Ok(())
}

#[test]
fn detects_entry_point_beside_executable_even_with_spaces_in_path() -> io::Result<()> {
    let directory = tempdir()?;
    let player_dir = directory.path().join("My exported game");
    let root = player_dir.join("game");
    fs::create_dir_all(&root)?;
    fs::write(root.join("main.luau"), "-- Minimal game entry point\n")?;

    let bundle = discover_bundle(&player_dir.join("protogine-player.exe"))?
        .expect("the readable entry point should be detected");
    assert_eq!(bundle.root(), root);
    assert_eq!(bundle.entry_point(), root.join("main.luau"));
    Ok(())
}

#[test]
fn does_not_search_parent_directories_for_unrelated_game_data() -> io::Result<()> {
    let directory = tempdir()?;
    fs::create_dir(directory.path().join("game"))?;
    fs::write(directory.path().join("game/main.luau"), "")?;
    let player_dir = directory.path().join("another-player");
    fs::create_dir(&player_dir)?;

    assert_eq!(
        discover_bundle(&player_dir.join("protogine-player.exe"))?,
        None
    );
    Ok(())
}

#[test]
fn file_named_game_is_invalid_game_data() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("game"), "not a directory")?;

    let error = discover_bundle(&directory.path().join("protogine-player.exe"))
        .expect_err("a file is not a bundle directory");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn directory_named_main_luau_is_invalid_game_data() -> io::Result<()> {
    let directory = tempdir()?;
    fs::create_dir_all(directory.path().join("game/main.luau"))?;

    let error = discover_bundle(&directory.path().join("protogine-player.exe"))
        .expect_err("a directory is not a script");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn relative_executable_paths_cannot_implicitly_use_the_working_directory() {
    let error = discover_bundle(Path::new("protogine-player.exe"))
        .expect_err("discovery must be independent of the working directory");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(windows)]
#[test]
fn unreadable_entry_point_is_an_error_instead_of_missing() -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = tempdir()?;
    fs::create_dir(directory.path().join("game"))?;
    let entry_point = directory.path().join("game/main.luau");
    fs::write(&entry_point, "")?;
    let _exclusive_handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&entry_point)?;

    assert!(discover_bundle(&directory.path().join("protogine-player.exe")).is_err());
    Ok(())
}
