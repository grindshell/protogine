#![cfg(feature = "scripting")]

use protogine::scripting::{ScriptHost, ScriptLimits, ScriptState};
use std::{fs, path::Path};
use tempfile::TempDir;

fn game(source: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), source).unwrap();
    root
}
fn load(root: &Path) -> ScriptHost {
    ScriptHost::load(root, ScriptLimits::default()).unwrap()
}

#[test]
fn lifecycle_and_fixed_update_contract() {
    let root = game(
        r#"
        local ticks = 0
        return {
            init = function(ctx) ctx.log("init") end,
            update = function(ctx, dt)
                assert(dt == 1 / 60)
                ticks += 1
                ctx.log(tostring(ticks))
            end,
            draw = function(ctx, alpha) assert(alpha == 0.25); ctx.log("draw") end,
            shutdown = function(ctx) ctx.log("shutdown") end,
        }
    "#,
    );
    let mut host = load(root.path());
    assert_eq!(host.state(), ScriptState::Loaded);
    assert!(host.update().is_err());
    host.init().unwrap();
    assert_eq!(host.take_logs(), ["init"]);
    assert!(host.init().is_err());
    for tick in 1..=3 {
        host.update().unwrap();
        assert_eq!(host.take_logs(), [tick.to_string()]);
    }
    assert!(host.draw(f64::NAN).is_err());
    assert_eq!(host.state(), ScriptState::Running);
    host.draw(0.25).unwrap();
    assert_eq!(host.take_logs(), ["draw"]);
    host.shutdown().unwrap();
    assert_eq!(host.take_logs(), ["shutdown"]);
    host.shutdown().unwrap();
    assert!(host.take_logs().is_empty());
    assert!(host.update().is_err());
}

#[test]
fn callback_tables_are_validated_without_running_metamethods() {
    for source in [
        "return 4",
        "return {}, nil",
        "return { update = 4 }",
        "return { udpate = function() end }",
        "return { [1] = function() end }",
        "return setmetatable({}, { __index = function() while true do end end })",
    ] {
        let root = game(source);
        assert!(
            ScriptHost::load(root.path(), ScriptLimits::default()).is_err(),
            "{source}"
        );
    }
    let root = game("return {}");
    let mut host = load(root.path());
    host.init().unwrap();
    host.update().unwrap();
    host.draw(0.0).unwrap();
    host.shutdown().unwrap();
}

#[test]
fn faults_stop_callbacks_and_skip_shutdown() {
    for phase in ["init", "update", "draw", "shutdown"] {
        let root = game(&format!(
            r#"
            return {{
                {phase} = function() error("intentional failure") end,
            }}
        "#
        ));
        let mut host = load(root.path());
        let result = match phase {
            "init" => host.init(),
            "update" => {
                host.init().unwrap();
                host.update()
            }
            "draw" => {
                host.init().unwrap();
                host.draw(0.0)
            }
            _ => {
                host.init().unwrap();
                host.shutdown()
            }
        };
        let error = result.unwrap_err();
        assert_eq!(error.phase, phase);
        assert!(error.message.contains("main.luau"), "{error}");
        assert_eq!(host.state(), ScriptState::Faulted);
        assert!(host.last_error().is_some());
        assert!(host.update().is_err());
        host.shutdown().unwrap();
        assert_eq!(host.state(), ScriptState::Faulted);
    }
    let root = game("return { shutdown = function() error('must not run') end }");
    let mut host = load(root.path());
    host.shutdown().unwrap();
    assert_eq!(host.state(), ScriptState::Stopped);
}

#[test]
fn retained_context_functions_expire_and_errors_can_be_caught() {
    let root = game(
        r#"
        local old
        return {
            init = function(ctx) old = ctx.log end,
            update = function(ctx)
                local ok = pcall(old, "stale")
                assert(not ok)
                ctx.log("current")
            end,
        }
    "#,
    );
    let mut host = load(root.path());
    host.init().unwrap();
    host.update().unwrap();
    assert_eq!(host.take_logs(), ["current"]);
}

#[test]
fn modules_resolve_relative_to_importer_and_cache_owned_values() {
    let root = game(
        r#"
        local a = require("./lib/a")
        local b = require("./lib/a")
        assert(a == b and a.count == 1 and a.value == 42)
        return { init = function(ctx) ctx.log("loaded") end }
    "#,
    );
    fs::create_dir(root.path().join("lib")).unwrap();
    fs::write(
        root.path().join("lib/a.luau"),
        "local b = require('./b'); return {count = 1, value = b.value}",
    )
    .unwrap();
    fs::write(root.path().join("lib/b.luau"), "return {value = 42}").unwrap();
    let mut host = load(root.path());
    host.init().unwrap();
    assert_eq!(host.take_logs(), ["loaded"]);
}

#[test]
fn module_cycles_and_failed_imports_do_not_poison_retry() {
    let root = game(
        r#"
        local ok, err = pcall(require, "./a")
        assert(not ok and string.find(tostring(err), "cyclic"))
        ok, err = pcall(require, "./a")
        assert(not ok and string.find(tostring(err), "cyclic"))
        assert(not pcall(require, "./fail"))
        assert(require("./fail").ok)
        return {}
    "#,
    );
    fs::write(root.path().join("a.luau"), "return require('./b')").unwrap();
    fs::write(root.path().join("b.luau"), "return require('./a')").unwrap();
    fs::write(root.path().join("fail.luau"), "attempt = (attempt or 0) + 1; if attempt == 1 then error('first attempt') end; return {ok = true}").unwrap();
    load(root.path());
}

#[test]
fn loaded_modules_keep_their_source_until_session_restart() {
    let root = game(
        "local value = require('./value'); return {update = function(ctx) assert(value == require('./value')); ctx.log(tostring(value.number)) end}",
    );
    let module = root.path().join("value.luau");
    fs::write(&module, "return {number = 42}").unwrap();
    let mut host = load(root.path());
    host.init().unwrap();
    fs::write(&module, "invalid source !!!").unwrap();
    host.update().unwrap();
    assert_eq!(host.take_logs(), ["42"]);
    assert!(ScriptHost::load(root.path(), ScriptLimits::default()).is_err());
}

#[test]
fn module_guard_keeps_original_helpers_and_source_context() {
    let root = game("xpcall = function() return true, {} end; return require('./broken')");
    fs::write(
        root.path().join("broken.luau"),
        "local function fail() error('original failure') end; fail(); return {}",
    )
    .unwrap();
    let error = ScriptHost::load(root.path(), ScriptLimits::default())
        .err()
        .unwrap();
    assert!(
        error.message.contains("original failure")
            && error.message.contains("broken.luau")
            && error.message.contains("stack traceback"),
        "{error}"
    );
}

#[cfg(windows)]
#[test]
fn windows_module_case_alias_uses_one_cache_entry() {
    let root = game("assert(require('./Value') == require('./value')); return {}");
    fs::write(root.path().join("Value.luau"), "return {}").unwrap();
    load(root.path());
}

#[test]
fn entry_module_executes_once_even_when_required_later() {
    let root = game(
        "entry_runs = (entry_runs or 0) + 1; return { update = function(ctx) local main = require('./main'); assert(type(main.update) == 'function'); ctx.log(tostring(entry_runs)) end }",
    );
    let mut host = load(root.path());
    host.init().unwrap();
    host.update().unwrap();
    assert_eq!(host.take_logs(), ["1"]);
}

#[test]
fn module_paths_and_host_capabilities_are_restricted() {
    let root = game(
        r#"
        for _, path in {"../outside", "/absolute", "C:/absolute", "@alias", "./bad.lua", "./missing"} do
            assert(not pcall(require, path), path)
        end
        assert(io == nil and os == nil and package == nil and coroutine == nil)
        assert(loadstring == nil and getfenv == nil and setfenv == nil)
        assert(not pcall(function() math.pi = 0 end))
        return {}
    "#,
    );
    load(root.path());
    assert!(ScriptHost::load(Path::new("relative"), ScriptLimits::default()).is_err());
    fs::write(root.path().join("main.luau"), [0xFF, 0xFE]).unwrap();
    assert!(ScriptHost::load(root.path(), ScriptLimits::default()).is_err());
}

#[test]
fn callback_return_values_are_faults() {
    let root = game("return { update = function() return 42 end }");
    let mut host = load(root.path());
    host.init().unwrap();
    assert!(
        host.update()
            .unwrap_err()
            .message
            .contains("must not return")
    );
    assert_eq!(host.state(), ScriptState::Faulted);
}

#[cfg(windows)]
#[test]
fn junction_escape_is_rejected() {
    let root = game("return require('./outside/value')");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("value.luau"), "return {}").unwrap();
    // A junction needs no symlink privilege. Remove it explicitly before temp cleanup.
    let junction = root.path().join("outside");
    let script = "New-Item -ItemType Junction -Path $env:PROTOGINE_LINK -Target $env:PROTOGINE_LINK_TARGET | Out-Null";
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", script])
        .env("PROTOGINE_LINK", &junction)
        .env("PROTOGINE_LINK_TARGET", outside.path())
        .status()
        .unwrap();
    assert!(status.success());
    let result = ScriptHost::load(root.path(), ScriptLimits::default());
    fs::remove_dir(junction).unwrap();
    assert!(result.is_err());
    assert!(outside.path().join("value.luau").exists());
}
