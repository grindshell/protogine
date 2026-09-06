//! Explicit bundle/data roots and bounded synchronous filesystem operations.

use super::utilities::{BYTE_LIMIT, UtilityBudget};
use mlua::{FromLuaMulti, Lua, LuaString, MultiValue, Scope, Table};
use std::{
    fs::{self, File, Metadata},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(super) struct FileSystem {
    bundle: PathBuf,
    data: Option<PathBuf>,
}

impl FileSystem {
    pub(super) fn new(bundle: &Path, data: Option<&Path>) -> mlua::Result<Self> {
        let bundle = root(bundle)?;
        let data = data.map(root).transpose()?;
        if let Some(data) = &data
            && (data.starts_with(&bundle) || bundle.starts_with(data))
        {
            return Err(mlua::Error::runtime(
                "bundle and data roots must be disjoint",
            ));
        }
        Ok(Self { bundle, data })
    }

    pub(super) fn bind<'s>(
        &'s self,
        lua: &Lua,
        scope: &'s Scope<'s, '_>,
        budget: &'s UtilityBudget<'_>,
        writable: bool,
    ) -> mlua::Result<Table> {
        let api = lua.create_table()?;
        api.raw_set(
            "read",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let (root, path) = <(LuaString, LuaString)>::from_lua_multi(args, lua)?;
                let path = self.resolve(&root.to_str()?, &path.to_str()?, false, false)?;
                if !path.is_file() {
                    return Err(mlua::Error::runtime("read requires a regular file"));
                }
                let mut bytes = Vec::new();
                let read = File::open(path)?
                    .take(BYTE_LIMIT as u64 + 1)
                    .read_to_end(&mut bytes);
                budget.transfer(bytes.len())?;
                read?;
                lua.create_string(bytes)
            })?,
        )?;
        api.raw_set(
            "list",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let (root, path) = <(LuaString, LuaString)>::from_lua_multi(args, lua)?;
                let path = self.resolve(&root.to_str()?, &path.to_str()?, true, false)?;
                let mut entries = Vec::new();
                let mut bytes = 0;
                for entry in fs::read_dir(path)? {
                    let entry = entry?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| mlua::Error::runtime("directory name is not UTF-8"))?;
                    segments(&name, false)?;
                    let metadata = fs::symlink_metadata(entry.path())?;
                    reject_link(&metadata)?;
                    let kind = if metadata.is_file() {
                        "file"
                    } else if metadata.is_dir() {
                        "directory"
                    } else {
                        return Err(mlua::Error::runtime("unsupported filesystem entry"));
                    };
                    bytes += name.len();
                    budget.bytes(bytes)?;
                    entries.push((name, kind));
                    budget.limit(entries.len() > 1024, "directory entry limit exceeded")?;
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                let table = lua.create_table()?;
                for (i, (name, kind)) in entries.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.raw_set("name", name)?;
                    entry.raw_set("kind", kind)?;
                    table.raw_set(i + 1, entry)?;
                }
                budget.check()?;
                Ok(table)
            })?,
        )?;
        api.raw_set(
            "mkdir",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let path = LuaString::from_lua_multi(args, lua)?;
                self.write_allowed(writable)?;
                let mut current = self.select_root("data")?.to_path_buf();
                for part in segments(&path.to_str()?, false)? {
                    current.push(part);
                    match fs::symlink_metadata(&current) {
                        Ok(meta) => {
                            reject_link(&meta)?;
                            if !meta.is_dir() {
                                return Err(mlua::Error::runtime("mkdir path contains a file"));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            budget.check()?;
                            fs::create_dir(&current)?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(())
            })?,
        )?;
        api.raw_set(
            "write",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let (path, bytes) = <(LuaString, LuaString)>::from_lua_multi(args, lua)?;
                self.write_allowed(writable)?;
                budget.transfer(bytes.as_bytes().len())?;
                let path = self.resolve("data", &path.to_str()?, false, true)?;
                if let Ok(meta) = fs::metadata(&path)
                    && !meta.is_file()
                {
                    return Err(mlua::Error::runtime(
                        "write requires a regular file destination",
                    ));
                }
                let parent = path.parent().expect("rooted file has a parent");
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(&bytes.as_bytes())?;
                temporary.as_file().sync_all()?;
                // No destructive operation on the previous file precedes persist.
                budget.check()?;
                temporary
                    .persist(&path)
                    .map_err(|error| mlua::Error::external(error.error))?;
                Ok(())
            })?,
        )?;
        api.set_readonly(true);
        Ok(api)
    }

    fn write_allowed(&self, writable: bool) -> mlua::Result<()> {
        if !writable {
            return Err(mlua::Error::runtime(
                "filesystem writes are disabled during draw",
            ));
        }
        self.select_root("data").map(|_| ())
    }

    fn select_root(&self, name: &str) -> mlua::Result<&Path> {
        match name {
            "bundle" => Ok(&self.bundle),
            "data" => self
                .data
                .as_deref()
                .ok_or_else(|| mlua::Error::runtime("no writable data root configured")),
            _ => Err(mlua::Error::runtime(
                "filesystem root must be bundle or data",
            )),
        }
    }

    fn resolve(
        &self,
        name: &str,
        path: &str,
        allow_root: bool,
        allow_missing_file: bool,
    ) -> mlua::Result<PathBuf> {
        let mut current = self.select_root(name)?.to_path_buf();
        let parts = segments(path, allow_root)?;
        for (i, part) in parts.iter().enumerate() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) => {
                    reject_link(&meta)?;
                    if i + 1 < parts.len() && !meta.is_dir() {
                        return Err(mlua::Error::runtime("path contains a non-directory"));
                    }
                }
                Err(error)
                    if allow_missing_file
                        && i + 1 == parts.len()
                        && error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(current)
    }
}

fn root(path: &Path) -> mlua::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(mlua::Error::runtime("filesystem roots must be absolute"));
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(mlua::Error::runtime("filesystem root must be a directory"));
    }
    Ok(path)
}

fn reject_link(metadata: &Metadata) -> mlua::Result<()> {
    let is_link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let is_link = {
        use std::os::windows::fs::MetadataExt;
        is_link || metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if is_link {
        Err(mlua::Error::runtime(
            "symlinks and reparse points are not allowed",
        ))
    } else {
        Ok(())
    }
}

fn segments(path: &str, allow_root: bool) -> mlua::Result<Vec<&str>> {
    if path.len() > 4096 || (path.is_empty() && !allow_root) {
        return Err(mlua::Error::runtime("invalid filesystem path length"));
    }
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts {
        if !crate::portable_path::valid_segment(part) {
            return Err(mlua::Error::runtime(
                "path must use portable relative slash-separated names",
            ));
        }
    }
    Ok(parts)
}
