//! Trusted native libraries. All foreign calls and pointer reads live here.

use crate::manifest::{GameManifest, valid_id};
use protogine_plugin_api::*;
use std::{
    ffi::c_void,
    fmt,
    marker::PhantomData,
    mem::{align_of, size_of},
    path::Path,
    ptr,
    rc::Rc,
    sync::Mutex,
    thread::{self, ThreadId},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError(pub String);
impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PluginError {}
fn error(message: impl ToString) -> PluginError {
    PluginError(message.to_string())
}

pub const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;

/// Recoverable application refusal versus a poisoned native registry.
#[derive(Clone, Debug)]
pub enum CallError {
    Rejected(PluginError),
    Fault(PluginError),
}
impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(error) | Self::Fault(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for CallError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionInfo {
    pub id: String,
    pub schema: String,
    pub schema_version: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    pub id: String,
    pub functions: Vec<FunctionInfo>,
}

#[derive(Default)]
struct HostState {
    active: bool,
    bytes: usize,
    logs: Vec<String>,
    fault: Option<&'static str>,
}
struct HostContext {
    thread: ThreadId,
    state: Mutex<HostState>,
}
impl HostContext {
    fn new() -> Self {
        Self {
            thread: thread::current().id(),
            state: Mutex::default(),
        }
    }

    fn begin(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.active = true;
        state.bytes = 0;
        state.logs.clear();
    }

    fn table(&mut self) -> PgHost {
        PgHost {
            abi_version: PG_ABI_VERSION,
            struct_size: size_of::<PgHost>() as u32,
            context: (self as *mut Self).cast(),
            log: Some(host_log),
            reserved: [0; 2],
        }
    }

    fn finish(&self) -> (Vec<String>, Option<&'static str>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.active = false;
        (std::mem::take(&mut state.logs), state.fault.take())
    }
}

// SAFETY: The trusted caller must supply the live context and readable message
// span from this host-initiated call. Validate lengths before reading foreign bytes.
unsafe extern "C" fn host_log(context: *mut c_void, level: u32, message: PgStr) -> u32 {
    let status = catch_status(|| {
        if context.is_null() {
            return PG_INVALID_ARGUMENT;
        }
        // SAFETY: Context is a stable Box kept alive around every foreign call.
        let context = unsafe { &*context.cast::<HostContext>() };
        let mut state = context.state.lock().unwrap_or_else(|p| p.into_inner());
        let failure = if context.thread != thread::current().id() || !state.active {
            Some("host logging requires the active runtime thread")
        } else if level > PG_LOG_ERROR {
            Some("invalid native log level")
        } else if message.len > 4096 || state.bytes + message.len as usize + 1 > 64 * 1024 {
            Some("native log limit exceeded")
        } else if message.len != 0 && message.data.is_null() {
            Some("null native log span")
        } else {
            None
        };
        if let Some(failure) = failure {
            state.fault = Some(failure);
            return PG_CONTRACT_ERROR;
        }
        // SAFETY: Nonzero spans were checked for null/size; readability is the ABI obligation.
        let bytes = if message.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(message.data, message.len as usize) }
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            state.fault = Some("native log is not UTF-8");
            return PG_CONTRACT_ERROR;
        };
        state.bytes += bytes.len() + 1;
        state.logs.push(format!(
            "{}: {text}",
            ["info", "warn", "error"][level as usize]
        ));
        PG_OK
    });
    if status == PG_PANIC && !context.is_null() {
        // SAFETY: Same live host context as above. A panic must remain a fault
        // even if foreign code ignores the callback's returned status.
        let context = unsafe { &*context.cast::<HostContext>() };
        context
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fault = Some("host logging panicked");
    }
    status
}

/// Diagnostics use host memory even if the plugin corrupts its returned pointer.
struct Diagnostic {
    bytes: [u8; PG_ERROR_CAPACITY as usize],
}

impl Diagnostic {
    fn new() -> Self {
        Self {
            bytes: [0; PG_ERROR_CAPACITY as usize],
        }
    }

    fn span(&mut self) -> PgError {
        PgError {
            data: self.bytes.as_mut_ptr(),
            capacity: PG_ERROR_CAPACITY,
            written: 0,
        }
    }

    fn text(&self, span: PgError) -> Result<&str, PluginError> {
        if !ptr::eq(span.data, self.bytes.as_ptr())
            || span.capacity != PG_ERROR_CAPACITY
            || span.written > span.capacity
        {
            return Err(error("plugin corrupted diagnostic pointer/capacity/length"));
        }
        std::str::from_utf8(&self.bytes[..span.written as usize])
            .map_err(|_| error("plugin diagnostic is not UTF-8"))
    }

    fn result(&self, span: PgError, status: u32) -> Result<(), PluginError> {
        let text = self.text(span)?;
        if status > PG_CONTRACT_ERROR {
            return Err(error(format!("unknown plugin status {status}")));
        }
        if status != PG_OK {
            return Err(error(format!("plugin status {status}: {text}")));
        }
        Ok(())
    }
}

struct LoadedPlugin {
    info: PluginInfo,
    calls: Vec<PgCall>,
    descriptor: PgPlugin,
    instance: *mut c_void,
    context: Box<HostContext>,
    _library: libloading::Library,
}

/// One thread-bound registry. Dropping it shuts down instances, then unloads all
/// libraries. A containing runtime must destroy its VM before dropping this set.
pub struct PluginSet {
    loaded: Vec<LoadedPlugin>,
    logs: Vec<String>,
    fault: Option<PluginError>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl PluginSet {
    /// Load/query every declaration before initializing any, then initialize in order.
    ///
    /// # Safety
    /// All declared libraries and dependencies (including DLL entry/exit routines)
    /// must be trusted, obey ABI v1 pointer/lifetime/thread rules and never unwind
    /// into the host. Keep the bundle/dependency files stable during loading.
    pub unsafe fn load_trusted(root: &Path, manifest: &GameManifest) -> Result<Self, PluginError> {
        let paths = manifest.resolve_libraries(root).map_err(error)?;
        let mut set = Self {
            loaded: Vec::new(),
            logs: Vec::new(),
            fault: None,
            _thread_bound: PhantomData,
        };
        let result = (|| {
            for (declaration, path) in manifest.plugins.iter().zip(paths) {
                // SAFETY: Caller accepts execution of these trusted libraries.
                let library = unsafe { open_library(&path) }
                    .map_err(|e| error(format!("{}: {e}", declaration.id)))?;
                // SAFETY: The sole bootstrap export must have the exact PgQuery signature.
                let query = unsafe { library.get::<PgQuery>(b"protogine_plugin_query\0") }
                    .map_err(|e| {
                        error(format!(
                            "{}: missing protogine_plugin_query: {e}",
                            declaration.id
                        ))
                    })?;
                let mut descriptor = PgPlugin::default();
                let mut diagnostic = Diagnostic::new();
                let mut span = diagnostic.span();
                // SAFETY: Destination and error buffers are host-owned and live for the call.
                let status = unsafe {
                    query(
                        PG_ABI_VERSION,
                        size_of::<PgPlugin>() as u32,
                        &mut descriptor,
                        &mut span,
                    )
                };
                diagnostic
                    .result(span, status)
                    .map_err(|e| error(format!("{} query: {e}", declaration.id)))?;
                // SAFETY: Caller guarantees valid readable descriptor spans for this library.
                let (info, calls) = unsafe { validate_descriptor(descriptor, &declaration.id) }
                    .map_err(|e| error(format!("{}: {e}", declaration.id)))?;
                set.loaded.push(LoadedPlugin {
                    info,
                    calls,
                    descriptor,
                    instance: ptr::null_mut(),
                    context: Box::new(HostContext::new()),
                    _library: library,
                });
            }
            for index in 0..set.loaded.len() {
                let plugin = &mut set.loaded[index];
                plugin.context.begin();
                let host = plugin.context.table();
                let mut diagnostic = Diagnostic::new();
                let mut span = diagnostic.span();
                let mut instance = ptr::null_mut();
                // SAFETY: Validated callback, live host/error storage; no host borrow crosses into Lua.
                let status =
                    unsafe { plugin.descriptor.init.unwrap()(&host, &mut instance, &mut span) };
                // Success transfers ownership even if another returned field is invalid:
                // preserve the instance so rollback still invokes its matching shutdown.
                if status == PG_OK {
                    plugin.instance = instance;
                }
                let (logs, fault) = plugin.context.finish();
                set.logs.extend(
                    logs.into_iter()
                        .map(|s| format!("[{}] {s}", plugin.info.id)),
                );
                let prefix = |e| error(format!("{} init: {e}", plugin.info.id));
                if status != PG_OK && !instance.is_null() {
                    return Err(prefix("failed init published an instance".to_string()));
                }
                diagnostic
                    .result(span, status)
                    .map_err(|e| prefix(e.to_string()))?;
                if let Some(fault) = fault {
                    return Err(prefix(fault.into()));
                }
                if instance.is_null() {
                    return Err(prefix("successful init returned a null instance".into()));
                }
            }
            Ok(())
        })();
        if let Err(mut failure) = result {
            if let Err(cleanup) = set.shutdown() {
                failure.0.push_str(&format!("\ncleanup: {cleanup}"));
            }
            for log in set.take_logs() {
                failure.0.push_str(&format!("\n{log}"));
            }
            return Err(failure);
        }
        Ok(set)
    }

    pub fn infos(&self) -> impl Iterator<Item = &PluginInfo> {
        self.loaded.iter().map(|p| &p.info)
    }

    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// Execute once using host-owned input and disjoint zeroed output scratch.
    /// Only validated success returns bytes. A contract fault refuses every later
    /// call, while shutdown remains available. No VM or kernel access occurs here.
    /// Oversize buffers and scratch allocation refusal return CallError::Rejected
    /// without poisoning this registry. The Luau adapter separately latches its
    /// script buffer/attempt budgets as session faults, including oversized buffers.
    pub fn call(
        &mut self,
        plugin_id: &str,
        function_id: &str,
        input: &[u8],
        capacity: usize,
    ) -> Result<Vec<u8>, CallError> {
        if let Some(fault) = &self.fault {
            return Err(CallError::Fault(fault.clone()));
        }
        let reject = |message| CallError::Rejected(error(message));
        if input
            .len()
            .checked_add(capacity)
            .is_none_or(|n| n > MAX_BATCH_BYTES)
        {
            return Err(reject("native buffer limit exceeded"));
        }
        let plugin = self
            .loaded
            .iter_mut()
            .find(|p| p.info.id == plugin_id)
            .ok_or_else(|| reject("unknown native plugin"))?;
        let index = plugin
            .info
            .functions
            .iter()
            .position(|f| f.id == function_id)
            .ok_or_else(|| reject("unknown native function"))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| reject("native scratch allocation failed"))?;
        output.resize(capacity, 0);
        let mut destination = PgOutput {
            data: if capacity == 0 {
                ptr::null_mut()
            } else {
                output.as_mut_ptr()
            },
            capacity: capacity as u64,
            written: 0,
        };
        let original = destination;
        let source = PgBytes {
            data: if input.is_empty() {
                ptr::null()
            } else {
                input.as_ptr()
            },
            len: input.len() as u64,
        };
        let mut diagnostic = Diagnostic::new();
        let mut span = diagnostic.span();
        plugin.context.begin();
        let host = plugin.context.table();
        // SAFETY: Callback was copied from the validated descriptor; its library
        // and initialized instance remain live. All spans are disjoint host memory.
        // The trusted plugin must obey read-only input and call-scoped lifetimes.
        let status = unsafe {
            plugin.calls[index].unwrap()(
                &host,
                plugin.instance,
                source,
                &mut destination,
                &mut span,
            )
        };
        let (logs, host_fault) = plugin.context.finish();
        self.logs.extend(
            logs.into_iter()
                .map(|s| format!("[{}] {s}", plugin.info.id)),
        );
        let contract = (|| {
            let text = diagnostic.text(span)?;
            if destination.data != original.data
                || destination.capacity != original.capacity
                || destination.written > original.capacity
                || (status != PG_OK && destination.written != 0)
            {
                return Err(error("plugin corrupted output pointer/capacity/length"));
            }
            if let Some(fault) = host_fault {
                return Err(error(fault));
            }
            if status >= PG_PANIC {
                return Err(error(format!(
                    "native panic/contract or unknown status {status}: {text}"
                )));
            }
            Ok(())
        })();
        if let Err(fault) = contract {
            let fault = error(format!("{plugin_id}/{function_id}: {fault}"));
            self.fault = Some(fault.clone());
            return Err(CallError::Fault(fault));
        }
        diagnostic
            .result(span, status)
            .map_err(|e| CallError::Rejected(error(format!("{plugin_id}/{function_id}: {e}"))))?;
        output.truncate(destination.written as usize);
        Ok(output)
    }

    /// Idempotent reverse teardown. Continue after errors; no libraries unload
    /// until every initialized instance has received shutdown.
    pub fn shutdown(&mut self) -> Result<(), PluginError> {
        let mut failures = Vec::new();
        for plugin in self.loaded.iter_mut().rev() {
            let instance = std::mem::replace(&mut plugin.instance, ptr::null_mut());
            if instance.is_null() {
                continue;
            }
            plugin.context.begin();
            let host = plugin.context.table();
            let mut diagnostic = Diagnostic::new();
            let mut span = diagnostic.span();
            // SAFETY: The instance came from successful init; all libraries remain
            // loaded. Shutdown must consume it on every return status.
            let status = unsafe { plugin.descriptor.shutdown.unwrap()(&host, instance, &mut span) };
            let (logs, fault) = plugin.context.finish();
            self.logs.extend(
                logs.into_iter()
                    .map(|s| format!("[{}] {s}", plugin.info.id)),
            );
            if let Err(e) = diagnostic.result(span, status) {
                failures.push(format!("{} shutdown: {e}", plugin.info.id));
            }
            if let Some(e) = fault {
                failures.push(format!("{} shutdown: {e}", plugin.info.id));
            }
        }
        self.loaded.clear();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(error(failures.join("\n")))
        }
    }
}

impl Drop for PluginSet {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

unsafe fn validate_descriptor(
    plugin: PgPlugin,
    expected_id: &str,
) -> Result<(PluginInfo, Vec<PgCall>), PluginError> {
    if plugin.abi_version != PG_ABI_VERSION || plugin.struct_size != size_of::<PgPlugin>() as u32 {
        return Err(error("plugin ABI version or descriptor size mismatch"));
    }
    if plugin.flags != 0
        || plugin.reserved != [0; 2]
        || plugin.init.is_none()
        || plugin.shutdown.is_none()
    {
        return Err(error(
            "plugin has unsupported flags/reserved fields or null lifecycle callbacks",
        ));
    }
    // SAFETY: Trusted descriptor spans are readable; helper validates length/null.
    let id = unsafe { identifier(plugin.id) }?;
    if id != expected_id {
        return Err(error(format!(
            "plugin id mismatch: expected {expected_id}, got {id}"
        )));
    }
    if plugin.function_count > 64
        || (plugin.function_count == 0) != plugin.functions.is_null()
        || (!plugin.functions.is_null()
            && !plugin
                .functions
                .addr()
                .is_multiple_of(align_of::<PgFunction>()))
    {
        return Err(error("invalid plugin function table pointer/count"));
    }
    // SAFETY: Bounded count/alignment checked; actual readability is the plugin contract.
    let functions = if plugin.function_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(plugin.functions, plugin.function_count as usize) }
    };
    let mut seen = std::collections::HashSet::new();
    let mut infos = Vec::new();
    let mut calls = Vec::new();
    for function in functions {
        if function.struct_size != size_of::<PgFunction>() as u32
            || function.schema_version == 0
            || function.reserved != [0; 2]
            || function.call.is_none()
        {
            return Err(error(
                "invalid native function size/schema version/reserved fields/callback",
            ));
        }
        // SAFETY: As for the descriptor ID; library remains loaded throughout validation.
        let function_id = unsafe { identifier(function.id) }?;
        if !seen.insert(function_id.clone()) {
            return Err(error("duplicate native function id"));
        }
        // SAFETY: Same immutable bounded UTF-8 span contract.
        let schema = unsafe { identifier(function.schema) }?;
        infos.push(FunctionInfo {
            id: function_id,
            schema,
            schema_version: function.schema_version,
        });
        calls.push(function.call);
    }
    Ok((
        PluginInfo {
            id,
            functions: infos,
        },
        calls,
    ))
}

unsafe fn identifier(value: PgStr) -> Result<String, PluginError> {
    if value.len == 0 || value.len > 128 || value.data.is_null() {
        return Err(error("invalid native identifier span"));
    }
    // SAFETY: Bounded non-null immutable span supplied by a trusted plugin.
    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len as usize) };
    let id = std::str::from_utf8(bytes).map_err(|_| error("native identifier is not UTF-8"))?;
    if !valid_id(id) {
        return Err(error("invalid native identifier syntax"));
    }
    Ok(id.into())
}

unsafe fn open_library(path: &Path) -> Result<libloading::Library, PluginError> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc"))]
    {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library,
        };
        // SAFETY: Caller trusts DLL initialization/termination. Only the adjacent
        // dependency directory and System32 participate in this explicit search.
        unsafe {
            Library::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Into::into)
        .map_err(error)
    }
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")))]
    {
        let _ = path;
        Err(error("native plugins require x86_64-pc-windows-msvc"))
    }
}
