//! Rooted traversal shared by the script filesystem API and asset loading.
//!
//! Both callers walk portable, slash-separated segments beneath an already
//! canonical root and refuse symlinks and reparse points at every descendant.
//! They differ only in the final-node policy, which is a parameter here rather
//! than a second copy of the traversal rules: `ctx.fs` keeps today's behavior,
//! while asset loading additionally requires a regular file, canonicalizes it,
//! and rechecks containment. As today, the root itself may have been reached
//! through a link, and this is not protection against a hostile filesystem
//! being rewritten underneath a running game.

use std::{
    fmt,
    fs::{self, Metadata},
    path::{Path, PathBuf},
};

pub(crate) const PATH_BYTES: usize = 4096;

/// Which final node a caller accepts. Traversal of intermediates is identical.
#[derive(Clone, Copy, Default)]
pub(crate) struct Policy {
    /// Accept the empty path, addressing the root directory itself.
    pub(crate) allow_root: bool,
    /// Accept a missing final node so the caller can create it.
    pub(crate) allow_missing_file: bool,
    /// Require a regular file, then canonicalize it and recheck containment.
    pub(crate) canonical_file: bool,
}

#[derive(Debug)]
pub(crate) enum PathError {
    Length,
    Segment,
    Link,
    NotDirectory,
    NotFile,
    Escape,
    Io(std::io::Error),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Length => "invalid filesystem path length",
            Self::Segment => "path must use portable relative slash-separated names",
            Self::Link => "symlinks and reparse points are not allowed",
            Self::NotDirectory => "path contains a non-directory",
            Self::NotFile => "path is not a regular file",
            Self::Escape => "resolved path left its root",
            Self::Io(error) => return write!(f, "{error}"),
        };
        f.write_str(text)
    }
}

/// Walk `path` beneath `root`, which the caller has already canonicalized.
pub(crate) fn resolve(root: &Path, path: &str, policy: Policy) -> Result<PathBuf, PathError> {
    let mut current = root.to_path_buf();
    let parts = segments(path, policy.allow_root)?;
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let last = i + 1 == parts.len();
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                reject_link(&meta)?;
                if !last && !meta.is_dir() {
                    return Err(PathError::NotDirectory);
                }
                if last && policy.canonical_file && !meta.is_file() {
                    return Err(PathError::NotFile);
                }
            }
            Err(error)
                if policy.allow_missing_file
                    && last
                    && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(PathError::Io(error)),
        }
    }
    if !policy.canonical_file {
        return Ok(current);
    }
    // The per-descendant link rejection above already refuses redirection, so
    // this recheck guards the resolved node itself rather than replacing it.
    let resolved = current.canonicalize().map_err(PathError::Io)?;
    if !resolved.starts_with(root) {
        return Err(PathError::Escape);
    }
    Ok(resolved)
}

pub(crate) fn segments(path: &str, allow_root: bool) -> Result<Vec<&str>, PathError> {
    if path.len() > PATH_BYTES || (path.is_empty() && !allow_root) {
        return Err(PathError::Length);
    }
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts {
        if !crate::portable_path::valid_segment(part) {
            return Err(PathError::Segment);
        }
    }
    Ok(parts)
}

pub(crate) fn is_link(metadata: &Metadata) -> bool {
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        link || metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    link
}

pub(crate) fn reject_link(metadata: &Metadata) -> Result<(), PathError> {
    if is_link(metadata) {
        Err(PathError::Link)
    } else {
        Ok(())
    }
}
