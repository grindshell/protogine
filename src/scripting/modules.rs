//! Bundle-rooted navigation for mlua's Luau require resolver.

use super::Budget;
use mlua::{
    Function, Lua,
    chunk::ChunkMode,
    luau::{NavigateError, Require},
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    rc::Rc,
};

const SOURCE_BYTES: u64 = 256 * 1024;
const IMPORT_DEPTH: usize = 64;
const MODULE_COUNT: usize = 256;

pub(super) struct BundleModules {
    root: PathBuf,
    current: PathBuf,
    resolved: Option<PathBuf>,
    active: Rc<RefCell<HashSet<String>>>,
    budget: Rc<Budget>,
    wrap: Function,
    loaders: RefCell<HashMap<PathBuf, Function>>,
}

impl BundleModules {
    pub(super) fn new(lua: &Lua, root: &Path, budget: Rc<Budget>) -> mlua::Result<Self> {
        if !root.is_absolute() {
            return Err(mlua::Error::runtime("script root must be absolute"));
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(mlua::Error::runtime("script root must be a directory"));
        }
        // Capture trusted helpers before any game code can replace its globals.
        // Retain source context for errors raised through nested imports.
        let wrap = lua
            .load(
                r#"
            local protect, raise, pack = xpcall, error, table.pack
            local trace, stringify = debug.traceback, tostring
            local function describe(err) return trace(stringify(err), 2) end
            return function(module, enter, leave)
                return function()
                    enter()
                    local result = pack(protect(module, describe))
                    leave()
                    if not result[1] then raise(result[2], 0) end
                    if result.n ~= 2 then raise("modules must return exactly one value", 0) end
                    return result[2]
                end
            end
        "#,
            )
            .set_name("=module_guard")
            .eval()?;
        Ok(Self {
            root,
            current: PathBuf::new(),
            resolved: None,
            active: Rc::default(),
            budget,
            wrap,
            loaders: RefCell::default(),
        })
    }

    fn checked_file(&self, path: &Path) -> mlua::Result<PathBuf> {
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            return Err(mlua::Error::runtime(
                "module must be a regular file within the bundle",
            ));
        }
        Ok(canonical)
    }

    fn navigate(&mut self, relative: PathBuf) -> Result<(), NavigateError> {
        let path = self.root.join(&relative);
        let file = path.with_extension("luau");
        // Reject ambiguity between a directory and its same-named module.
        if path.is_dir() && file.is_file() {
            return Err(NavigateError::Ambiguous);
        }
        let resolved = if path.is_dir() {
            if !path
                .canonicalize()
                .map_err(mlua::Error::from)?
                .starts_with(&self.root)
            {
                return Err(mlua::Error::runtime("module directory escapes bundle").into());
            }
            None
        } else {
            Some(self.checked_file(&file)?)
        };
        self.current = relative;
        self.resolved = resolved;
        Ok(())
    }

    fn compile(&self, lua: &Lua, path: &Path) -> mlua::Result<Function> {
        if let Some(loader) = self.loaders.borrow().get(path) {
            return Ok(loader.clone());
        }
        if self.loaders.borrow().len() >= MODULE_COUNT {
            self.budget.fault.set(Some("module count exceeded"));
            return Err(mlua::Error::runtime("module count exceeded"));
        }
        let mut source = Vec::new();
        File::open(path)?
            .take(SOURCE_BYTES + 1)
            .read_to_end(&mut source)?;
        if source.len() as u64 > SOURCE_BYTES {
            self.budget.fault.set(Some("module source limit exceeded"));
            return Err(mlua::Error::runtime("module source limit exceeded"));
        }
        let source = String::from_utf8(source).map_err(mlua::Error::external)?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(mlua::Error::external)?;
        let name = format!("@{}", relative.to_string_lossy().replace('\\', "/"));
        let module = lua
            .load(source)
            .set_name(&name)
            .set_mode(ChunkMode::Text)
            .into_function()?;
        let key = path.to_string_lossy().into_owned();
        let active = self.active.clone();
        let leave_active = active.clone();
        let leave_key = key.clone();
        let budget = self.budget.clone();
        let enter = lua.create_function(move |_, ()| {
            let mut active = active.borrow_mut();
            if active.contains(&key) {
                return Err(mlua::Error::runtime("cyclic module import"));
            }
            if active.len() >= IMPORT_DEPTH {
                budget.fault.set(Some("module import depth exceeded"));
                return Err(mlua::Error::runtime("module import depth exceeded"));
            }
            active.insert(key.clone());
            Ok(())
        })?;
        let leave = lua.create_function(move |_, ()| {
            leave_active.borrow_mut().remove(&leave_key);
            Ok(())
        })?;
        let loader: Function = self.wrap.call((module, enter, leave))?;
        self.loaders
            .borrow_mut()
            .insert(path.to_owned(), loader.clone());
        Ok(loader)
    }
}

impl Require for BundleModules {
    fn is_require_allowed(&self, name: &str) -> bool {
        name.starts_with('@')
    }
    fn reset(&mut self, name: &str) -> Result<(), NavigateError> {
        let name = name.strip_prefix('@').ok_or(NavigateError::NotFound)?;
        let name = name
            .rsplit_once(':')
            .filter(|(_, line)| line.parse::<u32>().is_ok())
            .map_or(name, |(path, _)| path);
        let path = Path::new(name);
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
            || path.extension() != Some("luau".as_ref())
        {
            return Err(NavigateError::NotFound);
        }
        self.navigate(path.with_extension(""))
    }
    fn jump_to_alias(&mut self, _: &str) -> Result<(), NavigateError> {
        Err(NavigateError::NotFound)
    }
    fn to_parent(&mut self) -> Result<(), NavigateError> {
        let mut parent = self.current.clone();
        if !parent.pop() {
            return Err(NavigateError::NotFound);
        }
        self.navigate(parent)
    }
    fn to_child(&mut self, name: &str) -> Result<(), NavigateError> {
        if name.is_empty() || name.contains(['/', '\\', ':', '.']) {
            return Err(
                mlua::Error::runtime("require names use extensionless path segments").into(),
            );
        }
        self.navigate(self.current.join(name))
    }
    fn has_module(&self) -> bool {
        self.resolved.is_some()
    }
    fn cache_key(&self) -> String {
        self.resolved
            .as_ref()
            .expect("resolver has a module")
            .to_string_lossy()
            .into_owned()
    }
    fn has_config(&self) -> bool {
        false
    }
    fn config(&self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "module aliases are disabled",
        ))
    }
    fn loader(&self, lua: &Lua) -> mlua::Result<Function> {
        self.compile(
            lua,
            self.resolved
                .as_ref()
                .ok_or_else(|| mlua::Error::runtime("module not resolved"))?,
        )
    }
}
