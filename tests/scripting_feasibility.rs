//! Run VM boundary probes in child processes so a broken interrupt cannot hang Cargo.
#![cfg(feature = "scripting")]

use mlua::{Lua, VmState, chunk::ChunkMode, thread::ThreadStatus};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[test]
fn scripting_dependency_probes() {
    for probe in [
        "jit",
        "deadline",
        "caught_deadline",
        "memory",
        "caught_memory",
        "tot_hecs",
        "host_load_loop",
        "host_update_loop",
        "host_pcall_loop",
        "host_xpcall_loop",
        "host_module_loop",
        "host_memory",
        "host_caught_memory",
        "host_log_limit",
        "host_depth",
        "host_source",
        "host_metamethod_loop",
        "host_builtin_sort",
        "host_builtin_sort_pcall",
        "host_builtin_sort_xpcall",
        "host_builtin_fill",
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "isolated_probe", "--nocapture"])
            .env("PROTOGINE_VM_PROBE", probe)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{probe}: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{probe} exceeded the 10-second process timeout");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn isolated_probe() -> mlua::Result<()> {
    let Ok(probe) = std::env::var("PROTOGINE_VM_PROBE") else {
        return Ok(());
    };
    if probe.starts_with("host_builtin_") {
        builtin_deadline_probe(&probe);
        return Ok(());
    }
    if probe.starts_with("host_") {
        host_probe(&probe);
        return Ok(());
    }
    let lua = Lua::new();
    match probe.as_str() {
        "jit" => {
            // SAFETY: This callback only inspects the live caller frame and pushes one
            // boolean using Luau's public C API. No VM pointers escape the callback.
            unsafe extern "C-unwind" fn in_native(state: *mut mlua::ffi::lua_State) -> i32 {
                unsafe {
                    let native = mlua::ffi::lua_incustomexecution(state, 1);
                    mlua::ffi::lua_pushboolean(state, native);
                }
                1
            }
            // SAFETY: The callback obeys the Lua C-function stack/return convention.
            lua.globals()
                .set("in_native", unsafe { lua.create_c_function(in_native)? })?;
            lua.sandbox(true)?;
            for enabled in [false, true] {
                lua.enable_jit(enabled);
                let (sum, native): (u32, bool) = lua
                    .load(
                        r#"
                    --!native
                    local function sum(n: number): (number, boolean)
                        local total = 0
                        for i = 1, n do total += i end
                        assert(in_native() == EXPECT_NATIVE, "native execution mismatch")
                        return total, in_native()
                    end
                    return sum(100)
                "#
                        .replace("EXPECT_NATIVE", if enabled { "true" } else { "false" }),
                    )
                    .set_name("@jit_probe.luau")
                    .set_mode(ChunkMode::Text)
                    .eval()?;
                assert_eq!(sum, 5050);
                assert_eq!(native, enabled);
            }
            let error = lua
                .load("local function fail() error('probe') end\nfail()")
                .set_name("@traceback_probe.luau")
                .exec()
                .unwrap_err()
                .to_string();
            assert!(error.contains("traceback_probe.luau"), "{error}");
            assert!(error.contains("stack traceback"), "{error}");
        }
        "deadline" | "caught_deadline" => {
            lua.sandbox(true)?;
            let native_seen = std::rc::Rc::new(std::cell::Cell::new(false));
            let seen = native_seen.clone();
            let deadline = Instant::now() + Duration::from_millis(20);
            lua.set_interrupt(move |lua| {
                // SAFETY: Inspect only the current live frame during its interrupt.
                let native = lua.exec_raw_lua(|raw| unsafe {
                    mlua::ffi::lua_incustomexecution(raw.state(), 0) != 0
                });
                seen.set(seen.get() || native);
                if Instant::now() >= deadline {
                    Ok(VmState::Yield)
                } else {
                    Ok(VmState::Continue)
                }
            });
            let source = if probe == "deadline" {
                "--!native\nlocal function spin() while true do end end; spin()"
            } else {
                "--!native\nlocal function spin() while true do end end; while true do pcall(spin) end"
            };
            let co =
                lua.create_thread(lua.load(source).set_mode(ChunkMode::Text).into_function()?)?;
            co.resume::<()>(())?;
            assert_eq!(co.status(), ThreadStatus::Resumable);
            assert!(
                native_seen.get(),
                "deadline must interrupt a native Luau frame"
            );
        }
        "memory" | "caught_memory" => {
            lua.set_memory_limit(lua.used_memory() + 1024 * 1024)?;
            let source = "return buffer.create(4 * 1024 * 1024)";
            if probe == "memory" {
                assert!(matches!(
                    lua.load(source).eval::<mlua::Value>(),
                    Err(mlua::Error::MemoryError(_))
                ));
            } else {
                let ok: bool = lua
                    .load(format!(
                        "local ok = pcall(function() {source} end); return ok"
                    ))
                    .eval()?;
                assert!(!ok, "pcall must report allocation failure");
            }
        }
        "tot_hecs" => {
            let manifest = tot::parse("version 1\nplugins []").unwrap();
            assert_eq!(
                manifest
                    .as_object()
                    .unwrap()
                    .get("version")
                    .unwrap()
                    .as_integer()
                    .unwrap()
                    .as_i64(),
                Some(1)
            );
            let mut world = hecs::World::new();
            let entity = world.spawn((12_i32,));
            assert_eq!(*world.get::<&i32>(entity).unwrap(), 12);
        }
        _ => panic!("unknown probe: {probe}"),
    }
    Ok(())
}

// Interrupt counts do not bound elapsed time: one table.sort or buffer.fill can
// cost much more than a bytecode loop iteration. Keep these inside the parent's
// independent 10-second watchdog, including the protected-call variants.
fn builtin_deadline_probe(probe: &str) {
    use protogine::scripting::{ScriptHost, ScriptLimits, ScriptState};
    let (setup, operation) = match probe {
        "host_builtin_sort" => (
            "local values = table.create(100000, 1)",
            "table.sort(values)",
        ),
        "host_builtin_sort_pcall" => (
            "local values = table.create(100000, 1)",
            "pcall(table.sort, values)",
        ),
        "host_builtin_sort_xpcall" => (
            "local values = table.create(100000, 1); local function sort() table.sort(values) end",
            "xpcall(sort, function() return 'caught' end)",
        ),
        "host_builtin_fill" => (
            "local values = buffer.create(32 * 1024 * 1024)",
            "buffer.fill(values, 0, 1)",
        ),
        _ => panic!("unknown builtin probe: {probe}"),
    };
    for native in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let source = |body: &str| {
            format!(
                "{}\n{setup}\nreturn {{ update = function() {body} end }}",
                if native { "--!native" } else { "" }
            )
        };
        // Calibrate a single non-preemptible operation on this host. The limit
        // below allows scheduler noise and several such operations, but cannot
        // hide a stride's worth of repeated expensive calls after the deadline.
        std::fs::write(root.path().join("main.luau"), source(operation)).unwrap();
        let mut control = ScriptHost::load(
            root.path(),
            ScriptLimits {
                callback_timeout: Duration::from_secs(2),
                ..ScriptLimits::default()
            },
        )
        .unwrap();
        control.init().unwrap();
        let mut slowest = Duration::ZERO;
        for _ in 0..3 {
            let start = Instant::now();
            control.update().unwrap();
            slowest = slowest.max(start.elapsed());
        }
        control.shutdown().unwrap();
        drop(control);

        std::fs::write(
            root.path().join("main.luau"),
            source(&format!("while true do {operation} end")),
        )
        .unwrap();
        let limits = ScriptLimits::default();
        let mut host = ScriptHost::load(root.path(), limits).unwrap();
        host.init().unwrap();
        let start = Instant::now();
        let error = host.update().unwrap_err();
        let elapsed = start.elapsed();
        let allowance = limits.callback_timeout + Duration::from_millis(100) + slowest * 4;
        assert!(error.message.contains("deadline"), "{probe}: {error}");
        assert!(
            elapsed <= allowance,
            "{probe}, native={native}: cancellation took {elapsed:?}, allowed {allowance:?} \
             (budget {:?}, single operation {slowest:?})",
            limits.callback_timeout
        );
        assert_eq!(host.state(), ScriptState::Faulted);
        assert!(host.update().is_err());
        host.shutdown().unwrap();
        assert_eq!(host.state(), ScriptState::Faulted);
    }
}

fn host_probe(probe: &str) {
    use protogine::scripting::{ScriptHost, ScriptLimits, ScriptState};
    let root = tempfile::tempdir().unwrap();
    let body = match probe {
        "host_load_loop" => "local function spin() while true do end end; spin(); return {}".into(),
        "host_update_loop" => "return { update = function() while true do end end }".into(),
        "host_pcall_loop" => "local function spin() while true do end end; return { update = function() while true do pcall(spin) end end }".into(),
        "host_xpcall_loop" => "local function spin() while true do end end; return { update = function() while true do xpcall(spin, function() return 'caught' end) end end }".into(),
        "host_metamethod_loop" => "local function spin() while true do end end; local value = setmetatable({}, { __tostring = function() while true do pcall(spin) end end }); return { update = function() tostring(value) end }".into(),
        "host_module_loop" => {
            std::fs::write(root.path().join("spin.luau"), "--!native\nlocal function spin() while true do end end; spin(); return {}").unwrap();
            "return { update = function() pcall(require, './spin') end }".into()
        }
        "host_memory" => "return { update = function() local b = buffer.create(16 * 1024 * 1024) end }".into(),
        "host_caught_memory" => "return { update = function(ctx) local ok = pcall(buffer.create, 16 * 1024 * 1024); assert(not ok); ctx.log('recovered') end }".into(),
        "host_log_limit" => "return { update = function(ctx) pcall(ctx.log, string.rep('x', 4097)) end }".into(),
        "host_depth" => {
            for n in 0..65 {
                std::fs::write(root.path().join(format!("m{n}.luau")), format!("return require('./m{}')", n + 1)).unwrap();
            }
            "pcall(require, './m0'); return {}".into()
        }
        "host_source" => format!("--{}\nreturn {{}}", "x".repeat(256 * 1024)),
        _ => panic!("unknown host probe: {probe}"),
    };
    std::fs::write(root.path().join("main.luau"), format!("--!native\n{body}")).unwrap();
    let limits = ScriptLimits {
        memory_bytes: 4 * 1024 * 1024,
        startup_timeout: Duration::from_millis(500),
        callback_timeout: Duration::from_millis(30),
    };
    let result = ScriptHost::load(root.path(), limits);
    if matches!(probe, "host_load_loop" | "host_depth" | "host_source") {
        let error = result.err().expect("load must fault");
        assert!(
            error.message.contains(match probe {
                "host_depth" => "depth",
                "host_source" => "source limit",
                _ => "deadline",
            }),
            "{error}"
        );
        return;
    }
    let mut host = result.unwrap();
    host.init().unwrap();
    let result = host.update();
    if probe == "host_caught_memory" {
        result.unwrap();
        assert_eq!(host.take_logs(), ["recovered"]);
        assert_eq!(host.state(), ScriptState::Running);
    } else {
        let error = result.unwrap_err();
        assert!(
            error.message.contains(match probe {
                "host_memory" => "memory",
                "host_log_limit" => "log limit",
                _ => "deadline",
            }),
            "{error}"
        );
        assert_eq!(host.state(), ScriptState::Faulted);
        assert!(host.update().is_err());
        host.shutdown().unwrap();
        assert_eq!(host.state(), ScriptState::Faulted);
    }
}
