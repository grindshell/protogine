#![cfg(feature = "scripting")]

use protogine::scripting::{ScriptHost, ScriptLimits, ScriptState};
use std::{fs, path::Path};
use tempfile::TempDir;

fn game(source: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), source).unwrap();
    root
}

fn load(root: &Path, data: Option<&Path>) -> ScriptHost {
    match data {
        Some(data) => ScriptHost::load_with_data_root(root, data, ScriptLimits::default()),
        None => ScriptHost::load(root, ScriptLimits::default()),
    }
    .unwrap()
}

#[test]
fn tot_round_trip_preserves_value_kinds_and_large_integers() {
    let root = game(
        r#"
        return { init = function(ctx)
            local d = ctx.data
            local value = d.parse('big 123456789012345678901234567890 small -42 real 42.0 nilValue null arr [] obj {}')
            assert(d.kind(value) == 'object')
            assert(value.nilValue == d.null and value.absent == nil)
            assert(d.kind(value.big) == 'integer' and value.big.text == '123456789012345678901234567890')
            assert(tostring(value.big) == value.big.text and d.number(value.small) == -42)
            assert(d.kind(value.real) == 'float' and d.number(value.real) == 42)
            assert(d.kind(value.arr) == 'array' and d.kind(value.obj) == 'object')
            assert(not pcall(d.number, value.big))
            assert(not pcall(function() value.big.text = '0' end))
            value.arr[1] = d.null
            value.arr[2] = d.integer('9007199254740991')
            assert(d.number(value.arr[2]) == 9007199254740991)
            value.message = 'hello\n世界'
            local text = d.format(value)
            local again = d.parse(text)
            assert(d.format(again) == text)
            assert(again.arr[1] == d.null and again.big.text == value.big.text)
            assert(again.message == value.message and d.kind(again.real) == 'float')
            ctx.log(d.export(again, 'json'))
        end }
    "#,
    );
    let mut host = load(root.path(), None);
    host.init().unwrap();
    let value = tot::parse(&host.take_logs()[0]).unwrap();
    assert_eq!(
        value.get("big").unwrap().as_integer().unwrap().as_str(),
        "123456789012345678901234567890"
    );
    assert!(matches!(value.get("obj"), Some(tot::Value::Object(map)) if map.is_empty()));
}

#[test]
fn library_exports_produce_typed_yaml_and_toml() {
    let root = game(
        r#"
        return { init = function(ctx)
            local d = ctx.data
            local value = {int = d.integer('42'), float = 42, text = 'yes: # \n世界', arr = d.array({true, false}), empty = {}}
            ctx.log(d.export(value, 'yaml'))
            ctx.log(d.export(value, 'toml'))
            ctx.log(d.export({null = d.null, array = d.array({}), object = {}}, 'yaml'))
            ctx.log(d.export({n = d.integer('18446744073709551615')}, 'yaml'))
            ctx.log(d.export({n = d.integer('9223372036854775807')}, 'toml'))
            ctx.log(d.export({n = d.integer('-9223372036854775808')}, 'yaml'))
            ctx.log(d.export({n = d.integer('-9223372036854775808'), array = d.array({})}, 'toml'))
        end }
    "#,
    );
    let mut host = load(root.path(), None);
    host.init().unwrap();
    let logs = host.take_logs();
    let yaml: yaml_serde::Value = yaml_serde::from_str(&logs[0]).unwrap();
    assert_eq!(yaml["int"].as_i64(), Some(42));
    assert!(matches!(&yaml["float"], yaml_serde::Value::Number(value) if value.is_f64()));
    assert_eq!(yaml["text"].as_str(), Some("yes: # \n世界"));
    let toml: toml::Value = toml::from_str(&logs[1]).unwrap();
    assert_eq!(toml["int"].as_integer(), Some(42));
    assert_eq!(toml["float"].as_float(), Some(42.0));
    assert!(toml["empty"].as_table().unwrap().is_empty());
    let yaml: yaml_serde::Value = yaml_serde::from_str(&logs[2]).unwrap();
    assert!(yaml["null"].is_null());
    assert!(yaml["array"].as_sequence().unwrap().is_empty());
    assert!(yaml["object"].as_mapping().unwrap().is_empty());
    let yaml: yaml_serde::Value = yaml_serde::from_str(&logs[3]).unwrap();
    assert_eq!(yaml["n"].as_u64(), Some(u64::MAX));
    let toml: toml::Value = toml::from_str(&logs[4]).unwrap();
    assert_eq!(toml["n"].as_integer(), Some(i64::MAX));
    let yaml: yaml_serde::Value = yaml_serde::from_str(&logs[5]).unwrap();
    assert_eq!(yaml["n"].as_i64(), Some(i64::MIN));
    let toml: toml::Value = toml::from_str(&logs[6]).unwrap();
    assert_eq!(toml["n"].as_integer(), Some(i64::MIN));
    assert!(toml["array"].as_array().unwrap().is_empty());
}

#[test]
fn export_refusals_report_tot_paths_without_dropping_array_values() {
    let root = game(
        r#"
        return { init = function(ctx)
            local d = ctx.data
            local values = d.array({d.integer('1'), d.null, d.integer('3')})
            local value = {['a.b'] = values}
            local ok, err = pcall(d.export, value, 'toml')
            assert(not ok and string.find(tostring(err), '"a.b"[1]', 1, true))
            assert(#values == 3 and values[2] == d.null)
            assert(d.export(value, 'json') == '{"a.b":[1,null,3]}')
            for _, format in {'yaml', 'toml'} do
                values[2] = d.integer('-9223372036854775809')
                local ok, err = pcall(d.export, value, format)
                assert(not ok and string.find(tostring(err), '"a.b"[1]', 1, true))
                assert(values[2].text == '-9223372036854775809')
            end
            values[2] = d.integer('2')
            ctx.log(d.export(value, 'toml'))
        end }
    "#,
    );
    let mut host = load(root.path(), None);
    host.init().unwrap();
    assert_eq!(host.state(), ScriptState::Running);
    let value: toml::Value = toml::from_str(&host.take_logs()[0]).unwrap();
    let values = value["a.b"].as_array().unwrap();
    assert_eq!(
        values
            .iter()
            .map(toml::Value::as_integer)
            .collect::<Vec<_>>(),
        [Some(1), Some(2), Some(3)]
    );
}

#[test]
fn invalid_values_and_unsupported_exports_are_catchable() {
    let root = game(
        r#"
        return { init = function(ctx)
            local d = ctx.data
            local function fails(fn, ...)
                local ok = pcall(fn, ...)
                assert(not ok)
            end
            for _, text in {'', '+1', '01', '1.0', ' 1', '1 ', 'nan'} do fails(d.integer, text) end
            fails(d.integer, 9007199254740992)
            fails(d.parse, 'x {'); fails(d.parse, string.char(255))
            fails(d.format, nil); fails(d.format, 0/0); fails(d.format, math.huge)
            fails(d.format, {1, 2}); fails(d.format, {x = 1, [1] = 2})
            fails(d.format, buffer.create(1)); fails(d.format, function() end)
            fails(d.format, setmetatable({}, {__index = function() error('must not execute') end}))
            local cycle = {}; cycle.self = cycle; fails(d.format, cycle)
            fails(d.array, {[2] = true}); fails(d.array, {x = true})
            local a = d.array({1, 2, 3}); a[2] = nil; fails(d.format, a)
            fails(d.export, d.array({}), 'toml')
            fails(d.export, d.null, 'toml'); fails(d.export, 42, 'toml')
            fails(d.export, {n = d.integer('18446744073709551616')}, 'yaml')
            fails(d.export, {n = d.integer('9223372036854775808')}, 'toml')
            local ok, err = pcall(d.export, {nested = {bad = d.null}}, 'toml')
            assert(not ok and string.find(tostring(err), 'nested') and string.find(tostring(err), 'null'))
            fails(d.export, {}, 'xml')
            local child = {}; assert(d.format({a = child, b = child})) -- sharing is not a cycle
            ctx.log('handled')
        end }
    "#,
    );
    let mut host = load(root.path(), None);
    host.init().unwrap();
    assert_eq!(host.take_logs(), ["handled"]);
    assert_eq!(host.state(), ScriptState::Running);
}

#[test]
fn filesystem_round_trip_permissions_and_expired_functions() {
    let root = game(
        r#"
        local oldRead, oldWrite, oldParse, oldArray, count
        return {
            init = function(ctx)
                local f = ctx.fs
                assert(f.read('bundle', 'raw.bin') == string.char(0, 255, 10))
                f.mkdir('nested/deep')
                f.write('nested/deep/value.bin', string.char(0, 255))
                f.write('nested/deep/value.bin', 'replacement')
                assert(f.read('data', 'nested/deep/value.bin') == 'replacement')
                f.write('z.txt', 'last'); f.write('a.txt', 'first')
                local list = f.list('data', '')
                assert(#list == 3 and list[1].name == 'a.txt' and list[2].name == 'nested' and list[2].kind == 'directory')
                oldRead, oldWrite, oldParse, oldArray = f.read, f.write, ctx.data.parse, ctx.data.array
                count = ctx.data.integer('12')
            end,
            update = function(ctx)
                assert(not pcall(oldRead, 'bundle', 'main.luau'))
                assert(not pcall(oldWrite, 'stale.txt', 'bad'))
                assert(not pcall(oldParse, 'value 1'))
                assert(not pcall(oldArray, {}))
                assert(ctx.data.number(count) == 12) -- owned values survive callbacks
                ctx.fs.write('updated.txt', 'update')
            end,
            draw = function(ctx)
                assert(ctx.fs.read('data', 'a.txt') == 'first')
                assert(not pcall(ctx.fs.write, 'a.txt', 'draw'))
                assert(not pcall(ctx.fs.mkdir, 'draw'))
            end,
            shutdown = function(ctx) ctx.fs.write('shutdown.txt', 'shutdown') end,
        }
    "#,
    );
    fs::write(root.path().join("raw.bin"), [0, 255, 10]).unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut host = load(root.path(), Some(data.path()));
    host.init().unwrap();
    host.update().unwrap();
    host.draw(0.5).unwrap();
    host.shutdown().unwrap();
    assert_eq!(fs::read(data.path().join("a.txt")).unwrap(), b"first");
    assert_eq!(
        fs::read(data.path().join("shutdown.txt")).unwrap(),
        b"shutdown"
    );
    assert!(!data.path().join("stale.txt").exists());
    assert!(!data.path().join("draw").exists());
}

#[test]
fn filesystem_paths_and_roots_are_explicit() {
    let root = game(
        r#"
        return { init = function(ctx)
            for _, path in {'../outside', '/absolute', 'C:/file', 'x\\y', 'x//y', './x', 'CON', 'nul.txt', 'COM1', 'x.', 'x ', 'a:b', ''} do
                assert(not pcall(ctx.fs.read, 'bundle', path), path)
                assert(not pcall(ctx.fs.write, path, 'bad'), path)
                assert(not pcall(ctx.fs.mkdir, path), path)
            end
            for _, path in {'CONIN$.dll', 'conout$/a.dll', 'COM¹.dll', 'com².dll', 'COM³.dll', 'LPT¹.dll', 'lpt²/a.dll', 'LPT³.dll'} do
                assert(not pcall(ctx.fs.write, path, 'bad'), path)
                assert(not pcall(ctx.fs.mkdir, path), path)
            end
            for _, path in {'COM10.dll', 'LPT1_extra.dll', 'CONIN_extra.dll', '雪.dll'} do
                ctx.fs.write(path, 'allowed')
                assert(ctx.fs.read('data', path) == 'allowed')
            end
            assert(not pcall(ctx.fs.read, 'unknown', 'main.luau'))
            assert(not pcall(ctx.fs.read, 'data', 'missing'))
            assert(not pcall(ctx.fs.write, 'missing/child', 'bad'))
            assert(not pcall(ctx.fs.read, 'bundle', 'directory'))
            assert(not pcall(ctx.fs.write, 'directory', 'bad'))
            ctx.log('handled')
        end }
    "#,
    );
    fs::create_dir(root.path().join("directory")).unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::create_dir(data.path().join("directory")).unwrap();
    let mut host = load(root.path(), Some(data.path()));
    host.init().unwrap();
    assert_eq!(host.take_logs(), ["handled"]);
    assert!(
        ScriptHost::load_with_data_root(root.path(), root.path(), ScriptLimits::default()).is_err()
    );
    assert!(
        ScriptHost::load_with_data_root(
            root.path(),
            &root.path().join("directory"),
            ScriptLimits::default()
        )
        .is_err()
    );
    assert!(
        ScriptHost::load_with_data_root(
            root.path(),
            root.path().parent().unwrap(),
            ScriptLimits::default()
        )
        .is_err()
    );
    assert!(
        ScriptHost::load_with_data_root(
            root.path(),
            Path::new("relative"),
            ScriptLimits::default()
        )
        .is_err()
    );
}

#[test]
fn absent_data_root_grants_only_bundle_reads() {
    let root = game(
        r#"return { init = function(ctx)
        assert(ctx.fs.read('bundle', 'main.luau'))
        assert(not pcall(ctx.fs.read, 'data', 'anything'))
        assert(not pcall(ctx.fs.write, 'new.txt', 'bad'))
        assert(not pcall(ctx.fs.mkdir, 'new'))
    end }"#,
    );
    load(root.path(), None).init().unwrap();
    assert!(!root.path().join("new.txt").exists());
}

#[test]
fn malformed_utility_arguments_count_toward_callback_limit() {
    for invalid in [
        "ctx.data.parse({})",
        "ctx.data.parse()",
        "ctx.data.array(false)",
        "ctx.data.array()",
        "ctx.data.export({}, {})",
        "ctx.data.export()",
        "ctx.fs.read('bundle', {})",
        "ctx.fs.read()",
        "ctx.fs.list('bundle', {})",
        "ctx.fs.list()",
        "ctx.fs.mkdir({})",
        "ctx.fs.mkdir()",
        "ctx.fs.write('progress.tot', {})",
        "ctx.fs.write()",
    ] {
        for exceed in [false, true] {
            let root = game(&format!(
                r#"return {{
                    init = function(ctx)
                        for i = 1, 127 do ctx.data.kind(false) end
                        assert(not pcall(function() {invalid} end))
                        if {exceed} then
                            pcall(function() {invalid} end)
                            pcall(ctx.fs.write, 'progress.tot', 'overwritten')
                        end
                    end,
                    update = function(ctx)
                        ctx.fs.write('progress.tot', 'next callback')
                    end,
                }}"#,
            ));
            let data = tempfile::tempdir().unwrap();
            let path = data.path().join("progress.tot");
            fs::write(&path, "original").unwrap();
            // This tests call accounting; allow disk sync time under suite load.
            let mut host = ScriptHost::load_with_data_root(
                root.path(),
                data.path(),
                ScriptLimits {
                    callback_timeout: std::time::Duration::from_secs(2),
                    ..ScriptLimits::default()
                },
            )
            .unwrap();
            let result = host.init();
            assert_eq!(fs::read_to_string(&path).unwrap(), "original", "{invalid}");
            if exceed {
                let error = result.expect_err(invalid);
                assert!(
                    error.message.contains("utility call limit exceeded"),
                    "{invalid}: {error}"
                );
                assert_eq!(host.state(), ScriptState::Faulted);
            } else {
                result.unwrap_or_else(|error| panic!("{invalid}: {error}"));
                assert_eq!(host.state(), ScriptState::Running);
                // Argument errors at attempt 128 are recoverable; the next
                // callback gets a fresh budget and can still save normally.
                host.update().unwrap();
                assert_eq!(fs::read_to_string(&path).unwrap(), "next callback");
            }
        }
    }
}

#[test]
fn utility_limits_latch_even_when_caught() {
    for body in [
        "pcall(d.parse, string.rep('x', 1024 * 1024 + 1))",
        "pcall(d.format, string.rep('x', 1024 * 1024 + 1))",
        "pcall(d.export, string.rep(string.char(1), 200000), 'json')", // escaping expands output beyond 1 MiB
        "pcall(d.parse, '[' .. string.rep('0 ', 16384) .. ']')",
        "local value = {}; local root = value; for i=1,65 do value.child = {}; value = value.child end; pcall(d.format, root)",
        "pcall(d.array, table.create(16385, false))",
        "for i=1,129 do pcall(d.kind, true) end",
        "pcall(ctx.fs.write, 'existing.txt', string.rep('x', 1024 * 1024 + 1))",
        "pcall(ctx.fs.read, 'data', 'large.bin')",
        "for i=1,9 do pcall(ctx.fs.read, 'data', 'one.bin') end",
    ] {
        let root = game(&format!(
            "return {{ init = function(ctx) local d = ctx.data; {body} end }}"
        ));
        let data = tempfile::tempdir().unwrap();
        fs::write(data.path().join("existing.txt"), "original").unwrap();
        fs::write(data.path().join("large.bin"), vec![0; 1024 * 1024 + 1]).unwrap();
        fs::write(data.path().join("one.bin"), vec![0; 1024 * 1024]).unwrap();
        let mut host = load(root.path(), Some(data.path()));
        let error = host.init().unwrap_err();
        assert!(error.message.contains("limit"), "{body}: {error}");
        assert_eq!(host.state(), ScriptState::Faulted);
        assert_eq!(
            fs::read_to_string(data.path().join("existing.txt")).unwrap(),
            "original"
        );
    }
}

#[test]
fn tot_nesting_failures_latch_before_following_writes() {
    for (open, close) in [("[", "]"), ("{ key ", " }")] {
        for depth in [65, 128, 129, 256] {
            let root = game(&format!(
                r#"return {{ init = function(ctx)
                    local source = string.rep('{open}', {depth}) .. '0' .. string.rep('{close}', {depth})
                    pcall(ctx.data.parse, source)
                    pcall(ctx.fs.write, 'progress.tot', 'overwritten')
                end }}"#,
            ));
            let data = tempfile::tempdir().unwrap();
            let path = data.path().join("progress.tot");
            fs::write(&path, "original").unwrap();
            let mut host = load(root.path(), Some(data.path()));
            let result = host.init();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "original",
                "opening {open:?}, depth {depth}: write after nesting failure"
            );
            let error = result.expect_err("caught nesting failure must fault the session");
            assert!(
                error.message.contains("data nesting limit exceeded"),
                "{error}"
            );
            assert_eq!(host.state(), ScriptState::Faulted);
        }
    }
}

#[test]
fn tot_depth_boundary_and_caught_syntax_errors_allow_following_writes() {
    let root = game(
        r#"return { init = function(ctx)
            local d = ctx.data
            local array = d.parse(string.rep('[', 64) .. '0' .. string.rep(']', 64))
            local object = d.parse(string.rep('{ key ', 64) .. '0' .. string.rep(' }', 64))
            for _ = 1,64 do array = array[1]; object = object.key end
            assert(array.text == '0' and object.text == '0')
            assert(not pcall(d.parse, 'x {'))
            -- User-authored text in a syntax diagnostic is not a resource failure.
            local message = 'maximum nesting depth of 128 exceeded'
            local source = '"' .. message .. '" null "' .. message .. '" null'
            local ok, err = pcall(d.parse, source)
            assert(not ok and string.find(tostring(err), 'duplicate key', 1, true))
            assert(string.find(tostring(err), message, 1, true))
            ctx.fs.write('progress.tot', 'updated')
        end }"#,
    );
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("progress.tot");
    fs::write(&path, "original").unwrap();
    let mut host = load(root.path(), Some(data.path()));
    host.init().unwrap();
    assert_eq!(host.state(), ScriptState::Running);
    assert_eq!(fs::read_to_string(path).unwrap(), "updated");
}

#[test]
fn directory_listing_limit_is_latched() {
    let root = game("return { init = function(ctx) pcall(ctx.fs.list, 'data', '') end }");
    let data = tempfile::tempdir().unwrap();
    for i in 0..1025 {
        fs::write(data.path().join(format!("{i}.txt")), "").unwrap();
    }
    let mut host = load(root.path(), Some(data.path()));
    assert!(
        host.init()
            .unwrap_err()
            .message
            .contains("directory entry limit")
    );
    assert_eq!(host.state(), ScriptState::Faulted);
}

#[cfg(windows)]
#[test]
fn locked_destination_replacement_preserves_original_and_cleans_tempfile() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = game(
        r#"return { init = function(ctx)
        assert(not pcall(ctx.fs.read, 'data', 'locked.txt'))
        assert(not pcall(ctx.fs.write, 'locked.txt', 'replacement'))
        ctx.log('write failed')
    end }"#,
    );
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("locked.txt");
    fs::write(&path, "original").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let mut host = load(root.path(), Some(data.path()));
    host.init().unwrap();
    assert_eq!(host.take_logs(), ["write failed"]);
    assert_eq!(host.state(), ScriptState::Running);
    drop(lock);
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(fs::read_dir(data.path()).unwrap().count(), 1);
}

#[test]
fn luau_example_restores_its_own_schema_across_sessions() {
    let root = game(include_str!("../examples/games/persistence/main.luau"));
    let data = tempfile::tempdir().unwrap();
    for previous in [0, 3] {
        let mut host = load(root.path(), Some(data.path()));
        host.init().unwrap();
        assert_eq!(host.take_logs(), [format!("Loaded {previous} ticks")]);
        for _ in 0..3 {
            host.update().unwrap();
        }
        host.shutdown().unwrap();
        let saved =
            tot::parse(&fs::read_to_string(data.path().join("progress.tot")).unwrap()).unwrap();
        assert_eq!(
            saved.get("ticks").unwrap().as_integer().unwrap().as_i64(),
            Some(previous + 3)
        );
    }
    // A schema failure must not cause the game to silently overwrite old data.
    let invalid = "version 2 ticks 999";
    fs::write(data.path().join("progress.tot"), invalid).unwrap();
    let mut host = load(root.path(), Some(data.path()));
    assert!(
        host.init()
            .unwrap_err()
            .message
            .contains("unsupported progress version")
    );
    host.shutdown().unwrap();
    assert_eq!(
        fs::read_to_string(data.path().join("progress.tot")).unwrap(),
        invalid
    );
}

#[cfg(windows)]
#[test]
fn junctions_cannot_escape_filesystem_roots() {
    let root = game(
        r#"return { init = function(ctx)
        assert(not pcall(ctx.fs.read, 'data', 'link/private.txt'))
        assert(not pcall(ctx.fs.write, 'link/private.txt', 'overwrite'))
        assert(not pcall(ctx.fs.mkdir, 'link/new'))
        assert(not pcall(ctx.fs.list, 'data', 'link'))
        assert(not pcall(ctx.fs.list, 'data', ''))
    end }"#,
    );
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private.txt"), "original").unwrap();
    let link = data.path().join("link");
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", "New-Item -ItemType Junction -Path $env:PROTOGINE_LINK -Target $env:PROTOGINE_LINK_TARGET | Out-Null"])
        .env("PROTOGINE_LINK", &link).env("PROTOGINE_LINK_TARGET", outside.path()).status().unwrap();
    assert!(status.success());
    let mut host = load(root.path(), Some(data.path()));
    let result = host.init();
    fs::remove_dir(link).unwrap();
    result.unwrap();
    assert_eq!(
        fs::read_to_string(outside.path().join("private.txt")).unwrap(),
        "original"
    );
    assert!(!outside.path().join("new").exists());
}
