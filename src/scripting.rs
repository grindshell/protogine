//! Headless Luau lifecycle. The Player is connected in a later implementation slice.

mod data;
mod filesystem;
mod modules;
mod utilities;

use mlua::{
    Function, Lua, MultiValue, StdLib, Value, VmState, state::LuaOptions, thread::ThreadStatus,
};
use std::{
    cell::Cell,
    fmt,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

pub const FIXED_DT: f64 = 1.0 / 60.0;
const LOG_BYTES: usize = 64 * 1024;

#[cfg(panic = "abort")]
compile_error!("the scripting host requires panic=unwind for uncatchable VM cancellation");

struct Interrupted;

// mlua transports callback panics as protected VM errors, then resumes them at
// the Rust boundary. Recognize only our cancellation payload; never hide bugs.
fn catch_interrupt<T>(call: impl FnOnce() -> mlua::Result<T>) -> mlua::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(result) => result,
        Err(payload) if payload.is::<Interrupted>() => {
            Err(mlua::Error::runtime("script interrupted"))
        }
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ScriptLimits {
    pub memory_bytes: usize,
    pub startup_timeout: Duration,
    pub callback_timeout: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            startup_timeout: Duration::from_secs(1),
            callback_timeout: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptState {
    Loaded,
    Running,
    Stopped,
    Faulted,
}

#[derive(Debug, Clone)]
pub struct ScriptError {
    pub phase: &'static str,
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.phase, self.message)
    }
}
impl std::error::Error for ScriptError {}

#[derive(Default)]
struct Budget {
    deadline: Cell<Option<Instant>>,
    fault: Cell<Option<&'static str>>,
}

impl Budget {
    fn check(&self) -> Option<&'static str> {
        if self
            .deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fault.set(Some("script deadline exceeded"));
        }
        self.fault.get()
    }
}

/// One game session and one VM, confined to the calling thread.
///
/// Load -> init -> zero or more update/draw calls -> shutdown. Faults are terminal.
/// Drop releases host resources but never implicitly executes game code.
pub struct ScriptHost {
    lua: Lua,
    callbacks: [Option<Function>; 4],
    budget: Rc<Budget>,
    limits: ScriptLimits,
    state: ScriptState,
    last_error: Option<ScriptError>,
    logs: Vec<String>,
    data: data::Data,
    filesystem: filesystem::FileSystem,
}

impl ScriptHost {
    pub fn load(root: &Path, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Self::load_with_roots(root, None, limits)
    }

    /// Grant filesystem writes beneath an existing, absolute directory disjoint
    /// from the bundle. The caller selects and creates the application's location.
    pub fn load_with_data_root(
        root: &Path,
        data_root: &Path,
        limits: ScriptLimits,
    ) -> Result<Self, ScriptError> {
        Self::load_with_roots(root, Some(data_root), limits)
    }

    fn load_with_roots(
        root: &Path,
        data_root: Option<&Path>,
        limits: ScriptLimits,
    ) -> Result<Self, ScriptError> {
        let load_error = |error: mlua::Error| ScriptError {
            phase: "load",
            message: error.to_string(),
        };
        if limits.memory_bytes < 1024 * 1024
            || limits.memory_bytes > isize::MAX as usize
            || limits.startup_timeout.is_zero()
            || limits.callback_timeout.is_zero()
            || Instant::now().checked_add(limits.startup_timeout).is_none()
            || Instant::now()
                .checked_add(limits.callback_timeout)
                .is_none()
        {
            return Err(load_error(mlua::Error::runtime("invalid script limits")));
        }
        let libraries = StdLib::TABLE
            | StdLib::STRING
            | StdLib::UTF8
            | StdLib::MATH
            | StdLib::BIT
            | StdLib::BUFFER
            | StdLib::VECTOR
            | StdLib::DEBUG;
        let lua = Lua::new_with(libraries, LuaOptions::new().catch_rust_panics(false))
            .map_err(load_error)?;
        lua.set_memory_limit(limits.memory_bytes)
            .map_err(load_error)?;
        lua.enable_jit(true);
        let globals = lua.globals();
        for name in [
            "loadstring",
            "getfenv",
            "setfenv",
            "collectgarbage",
            "newproxy",
            "print",
        ] {
            globals.raw_set(name, Value::Nil).map_err(load_error)?;
        }
        let budget = Rc::new(Budget::default());
        let interrupt_budget = budget.clone();
        lua.set_interrupt(move |_| {
            // Ordinary errors are catchable, and yields cannot cross all metamethod
            // boundaries. mlua's protected panic path escapes both pcall and xpcall.
            if interrupt_budget.check().is_some() {
                std::panic::resume_unwind(Box::new(Interrupted));
            }
            Ok(VmState::Continue)
        });
        let modules = modules::BundleModules::new(root, budget.clone()).map_err(load_error)?;
        let data = data::Data::new(&lua).map_err(load_error)?;
        let filesystem = filesystem::FileSystem::new(root, data_root).map_err(load_error)?;
        globals
            .raw_set(
                "require",
                lua.create_require_function(modules).map_err(load_error)?,
            )
            .map_err(load_error)?;
        lua.sandbox(true).map_err(load_error)?;
        // Use the same resolver/cache for the entry and imported modules. The
        // bootstrap's source name supplies the bundle-relative require context.
        let entry = lua
            .load("return require('./main')")
            .set_name("@main.luau")
            .into_function()
            .map_err(load_error)?;
        let mut host = Self {
            lua,
            callbacks: [None, None, None, None],
            budget,
            limits,
            state: ScriptState::Loaded,
            last_error: None,
            logs: Vec::new(),
            data,
            filesystem,
        };
        let result = host.execute("load", entry, MultiValue::new(), limits.startup_timeout)?;
        let table = match (result.len(), result.front()) {
            (1, Some(Value::Table(table))) => table,
            _ => {
                return Err(load_error(mlua::Error::runtime(
                    "main.luau must return exactly one callback table",
                )));
            }
        };
        if table.metatable().is_some() {
            return Err(load_error(mlua::Error::runtime(
                "callback table must have no metatable",
            )));
        }
        for pair in table.clone().pairs::<Value, Value>() {
            let (key, value) = pair.map_err(load_error)?;
            let Value::String(key) = key else {
                return Err(load_error(mlua::Error::runtime(
                    "callback names must be strings",
                )));
            };
            let index = match key.to_str().map_err(load_error)?.as_ref() {
                "init" => 0,
                "update" => 1,
                "draw" => 2,
                "shutdown" => 3,
                _ => {
                    return Err(load_error(mlua::Error::runtime(format!(
                        "unknown callback: {key:?}"
                    ))));
                }
            };
            let Value::Function(function) = value else {
                return Err(load_error(mlua::Error::runtime(
                    "callbacks must be functions",
                )));
            };
            host.callbacks[index] = Some(function);
        }
        Ok(host)
    }

    pub fn state(&self) -> ScriptState {
        self.state
    }
    pub fn last_error(&self) -> Option<&ScriptError> {
        self.last_error.as_ref()
    }
    /// Logs from the most recent lifecycle call. Storage is bounded per call.
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn init(&mut self) -> Result<(), ScriptError> {
        self.require_state("init", ScriptState::Loaded)?;
        self.callback(0, "init", None, self.limits.startup_timeout)?;
        self.state = ScriptState::Running;
        Ok(())
    }

    /// Advance exactly one simulation tick. Frame accumulation belongs to the runtime.
    pub fn update(&mut self) -> Result<(), ScriptError> {
        self.require_state("update", ScriptState::Running)?;
        self.callback(1, "update", Some(FIXED_DT), self.limits.callback_timeout)
    }

    pub fn draw(&mut self, alpha: f64) -> Result<(), ScriptError> {
        self.require_state("draw", ScriptState::Running)?;
        if !alpha.is_finite() || !(0.0..1.0).contains(&alpha) {
            return Err(ScriptError {
                phase: "draw",
                message: "alpha must be finite and in [0, 1)".into(),
            });
        }
        self.callback(2, "draw", Some(alpha), self.limits.callback_timeout)
    }

    pub fn shutdown(&mut self) -> Result<(), ScriptError> {
        match self.state {
            ScriptState::Stopped | ScriptState::Faulted => return Ok(()),
            ScriptState::Loaded => {} // Init never completed: skip game shutdown.
            ScriptState::Running => {
                self.callback(3, "shutdown", None, self.limits.startup_timeout)?
            }
        }
        self.state = ScriptState::Stopped;
        Ok(())
    }

    fn require_state(&self, phase: &'static str, expected: ScriptState) -> Result<(), ScriptError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(ScriptError {
                phase,
                message: format!("expected {expected:?}, session is {:?}", self.state),
            })
        }
    }

    fn callback(
        &mut self,
        index: usize,
        phase: &'static str,
        number: Option<f64>,
        timeout: Duration,
    ) -> Result<(), ScriptError> {
        self.logs.clear();
        let Some(function) = self.callbacks[index].clone() else {
            return Ok(());
        };
        self.budget.deadline.set(Some(Instant::now() + timeout));
        let lua = self.lua.clone();
        let budget = self.budget.clone();
        let mut logs = Vec::new();
        let mut bytes = 0;
        let utility_budget = utilities::UtilityBudget::new(&budget);
        let result = catch_interrupt(|| {
            lua.scope(|scope| {
                let context = lua.create_table()?;
                let log = scope.create_function_mut(|_, message: mlua::LuaString| {
                    if message.as_bytes().len() > 4096
                        || bytes + message.as_bytes().len() + 1 > LOG_BYTES
                    {
                        budget.fault.set(Some("script log limit exceeded"));
                        return Err(mlua::Error::runtime("script log limit exceeded"));
                    }
                    let message = message.to_str()?.to_string();
                    bytes += message.len() + 1;
                    logs.push(message);
                    Ok(())
                })?;
                context.raw_set("log", log)?;
                context.raw_set("data", self.data.bind(&lua, scope, &utility_budget)?)?;
                context.raw_set(
                    "fs",
                    self.filesystem
                        .bind(&lua, scope, &utility_budget, phase != "draw")?,
                )?;
                context.set_readonly(true);
                let mut args = MultiValue::from_vec(vec![Value::Table(context)]);
                if let Some(number) = number {
                    args.push_back(Value::Number(number));
                }
                let co = lua.create_thread(function)?;
                let result = co.resume::<MultiValue>(args)?;
                if co.status() != ThreadStatus::Finished {
                    return Err(mlua::Error::runtime("game callback yielded"));
                }
                if !result.is_empty() {
                    return Err(mlua::Error::runtime(
                        "game callbacks must not return values",
                    ));
                }
                Ok(())
            })
        });
        self.logs = logs;
        self.finish(phase, result)
    }

    fn execute(
        &mut self,
        phase: &'static str,
        function: Function,
        args: MultiValue,
        timeout: Duration,
    ) -> Result<MultiValue, ScriptError> {
        self.budget.deadline.set(Some(Instant::now() + timeout));
        let result = catch_interrupt(|| {
            let co = self.lua.create_thread(function)?;
            let result = co.resume(args)?;
            if co.status() != ThreadStatus::Finished {
                return Err(mlua::Error::runtime("module yielded"));
            }
            Ok(result)
        });
        self.finish(phase, result)
    }

    fn finish<T>(
        &mut self,
        phase: &'static str,
        result: mlua::Result<T>,
    ) -> Result<T, ScriptError> {
        let fault = self.budget.check();
        self.budget.deadline.set(None);
        let result = match fault {
            Some(message) => Err(mlua::Error::runtime(message)),
            None => result,
        };
        result.map_err(|error| {
            let error = ScriptError {
                phase,
                message: error.to_string(),
            };
            self.state = ScriptState::Faulted;
            self.last_error = Some(error.clone());
            error
        })
    }
}
