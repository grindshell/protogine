//! Headless Luau lifecycle, shared by the Player and development tools.

mod data;
mod drawing;
mod filesystem;
mod modules;
#[cfg(feature = "native-plugins")]
mod native;
mod utilities;
mod world;

pub(crate) use world::EngineContext;

use mlua::{
    Function, Lua, MultiValue, StdLib, Value, VmState, state::LuaOptions, thread::ThreadStatus,
};
use std::{
    cell::{Cell, RefCell},
    fmt,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::drawing::DrawCommand;
pub use crate::kernel::FIXED_DT;
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
    fault: RefCell<Option<String>>,
}

impl Budget {
    fn fail(&self, message: impl Into<String>) {
        self.fault
            .borrow_mut()
            .get_or_insert_with(|| message.into());
    }

    fn check(&self) -> Option<String> {
        if self
            .deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fail("script deadline exceeded");
        }
        self.fault.borrow().clone()
    }
}

#[derive(Default)]
struct CallbackLogs {
    lines: RefCell<Vec<String>>,
    bytes: Cell<usize>,
}

impl CallbackLogs {
    fn push(&self, budget: &Budget, message: String) -> mlua::Result<()> {
        if self.bytes.get() + message.len() + 1 > LOG_BYTES {
            budget.fail("script log limit exceeded");
            return Err(mlua::Error::runtime("script log limit exceeded"));
        }
        self.bytes.set(self.bytes.get() + message.len() + 1);
        self.lines.borrow_mut().push(message);
        Ok(())
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
    entities: world::EntityCache,
    draw_commands: Vec<DrawCommand>,
}

impl ScriptHost {
    pub fn load(root: &Path, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Self::load_with_roots(root, None, limits, None)
    }

    /// Seed math.random before evaluating any game source, for repeatable runs.
    pub fn load_seeded(root: &Path, limits: ScriptLimits, seed: i32) -> Result<Self, ScriptError> {
        Self::load_with_roots(root, None, limits, Some(seed))
    }

    /// Grant filesystem writes beneath an existing, absolute directory disjoint
    /// from the bundle. The caller selects and creates the application's location.
    pub fn load_with_data_root(
        root: &Path,
        data_root: &Path,
        limits: ScriptLimits,
    ) -> Result<Self, ScriptError> {
        Self::load_with_roots(root, Some(data_root), limits, None)
    }

    pub(crate) fn load_with_roots(
        root: &Path,
        data_root: Option<&Path>,
        limits: ScriptLimits,
        seed: Option<i32>,
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
        if let Some(seed) = seed {
            globals
                .get::<mlua::Table>("math")
                .map_err(load_error)?
                .get::<Function>("randomseed")
                .map_err(load_error)?
                .call::<()>(seed)
                .map_err(load_error)?;
        }
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
        let entities = world::EntityCache::new(&lua).map_err(load_error)?;
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
            entities,
            draw_commands: Vec::new(),
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

    /// Owned commands from the last successful draw, cleared on fault or stop.
    pub fn draw_commands(&self) -> &[DrawCommand] {
        &self.draw_commands
    }

    pub fn init(&mut self) -> Result<(), ScriptError> {
        self.init_in(None)
    }

    pub(crate) fn init_in(&mut self, engine: Option<EngineContext<'_>>) -> Result<(), ScriptError> {
        self.require_state("init", ScriptState::Loaded)?;
        self.callback(0, "init", None, self.limits.startup_timeout, engine)?;
        self.state = ScriptState::Running;
        Ok(())
    }

    /// Advance exactly one simulation tick. Frame accumulation belongs to the runtime.
    pub fn update(&mut self) -> Result<(), ScriptError> {
        self.update_in(None)
    }

    pub(crate) fn update_in(
        &mut self,
        engine: Option<EngineContext<'_>>,
    ) -> Result<(), ScriptError> {
        self.require_state("update", ScriptState::Running)?;
        self.callback(
            1,
            "update",
            Some(FIXED_DT),
            self.limits.callback_timeout,
            engine,
        )
    }

    pub fn draw(&mut self, alpha: f64) -> Result<(), ScriptError> {
        self.draw_in(alpha, None)
    }

    pub(crate) fn draw_in(
        &mut self,
        alpha: f64,
        engine: Option<EngineContext<'_>>,
    ) -> Result<(), ScriptError> {
        self.draw_commands.clear();
        self.require_state("draw", ScriptState::Running)?;
        if !alpha.is_finite() || !(0.0..1.0).contains(&alpha) {
            return Err(ScriptError {
                phase: "draw",
                message: "alpha must be finite and in [0, 1)".into(),
            });
        }
        self.callback(2, "draw", Some(alpha), self.limits.callback_timeout, engine)
    }

    pub fn shutdown(&mut self) -> Result<(), ScriptError> {
        self.shutdown_in(None)
    }

    pub(crate) fn shutdown_in(
        &mut self,
        engine: Option<EngineContext<'_>>,
    ) -> Result<(), ScriptError> {
        self.draw_commands.clear();
        match self.state {
            ScriptState::Stopped | ScriptState::Faulted => return Ok(()),
            ScriptState::Loaded => {} // Init never completed: skip game shutdown.
            ScriptState::Running => {
                self.callback(3, "shutdown", None, self.limits.startup_timeout, engine)?
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
        mut engine: Option<EngineContext<'_>>,
    ) -> Result<(), ScriptError> {
        self.logs.clear();
        let Some(function) = self.callbacks[index].clone() else {
            return Ok(());
        };
        self.budget.deadline.set(Some(Instant::now() + timeout));
        let lua = self.lua.clone();
        let budget = self.budget.clone();
        let logs = CallbackLogs::default();
        #[cfg(feature = "native-plugins")]
        let native = engine.as_mut().and_then(|e| e.plugins.take());
        // Mutable access is needed only to detach the scoped native capability.
        let _ = &mut engine;
        let utility_budget = utilities::UtilityBudget::new(&budget);
        let commands = RefCell::new(Vec::new());
        let result = catch_interrupt(|| {
            lua.scope(|scope| {
                let context = lua.create_table()?;
                let log = scope.create_function(|_, message: mlua::LuaString| {
                    if message.as_bytes().len() > 4096 {
                        budget.fail("script log limit exceeded");
                        return Err(mlua::Error::runtime("script log limit exceeded"));
                    }
                    let message = message.to_str()?.to_string();
                    logs.push(&budget, message)
                })?;
                context.raw_set("log", log)?;
                #[cfg(feature = "native-plugins")]
                if engine.is_some() {
                    context.raw_set(
                        "native",
                        native::bind(
                            &lua,
                            scope,
                            native,
                            matches!(phase, "init" | "update"),
                            &budget,
                            &logs,
                        )?,
                    )?;
                }
                if phase == "draw" {
                    context.raw_set(
                        "draw",
                        drawing::bind(&lua, scope, &utility_budget, &commands)?,
                    )?;
                }
                context.raw_set("data", self.data.bind(&lua, scope, &utility_budget)?)?;
                context.raw_set(
                    "fs",
                    self.filesystem
                        .bind(&lua, scope, &utility_budget, phase != "draw")?,
                )?;
                if let Some(engine) = &engine {
                    let (world, input) = engine.bind(
                        &lua,
                        scope,
                        &utility_budget,
                        matches!(phase, "init" | "update"),
                        &self.entities,
                    )?;
                    context.raw_set("world", world)?;
                    context.raw_set("input", input)?;
                }
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
        self.logs = logs.lines.into_inner();
        self.finish(phase, result)?;
        if phase == "draw" {
            self.draw_commands = commands.into_inner();
        }
        Ok(())
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
        result.map_err(|error| self.fault(phase, error.to_string()))
    }

    pub(crate) fn fault(&mut self, phase: &'static str, message: String) -> ScriptError {
        self.draw_commands.clear();
        let error = ScriptError { phase, message };
        self.state = ScriptState::Faulted;
        self.last_error = Some(error.clone());
        error
    }
}
