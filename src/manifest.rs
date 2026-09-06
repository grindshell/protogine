//! Versioned, optional game.tot declarations. Parsing never executes native code.

use std::{
    collections::HashSet,
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
};

pub const MANIFEST_BYTES: u64 = 64 * 1024;
pub const PLUGIN_LIMIT: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDeclaration {
    pub id: String,
    pub library: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameManifest {
    pub plugins: Vec<PluginDeclaration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);
impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "game.tot: {}", self.0)
    }
}
impl std::error::Error for ManifestError {}
fn error(message: impl ToString) -> ManifestError {
    ManifestError(message.to_string())
}

impl GameManifest {
    pub fn load(root: &Path) -> Result<Self, ManifestError> {
        let root = canonical_root(root)?;
        let path = root.join("game.tot");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(error(e)),
            Ok(meta) => reject_link(&meta)?,
        }
        if !path.is_file() {
            return Err(error("manifest must be a regular file"));
        }
        let mut source = String::new();
        fs::File::open(&path)
            .map_err(error)?
            .take(MANIFEST_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(error)?;
        Self::parse(&source)
    }

    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        if source.len() as u64 > MANIFEST_BYTES {
            return Err(error("manifest exceeds 64 KiB"));
        }
        let tot::Value::Object(root) = tot::parse(source).map_err(error)? else {
            return Err(error("manifest must be an object"));
        };
        fields(&root, &["version", "plugins"])?;
        if !matches!(root.get("version"), Some(tot::Value::Integer(n)) if n.as_i128() == Some(1)) {
            return Err(error("version must be the integer 1"));
        }
        let entries = match root.get("plugins") {
            None => return Ok(Self::default()),
            Some(tot::Value::Array(entries)) => entries,
            _ => return Err(error("plugins must be an array")),
        };
        if entries.len() > PLUGIN_LIMIT {
            return Err(error("at most 16 plugins may be declared"));
        }
        let mut ids = HashSet::new();
        let mut plugins = Vec::new();
        for entry in entries {
            let tot::Value::Object(entry) = entry else {
                return Err(error("plugin entry must be an object"));
            };
            fields(entry, &["id", "library"])?;
            let id = entry
                .get("id")
                .and_then(tot::Value::as_str)
                .ok_or_else(|| error("plugin id must be a string"))?;
            if !valid_id(id) {
                return Err(error(
                    "plugin id must be a dotted lowercase identifier of at most 128 bytes",
                ));
            }
            if !ids.insert(id) {
                return Err(error(format!("duplicate plugin id: {id}")));
            }
            let library = entry
                .get("library")
                .and_then(tot::Value::as_str)
                .ok_or_else(|| error("library must be a string"))?;
            library_segments(library)?;
            plugins.push(PluginDeclaration {
                id: id.into(),
                library: library.into(),
            });
        }
        Ok(Self { plugins })
    }

    /// Resolve every primary library before opening any. Paths below the canonical
    /// root must not be symlinks/reparse points or be changed concurrently.
    pub fn resolve_libraries(&self, root: &Path) -> Result<Vec<PathBuf>, ManifestError> {
        let root = canonical_root(root)?;
        let mut paths = HashSet::new();
        let mut result = Vec::new();
        // Revalidate caller-constructed manifests as strictly as parsed ones.
        if self.plugins.len() > PLUGIN_LIMIT {
            return Err(error("at most 16 plugins may be declared"));
        }
        let mut ids = HashSet::new();
        for plugin in &self.plugins {
            if !valid_id(&plugin.id) || !ids.insert(&plugin.id) {
                return Err(error("invalid or duplicate plugin id"));
            }
            let mut path = root.clone();
            for segment in library_segments(&plugin.library)? {
                path.push(segment);
                reject_link(
                    &fs::symlink_metadata(&path)
                        .map_err(|e| error(format!("{}: {e}", path.display())))?,
                )?;
            }
            let path = path.canonicalize().map_err(error)?;
            if !path.starts_with(&root) || !path.is_file() {
                return Err(error("library must be a regular file within the bundle"));
            }
            if !paths.insert(path.to_string_lossy().to_lowercase()) {
                return Err(error("duplicate library path"));
            }
            result.push(path);
        }
        Ok(result)
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() <= 128
        && id.contains('.')
        && id.split('.').all(|part| {
            part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn fields(map: &tot::Map, allowed: &[&str]) -> Result<(), ManifestError> {
    for (key, _) in map.iter() {
        if !allowed.contains(&key) {
            return Err(error(format!("unknown field: {key}")));
        }
    }
    Ok(())
}

fn canonical_root(root: &Path) -> Result<PathBuf, ManifestError> {
    if !root.is_absolute() {
        return Err(error("bundle root must be absolute"));
    }
    let root = root.canonicalize().map_err(error)?;
    if !root.is_dir() {
        return Err(error("bundle root must be a directory"));
    }
    Ok(root)
}

fn reject_link(meta: &fs::Metadata) -> Result<(), ManifestError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(error("reparse points are not allowed"));
        }
    }
    if meta.file_type().is_symlink() {
        return Err(error("symlinks are not allowed"));
    }
    Ok(())
}

fn library_segments(path: &str) -> Result<Vec<&str>, ManifestError> {
    if path.len() > 1024 || !path.to_ascii_lowercase().ends_with(".dll") {
        return Err(error(
            "library must be a relative .dll path of at most 1024 bytes",
        ));
    }
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts {
        if !crate::portable_path::valid_segment(part) {
            return Err(error(
                "library must use portable relative path segments without traversal",
            ));
        }
    }
    Ok(parts)
}
