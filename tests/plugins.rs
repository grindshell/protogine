#![cfg(all(
    feature = "native-plugins",
    feature = "scripting",
    target_os = "windows",
    target_arch = "x86_64",
    target_env = "msvc"
))]

use protogine::{
    input::InputSnapshot,
    manifest::GameManifest,
    plugins::PluginSet,
    runtime::GameRuntime,
    scripting::{ScriptLimits, ScriptState},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

fn compile(source: &Path, header: &Path, dll: &Path, args: &[String]) {
    let output = Command::new(std::env::var_os("CLANG").unwrap_or_else(|| "clang".into()))
        .args([
            "--target=x86_64-pc-windows-msvc",
            "-std=c11",
            "-shared",
            "-Werror",
        ])
        .arg("-I")
        .arg(header)
        .arg(source)
        .arg("-o")
        .arg(dll)
        .args(args)
        .output()
        .expect("native tests require clang and the MSVC/Windows SDK");
    assert!(
        output.status.success(),
        "C fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native fixture exceeded the independent 15-second watchdog");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fixtures {
    directory: tempfile::TempDir,
    header: PathBuf,
}
impl Fixtures {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let header = directory.path().join("standalone-sdk");
        fs::create_dir(&header).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("include/protogine_plugin.h"),
            header.join("protogine_plugin.h"),
        )
        .unwrap();
        Self { directory, header }
    }
    fn build(&self, name: &str, mode: u32, id: &str, extra: &[String]) -> PathBuf {
        let output = self.directory.path().join(format!("{name}.dll"));
        let mut args = vec![format!("-DMODE={mode}"), format!("-DFIXTURE_ID=\"{id}\"")];
        args.extend_from_slice(extra);
        compile(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin.c"),
            &self.header,
            &output,
            &args,
        );
        output
    }
    fn helper(&self, directory: &Path, value: u32) -> PathBuf {
        let output = directory.join("protogine_phase4_helper.dll");
        compile(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin_helper.c"),
            &self.header,
            &output,
            &[format!("-DHELPER_VALUE={value}")],
        );
        output
    }

    fn distance_bundle(&self, root: &Path) {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"));
        fs::create_dir_all(root.join("plugins")).unwrap();
        compile(
            &project.join("examples/plugins/grid_distance.c"),
            &self.header,
            &root.join("plugins/grid_distance.dll"),
            &["-O2".into()],
        );
        for file in ["main.luau", "distance.luau", "game.tot"] {
            fs::copy(
                project.join("examples/games/native_distance").join(file),
                root.join(file),
            )
            .unwrap();
        }
    }
}

fn bundle(root: &Path, a: &Path, b: Option<&Path>) {
    fs::create_dir_all(root.join("plugins")).unwrap();
    fs::copy(a, root.join("plugins/a.dll")).unwrap();
    let second = if let Some(b) = b {
        fs::copy(b, root.join("plugins/b.dll")).unwrap();
        r#"{id "org.example.b" library "plugins/b.dll"}"#
    } else {
        ""
    };
    fs::write(
        root.join("game.tot"),
        format!(r#"version 1 plugins [{{id "org.example.a" library "plugins/a.dll"}} {second}]"#),
    )
    .unwrap();
    fs::write(
        root.join("main.luau"),
        r#"return {
        init=function(ctx) ctx.log('luau init') end,
        draw=function(ctx) ctx.draw.clear(0,0.5,0,1) end,
        shutdown=function(ctx)
            local trace = ctx.fs.read('bundle', 'trace.log')
            assert(not string.find(trace, 'shutdown org.', 1, true))
            ctx.log('luau shutdown')
        end,
    }"#,
    )
    .unwrap();
}

fn trace(root: &Path) -> String {
    fs::read_to_string(root.join("trace.log")).unwrap_or_default()
}

fn assert_teardown(root: &Path, initialized: &[&str]) {
    let text = trace(root);
    let lines: Vec<_> = text.lines().collect();
    let actual: Vec<_> = lines
        .iter()
        .copied()
        .filter(|s| s.starts_with("shutdown "))
        .collect();
    let expected: Vec<_> = initialized
        .iter()
        .rev()
        .map(|id| format!("shutdown org.example.{id}"))
        .collect();
    assert_eq!(actual, expected, "{text}");
    if let Some(last_shutdown) = lines.iter().rposition(|s| s.starts_with("shutdown ")) {
        assert!(
            lines.iter().position(|s| s.starts_with("unload ")).unwrap() > last_shutdown,
            "{text}"
        );
    }
    for id in initialized {
        assert_eq!(
            text.matches(&format!("unload org.example.{id}\n")).count(),
            1,
            "{text}"
        );
    }
}

// The parent never runs foreign code: every load/callback is in a watched process.
#[test]
fn native_abi_and_runtime_contracts() {
    let fixtures = Fixtures::new();
    let runner = fixtures.directory.path().join("probe.exe");
    fs::copy(std::env::current_exe().unwrap(), &runner).unwrap();
    let a = fixtures.build("a", 0, "org.example.a", &[]);
    let b = fixtures.build("b", 0, "org.example.b", &[]);
    let a_cleanup_error = fixtures.build("a-cleanup", 6, "org.example.a", &[]);
    let b_cleanup_error = fixtures.build("b-cleanup", 6, "org.example.b", &[]);
    let mut cases = Vec::new();
    for case in [
        "normal",
        "drop",
        "drop_loaded",
        "load_fault",
        "init_fault",
        "update_fault",
        "draw_fault",
        "shutdown_fault",
        "cleanup_fault",
        "primary_fault",
        "registry",
    ] {
        let root = fixtures.directory.path().join(case);
        bundle(
            &root,
            &a,
            Some(if matches!(case, "cleanup_fault" | "primary_fault") {
                &b_cleanup_error
            } else {
                &b
            }),
        );
        cases.push((case.to_string(), root));
    }
    for (mode, message) in [
        (1, "ABI version"),
        (2, "descriptor size"),
        (3, "protogine_plugin_query"),
        (4, "null lifecycle"),
        (5, "init failed"),
        (7, "duplicate native function"),
        (8, "callback"),
        (9, "native log limit"),
        (10, "diagnostic pointer/capacity/length"),
        (11, "diagnostic is not UTF-8"),
        (12, "schema version"),
        (13, "reserved"),
        (14, "null instance"),
        (15, "failed init published"),
        (18, "pointer/count"),
        (19, "identifier syntax"),
        (20, "diagnostic pointer/capacity/length"),
        (21, "active runtime thread"),
        (22, "unknown plugin status"),
        (23, "diagnostic pointer/capacity/length"),
        (24, "flags"),
        (25, "null lifecycle"),
        (26, "pointer/count"),
        (27, "identifier span"),
        (28, "identifier span"),
    ] {
        let case = format!("reject-{mode}");
        let faulty = fixtures.build(&case, mode, "org.example.b", &[]);
        let root = fixtures.directory.path().join(&case);
        bundle(&root, &a_cleanup_error, Some(&faulty));
        fs::write(root.join("expected-error"), message).unwrap();
        cases.push((case, root));
    }
    let empty = fixtures.build("empty", 17, "org.example.a", &[]);
    let root = fixtures.directory.path().join("empty");
    bundle(&root, &empty, None);
    cases.push(("empty".into(), root));
    let root = fixtures.directory.path().join("id_mismatch");
    bundle(&root, &b, None);
    cases.push(("id_mismatch".into(), root));
    let root = fixtures.directory.path().join("missing");
    bundle(&root, &a, Some(&b));
    fs::remove_file(root.join("plugins/b.dll")).unwrap();
    cases.push(("missing".into(), root));

    // Give the DLL a unique dependency, then put a decoy in both cwd and app dir.
    let real_dir = fixtures.directory.path().join("real-helper");
    fs::create_dir(&real_dir).unwrap();
    let helper = fixtures.helper(&real_dir, 7);
    let dependent = fixtures.build(
        "dependent",
        0,
        "org.example.a",
        &[
            "-DUSE_HELPER".into(),
            helper.with_extension("lib").display().to_string(),
        ],
    );
    let decoy = fixtures.helper(fixtures.directory.path(), 99);
    let cwd = fixtures.directory.path().join("unrelated-cwd");
    fs::create_dir(&cwd).unwrap();
    fs::copy(&decoy, cwd.join(decoy.file_name().unwrap())).unwrap();
    for case in ["helper_ok", "helper_missing"] {
        let root = fixtures.directory.path().join(case);
        bundle(&root, &dependent, None);
        if case == "helper_ok" {
            fs::copy(
                &helper,
                root.join("plugins").join(helper.file_name().unwrap()),
            )
            .unwrap();
        }
        cases.push((case.into(), root));
    }
    for (case, root) in cases {
        let mut child = Command::new(&runner)
            .args(["--exact", "native_probe", "--nocapture"])
            .current_dir(&cwd)
            .env("PROTOGINE_NATIVE_CASE", &case)
            .env("PROTOGINE_NATIVE_ROOT", &root)
            .env("PROTOGINE_PLUGIN_TRACE", root.join("trace.log"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        assert!(
            wait(&mut child).success(),
            "native case {case}: {}",
            trace(&root)
        );
    }
}

#[test]
fn native_probe() {
    let Ok(case) = std::env::var("PROTOGINE_NATIVE_CASE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("PROTOGINE_NATIVE_ROOT").unwrap());
    if case == "distance" {
        let limits = ScriptLimits {
            startup_timeout: Duration::from_secs(5),
            ..ScriptLimits::default()
        };
        // SAFETY: Independently built, controlled distance-field example.
        let mut runtime =
            unsafe { GameRuntime::load_trusted(&root, None, limits, Some(0)) }.unwrap();
        runtime.init().unwrap();
        assert_eq!(
            runtime.take_logs(),
            ["distance parity and schema refusals passed"]
        );
        runtime.shutdown().unwrap();
        return;
    }
    if let Some(case) = case.strip_prefix("batch-") {
        batch_probe(&root, case);
        return;
    }
    if case == "registry" || case == "empty" {
        let manifest = GameManifest::load(&root).unwrap();
        // SAFETY: Independently compiled, controlled fixtures with valid pointer storage.
        let mut plugins = unsafe { PluginSet::load_trusted(&root, &manifest) }.unwrap();
        let infos: Vec<_> = plugins.infos().cloned().collect();
        assert_eq!(infos[0].id, "org.example.a");
        if case == "empty" {
            assert!(infos[0].functions.is_empty());
        } else {
            assert_eq!(infos.len(), 2);
            assert_eq!(infos[0].functions[0].id, "example.batch");
            assert_eq!(infos[0].functions[0].schema, "example.empty");
            assert_eq!(infos[0].functions[0].schema_version, 1);
        }
        plugins.shutdown().unwrap();
        plugins.shutdown().unwrap();
        drop(plugins);
        // Copied metadata survives unload; no raw descriptor escapes the registry.
        assert_eq!(infos[0].id, "org.example.a");
        assert_teardown(&root, if case == "empty" { &["a"] } else { &["a", "b"] });
        return;
    }
    let data = root.parent().unwrap().join(format!("data-{case}"));
    fs::create_dir(&data).unwrap();
    let source = fs::read_to_string(root.join("main.luau")).unwrap().replace(
        "ctx.log('luau shutdown')",
        "ctx.fs.write('shutdown.txt', 'called'); ctx.log('luau shutdown')",
    );
    fs::write(root.join("main.luau"), source).unwrap();
    if case.ends_with("_fault") && case != "cleanup_fault" {
        let phase = if case == "primary_fault" {
            "update"
        } else {
            case.strip_suffix("_fault").unwrap()
        };
        let source = if phase == "load" {
            "error('primary script fault')".into()
        } else {
            format!(
                "return {{{phase}=function() error('primary script fault') end, {} }}",
                if phase == "shutdown" {
                    ""
                } else {
                    "shutdown=function(ctx) ctx.log('must skip shutdown') end"
                }
            )
        };
        fs::write(root.join("main.luau"), source).unwrap();
    }
    // SAFETY: Only this test's compiled DLLs/dependencies are declared in this bundle.
    let result =
        unsafe { GameRuntime::load_trusted(&root, Some(&data), ScriptLimits::default(), Some(0)) };
    if case.starts_with("reject-")
        || matches!(
            case.as_str(),
            "missing" | "helper_missing" | "id_mismatch" | "load_fault"
        )
    {
        let error = result.err().expect("fixture must refuse startup");
        if case.starts_with("reject-") {
            let expected = fs::read_to_string(root.join("expected-error")).unwrap();
            assert!(error.message.contains(&expected), "{case}: {error}");
            let mode: u32 = case.strip_prefix("reject-").unwrap().parse().unwrap();
            let initialized: &[&str] = if matches!(mode, 9 | 21 | 23) {
                &["a", "b"]
            } else if matches!(mode, 5 | 14 | 15) {
                &["a"]
            } else {
                &[]
            };
            assert_teardown(&root, initialized);
            if !initialized.is_empty() {
                assert!(
                    error.message.contains("cleanup:") && error.message.contains("shutdown failed"),
                    "{error}"
                );
            } else {
                assert!(!trace(&root).contains("init org."));
            }
        } else if case == "load_fault" {
            assert!(error.message.contains("primary script fault"));
            assert_teardown(&root, &["a", "b"]);
        } else if case == "id_mismatch" {
            assert!(error.message.contains("plugin id mismatch"));
        } else {
            // Missing primaries resolve before any load; a missing dependency
            // must fail in the OS loader, without executing a decoy's query.
            assert!(trace(&root).is_empty(), "{}", trace(&root));
        }
        return;
    }
    let mut runtime = result.unwrap();
    assert!(!trace(&root).contains("shutdown org."));
    if case != "helper_ok" {
        assert!(trace(&root).starts_with("load org.example.a\nquery org.example.a\nload org.example.b\nquery org.example.b\ninit org.example.a\ninit org.example.b\n"));
    }
    if case == "drop_loaded" {
        drop(runtime);
        assert!(!data.join("shutdown.txt").exists());
        assert_teardown(&root, &["a", "b"]);
        return;
    }
    let mut result = runtime.init();
    let mut logs = runtime.take_logs();
    if result.is_ok() && matches!(case.as_str(), "update_fault" | "primary_fault") {
        result = runtime.step(InputSnapshot::default());
    }
    if result.is_ok() && case == "draw_fault" {
        result = runtime.draw(0.0);
    }
    if case == "drop" {
        drop(runtime);
        assert!(!data.join("shutdown.txt").exists());
        assert_teardown(&root, &["a", "b"]);
        return;
    }
    if result.is_ok() {
        result = runtime.shutdown();
    }
    logs.extend(runtime.take_logs());
    if case.ends_with("_fault") {
        let error = result.unwrap_err();
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert_eq!(runtime.last_error().unwrap().to_string(), error.to_string());
        assert!(
            error.message.contains(if case == "cleanup_fault" {
                "shutdown failed"
            } else {
                "primary script fault"
            }),
            "{error}"
        );
        assert!(!logs.iter().any(|s| s.contains("must skip shutdown")));
        if case == "primary_fault" {
            assert!(
                logs.iter()
                    .any(|s| s.contains("cleanup:") && s.contains("shutdown failed"))
            );
        }
    } else {
        result.unwrap();
        assert_eq!(runtime.state(), ScriptState::Stopped);
        assert_eq!(
            fs::read_to_string(data.join("shutdown.txt")).unwrap(),
            "called"
        );
        assert!(
            logs.iter()
                .position(|s| s.contains("[org.example.a]"))
                .unwrap()
                < logs.iter().position(|s| s.contains("luau init")).unwrap()
        );
    }
    runtime.shutdown().unwrap();
    drop(runtime);
    assert_teardown(
        &root,
        if case == "helper_ok" {
            &["a"]
        } else {
            &["a", "b"]
        },
    );
}

#[test]
fn distance_field_parity_and_schema() {
    let fixtures = Fixtures::new();
    let root = fixtures.directory.path().join("game");
    fixtures.distance_bundle(&root);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/distance.luau"),
        root.join("main.luau"),
    )
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_probe", "--nocapture"])
        .current_dir(fixtures.directory.path())
        .env("PROTOGINE_NATIVE_CASE", "distance")
        .env("PROTOGINE_NATIVE_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    assert!(wait(&mut child).success());
}

#[test]
#[cfg(feature = "player")]
#[ignore = "requires graphics; runs the native distance example in a copied Player"]
fn distance_player_capture() {
    let fixtures = Fixtures::new();
    let root = fixtures.directory.path().join("distribution");
    fixtures.distance_bundle(&root.join("game"));
    let executable = root.join("protogine-player.exe");
    fs::copy(env!("CARGO_BIN_EXE_protogine-player"), &executable).unwrap();
    let mut captures = Vec::new();
    for index in 0..2 {
        let capture = root.join(format!("distance-{index}.png"));
        let stderr = root.join(format!("stderr-{index}.log"));
        let mut child = Command::new(&executable)
            .current_dir(fixtures.directory.path())
            .env("PLAYER_CAPTURE", &capture)
            .env("PLAYER_CAPTURE_FRAME", "2")
            .env("PLAYER_WIDTH", "400")
            .env("PLAYER_HEIGHT", "300")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        assert!(
            wait(&mut child).success(),
            "{}",
            fs::read_to_string(stderr).unwrap()
        );
        let pixels = image::open(&capture).unwrap().into_rgba8();
        assert_eq!(pixels.dimensions(), (400, 300));
        assert!((229..=230).contains(&pixels.get_pixel(15, 15).0[1]));
        assert!((20..=21).contains(&pixels.get_pixel(169, 15).0[1]));
        captures.push(fs::read(capture).unwrap());
    }
    assert_eq!(captures[0], captures[1]);
}

#[test]
fn native_batch_contracts() {
    let fixtures = Fixtures::new();
    let runner = fixtures.directory.path().join("batch-probe.exe");
    fs::copy(std::env::current_exe().unwrap(), &runner).unwrap();
    let mut cases = Vec::new();
    for mode in std::iter::once(0).chain(30..=50) {
        let dll = fixtures.build(&format!("batch-{mode}"), mode, "org.example.a", &[]);
        cases.push((mode.to_string(), dll));
    }
    let echo = fixtures.build("echo", 30, "org.example.a", &[]);
    for case in [
        "limits",
        "transfer",
        "calls",
        "calls-invalid-types",
        "calls-missing-args",
        "logs",
        "host",
        "world",
        "invalid-ids",
        "safe-runtime",
    ] {
        cases.push((case.into(), echo.clone()));
    }
    cases.push((
        "poison".into(),
        fixtures.build("poison", 36, "org.example.a", &[]),
    ));
    for (case, dll) in cases {
        let root = fixtures.directory.path().join(format!("game-{case}"));
        bundle(&root, &dll, None);
        let mut child = Command::new(&runner)
            .args(["--exact", "native_probe", "--nocapture"])
            .current_dir(fixtures.directory.path())
            .env("PROTOGINE_NATIVE_CASE", format!("batch-{case}"))
            .env("PROTOGINE_NATIVE_ROOT", &root)
            .env("PROTOGINE_PLUGIN_TRACE", root.join("trace.log"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        assert!(wait(&mut child).success(), "batch {case}: {}", trace(&root));
    }
}

fn batch_probe(root: &Path, case: &str) {
    if case == "safe-runtime" {
        // Leave the fixture DLL beside the game, but declare no plugins. Safe
        // loading must supply an empty API without loading or calling the DLL.
        fs::write(root.join("game.tot"), "version 1").unwrap();
        fs::write(root.join("main.luau"), r#"
            return {update = function(ctx)
                assert(next(ctx.native.plugins) == nil)
                local input = buffer.create(2)
                local output = buffer.create(4); buffer.fill(output, 0, 85)
                local ok, err = pcall(ctx.native.call, 'org.example.a', 'example.batch', input, output)
                assert(not ok and string.find(tostring(err), 'unknown native plugin', 1, true))
                assert(buffer.tostring(output) == string.rep(string.char(85), 4))
                local entity = ctx.world.spawn(10, 20)
                ctx.world.set_velocity(entity, 60, 0)
            end}
        "#).unwrap();
        let mut runtime = GameRuntime::load(root, ScriptLimits::default()).unwrap();
        runtime.init().unwrap();
        runtime.step(InputSnapshot::default()).unwrap();
        assert_eq!(runtime.state(), ScriptState::Running);
        assert_eq!(runtime.completed_ticks(), 1);
        assert_eq!(runtime.kernel().snapshot().unwrap()[0].position.x, 11.0);
        runtime.shutdown().unwrap();
        assert_eq!(runtime.state(), ScriptState::Stopped);
        assert!(trace(root).is_empty(), "safe load executed native code");
        return;
    }
    if matches!(case, "host" | "poison") {
        let manifest = GameManifest::load(root).unwrap();
        // SAFETY: Controlled C fixture; native calls are inside the watched child.
        let mut plugins = unsafe { PluginSet::load_trusted(root, &manifest) }.unwrap();
        if case == "poison" {
            use protogine::plugins::CallError;
            let first = plugins
                .call("org.example.a", "example.batch", &[0], 1)
                .unwrap_err();
            assert!(matches!(first, CallError::Fault(_)));
            let next = plugins
                .call("org.example.a", "example.batch", &[0], 1)
                .unwrap_err();
            assert!(matches!(next, CallError::Fault(_)));
            assert_eq!(first.to_string(), next.to_string());
            plugins.shutdown().unwrap();
            assert_eq!(trace(root).matches("call org.example.a\n").count(), 1);
            assert_teardown(root, &["a"]);
            return;
        }
        assert!(
            plugins
                .call("missing.plugin", "example.batch", &[], 0)
                .is_err()
        );
        assert!(
            plugins
                .call("org.example.a", "missing.function", &[], 0)
                .is_err()
        );
        assert!(matches!(
            plugins.call("org.example.a", "example.batch", &[], usize::MAX),
            Err(protogine::plugins::CallError::Rejected(_))
        ));
        assert_eq!(
            plugins
                .call("org.example.a", "example.batch", &[0, 255], 2)
                .unwrap(),
            [255, 0]
        );
        plugins.shutdown().unwrap();
        assert!(
            plugins
                .call("org.example.a", "example.batch", &[], 0)
                .is_err()
        );
        assert_teardown(root, &["a"]);
        return;
    }
    let common = r#"
        local input = buffer.create(2)
        buffer.writeu8(input, 0, 17); buffer.writeu8(input, 1, 42)
        local output = buffer.create(4); buffer.fill(output, 0, 85)
        local stale, entity, registry
        return {
            init=function(ctx)
                stale = ctx.native.call
                registry = ctx.native.plugins
                local info = registry['org.example.a']['example.batch']
                assert(info.schema == 'example.empty' and info.schema_version == 1)
                assert(not pcall(function() info.schema = 'bad' end))
                assert(not pcall(ctx.native.call, 'unknown.plugin', 'example.batch', input, output))
                assert(not pcall(ctx.native.call, 'org.example.a', 'unknown.function', input, output))
                assert(not pcall(ctx.native.call, 'org.example.a', 'example.batch', 'bad', output))
                entity = ctx.world.spawn(0, 0)
            end,
            update=function(ctx)
                assert(not pcall(stale, 'org.example.a', 'example.batch', input, output))
                -- Declarations are frozen at load, so one shared readonly snapshot
                -- serves every callback and stays valid when a script retains it.
                assert(rawequal(ctx.native.plugins, registry))
                assert(rawequal(ctx.native.plugins['org.example.a']['example.batch'],
                    registry['org.example.a']['example.batch']))
                assert(registry['org.example.a']['example.batch'].schema_version == 1)
                assert(not pcall(function() ctx.native.plugins['org.example.a'] = nil end))
                local function call(a, b) return ctx.native.call('org.example.a', 'example.batch', a, b) end
                BODY
            end,
            draw=function(ctx)
                assert(rawequal(ctx.native.plugins, registry))
                assert(not pcall(ctx.native.call, 'org.example.a', 'example.batch', input, output))
            end,
            shutdown=function(ctx)
                assert(rawequal(ctx.native.plugins, registry))
                assert(not pcall(ctx.native.call, 'org.example.a', 'example.batch', input, output))
                ctx.log('shutdown once')
            end,
        }
    "#;
    let body = match case {
        "30" => {
            r#"
            ctx.log('before')
            assert(call(input, output) == 2)
            ctx.log('after')
            assert(buffer.readu8(output,0) == 238 and buffer.readu8(output,1) == 213)
            assert(buffer.readu8(output,2) == 85 and buffer.readu8(output,3) == 85)
            assert(buffer.readu8(input,0) == 17)
            assert(call(input, input) == 2 and buffer.readu8(input,0) == 238)
            assert(call(buffer.create(0), buffer.create(0)) == 0)
            assert(not pcall(call, input, buffer.create(0)))
        "#
        }
        "0" => "assert(call(input, output) == 0); assert(buffer.readu8(output,0) == 85)",
        "limits" => "pcall(call, buffer.create(16*1024*1024), buffer.create(1))",
        "transfer" => {
            "local a, b = buffer.create(8*1024*1024), buffer.create(8*1024*1024); for i=1,4 do assert(call(a,b) == buffer.len(a)); assert(buffer.readu8(b,0) == 255) end; pcall(call,a,b)"
        }
        "calls" => {
            "local a=buffer.create(0); for i=1,128 do assert(call(a,a) == 0) end; pcall(call,a,a)"
        }
        "calls-invalid-types" => {
            "for i=1,128 do assert(not pcall(call,'bad',output)) end; ctx.log('128 rejected calls'); pcall(call,'bad',output)"
        }
        "calls-missing-args" => {
            "for i=1,128 do assert(not pcall(ctx.native.call)) end; ctx.log('128 rejected calls'); pcall(ctx.native.call)"
        }
        "logs" => {
            "local line=string.rep('x',4000); for i=1,16 do ctx.log(line) end; for i=1,100 do pcall(call,input,output) end"
        }
        "world" => {
            "assert(call(input,output) == 2); ctx.world.set_velocity(entity, buffer.readu8(output,0), 0)"
        }
        "invalid-ids" => {
            r#"
            for _, ids in {{'Org.example.a', 'example.batch'}, {'org.example.a', 'example-batch'}} do
                local ok, err = pcall(ctx.native.call, ids[1], ids[2], input, output)
                assert(not ok and string.find(tostring(err), 'invalid native identifier', 1, true))
                assert(buffer.tostring(output) == string.rep(string.char(85), 4))
            end
            -- A valid call still works after both recoverable refusals.
            assert(call(input, output) == 2)
            assert(buffer.readu8(output, 0) == 238 and buffer.readu8(output, 1) == 213)
        "#
        }
        "44" => {
            r#"
            local ok = pcall(ctx.native.call, 'org.example.a', 'example.batch', input, output)
            if ok then ctx.log('delivered') end
            if buffer.tostring(output) ~= string.rep(string.char(85),4) then
                ctx.log('output changed')
            end
        "#
        }
        _ => {
            r#"
            local ok = pcall(call, input, output)
            assert(not ok)
            assert(buffer.tostring(output) == string.rep(string.char(85),4))
            ctx.log('output preserved')
        "#
        }
    };
    fs::write(root.join("main.luau"), common.replace("BODY", body)).unwrap();
    // Non-timeout cases should test their specific budgets on slow/debug hosts.
    let limits = ScriptLimits {
        callback_timeout: if case == "44" {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        },
        ..ScriptLimits::default()
    };
    // SAFETY: Same controlled fixtures and subprocess boundary as the loader tests.
    let mut runtime = unsafe { GameRuntime::load_trusted(root, None, limits, Some(0)) }.unwrap();
    runtime.init().unwrap();
    let result = runtime.step(InputSnapshot::default());
    let logs = runtime.take_logs();
    let fatal = matches!(
        case,
        "limits" | "transfer" | "calls" | "calls-invalid-types" | "calls-missing-args" | "logs"
    ) || case
        .parse::<u32>()
        .is_ok_and(|n| (33..=47).contains(&n) || n == 50);
    if fatal {
        let error = result.unwrap_err();
        assert_eq!(runtime.state(), ScriptState::Faulted);
        assert_eq!(runtime.completed_ticks(), 0);
        assert!(runtime.step(InputSnapshot::default()).is_err());
        let expected = match case {
            "limits" | "calls" | "calls-invalid-types" | "calls-missing-args" => {
                "native call/buffer limit"
            }
            "transfer" => "native transfer limit",
            "logs" => "script log limit",
            "33" | "34" | "35" => "status",
            "36" | "37" | "38" | "39" | "47" => "output pointer/capacity/length",
            "40" | "41" | "46" => "diagnostic pointer/capacity/length",
            "42" => "UTF-8",
            "43" => "native log limit",
            "44" => "deadline",
            "45" => "runtime thread",
            "50" => "log level",
            _ => unreachable!(),
        };
        assert!(error.message.contains(expected), "{case}: {error}");
        if matches!(case, "calls-invalid-types" | "calls-missing-args") {
            assert!(logs.iter().any(|s| s == "128 rejected calls"));
            assert_eq!(trace(root).matches("call org.example.a\n").count(), 0);
        }
        if case == "44" {
            assert!(
                !logs
                    .iter()
                    .any(|s| s == "delivered" || s == "output changed")
            );
        }
    } else {
        result.unwrap();
        assert_eq!(runtime.completed_ticks(), 1);
        if case == "world" {
            assert_eq!(
                runtime.kernel().snapshot().unwrap()[0].position.x,
                238.0 / 60.0
            );
        }
        if case == "30" {
            assert_eq!(
                &logs[..3],
                ["before", "[org.example.a] info: batch", "after"]
            );
        }
        if matches!(case, "31" | "32" | "48" | "49") {
            assert!(logs.iter().any(|s| s == "output preserved"));
        }
        runtime.draw(0.0).unwrap();
        runtime.shutdown().unwrap();
        assert_eq!(
            runtime.take_logs(),
            ["shutdown once", "[org.example.a] info: shutdown"]
        );
    }
    runtime.shutdown().unwrap();
    assert_teardown(root, &["a"]);
    if case.parse::<u32>().is_ok_and(|n| (31..=50).contains(&n)) {
        assert_eq!(
            trace(root).matches("call org.example.a\n").count(),
            1,
            "no retry or later native call"
        );
    }
    if matches!(case, "0" | "30" | "world" | "invalid-ids") {
        assert_eq!(
            trace(root).matches("call org.example.a\n").count(),
            if case == "30" { 4 } else { 1 }
        );
    }
}

#[test]
#[cfg(feature = "player")]
#[ignore = "requires a graphics context; launches copied Players with C plugins"]
fn native_player_capture() {
    let fixtures = Fixtures::new();
    let a = fixtures.build("a", 0, "org.example.a", &[]);
    let bad = fixtures.build("bad", 5, "org.example.a", &[]);
    let bad_call = fixtures.build("bad-call", 36, "org.example.a", &[]);
    let root = fixtures.directory.path().join("exported");
    for (name, dll, expected) in [
        ("normal", &a, 0),
        ("failed-init", &bad, 3),
        ("failed-call", &bad_call, 3),
    ] {
        let distribution = root.join(name);
        let game = distribution.join("game");
        bundle(&game, dll, None);
        if name == "failed-call" {
            fs::write(game.join("main.luau"), r#"return { update=function(ctx)
                pcall(ctx.native.call, 'org.example.a', 'example.batch', buffer.create(1), buffer.create(1))
            end }"#).unwrap();
        }
        let executable = distribution.join("protogine-player.exe");
        fs::copy(env!("CARGO_BIN_EXE_protogine-player"), &executable).unwrap();
        let capture = distribution.join("capture.png");
        let stderr = distribution.join("stderr.log");
        let mut child = Command::new(executable)
            .current_dir(fixtures.directory.path())
            .env("PROTOGINE_PLUGIN_TRACE", game.join("trace.log"))
            .env("PLAYER_CAPTURE", &capture)
            .env("PLAYER_CAPTURE_FRAME", "1")
            .env_remove("PLAYER_WIDTH")
            .env_remove("PLAYER_HEIGHT")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        assert_eq!(
            wait(&mut child).code(),
            Some(expected),
            "{}",
            fs::read_to_string(&stderr).unwrap()
        );
        let pixels = image::open(&capture).unwrap().into_rgba8();
        if expected == 0 {
            assert!((127..=128).contains(&pixels.get_pixel(0, 0).0[1]));
            assert_teardown(&game, &["a"]);
        } else {
            let text = fs::read_to_string(stderr).unwrap();
            let (phase, diagnostic, initialized): (&str, &str, &[&str]) = if name == "failed-call" {
                ("update", "output pointer/capacity/length", &["a"])
            } else {
                ("plugins.load", "init failed", &[])
            };
            assert!(
                text.contains(&format!("Game error: {phase}:")) && text.contains(diagnostic),
                "{text}"
            );
            assert!(pixels.pixels().any(|p| p.0[0] != 0));
            assert_teardown(&game, initialized);
        }
    }
}
