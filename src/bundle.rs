//! Discovery of the initial, unpacked release bundle: `game/main.luau` beside
//! the executable. Discovery does not parse or execute the script.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub struct GameBundle {
    root: PathBuf,
}

impl GameBundle {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entry_point(&self) -> PathBuf {
        self.root.join("main.luau")
    }
}

/// Finds a readable entry-point file relative to an absolute executable path.
///
/// Returns `None` when the bundle or entry point is absent. Invalid file types
/// and I/O failures are errors, rather than being silently treated as missing.
/// The caller supplies the executable path so discovery needs no global state.
pub fn discover_bundle(executable: &Path) -> io::Result<Option<GameBundle>> {
    if !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bundle discovery requires an absolute executable path",
        ));
    }
    let executable_dir = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable has no parent directory",
        )
    })?;
    let bundle = GameBundle {
        root: executable_dir.join("game"),
    };
    let root_metadata = match fs::metadata(&bundle.root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(with_path(&bundle.root, error)),
    };
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} must be a directory", bundle.root.display()),
        ));
    }
    let entry_point = bundle.entry_point();

    let metadata = match fs::metadata(&entry_point) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(with_path(&entry_point, error)),
    };
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} must be a regular file", entry_point.display()),
        ));
    }
    File::open(&entry_point).map_err(|error| with_path(&entry_point, error))?;

    Ok(Some(bundle))
}

fn with_path(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
