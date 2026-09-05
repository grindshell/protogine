use protogine::manifest::{GameManifest, MANIFEST_BYTES, PluginDeclaration};
use std::fs;

#[test]
fn optional_manifest_and_ordered_strict_schema() {
    let root = tempfile::tempdir().unwrap();
    assert!(GameManifest::load(root.path()).unwrap().plugins.is_empty());
    assert!(GameManifest::parse("version 1").unwrap().plugins.is_empty());
    let source = r#"version 1 plugins [{id "org.example.b" library "plugins/b.dll"} {id "org.example.a" library "plugins/a.dll"}]"#;
    fs::write(root.path().join("game.tot"), source).unwrap();
    let manifest = GameManifest::load(root.path()).unwrap();
    assert_eq!(
        manifest
            .plugins
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["org.example.b", "org.example.a"]
    );
    for source in [
        "",
        "version 2",
        "version 1.0",
        "version 1e0",
        "version true",
        "version 1 typo []",
        "version 1 plugins {}",
        "version 1 version 1",
        "version 1 plugins [null]",
        r#"version 1 plugins [{id "x" library "a.dll"}]"#,
        r#"version 1 plugins [{id "Org.a" library "a.dll"}]"#,
        r#"version 1 plugins [{id "org.a" library "a.dll" optional true}]"#,
        r#"version 1 plugins [{id "org.a" library "a.dll"} {id "org.a" library "b.dll"}]"#,
    ] {
        assert!(GameManifest::parse(source).is_err(), "{source}");
    }
}

#[test]
fn manifests_and_library_paths_have_bounded_validation() {
    assert!(GameManifest::parse(&" ".repeat(MANIFEST_BYTES as usize + 1)).is_err());
    let entry = r#"{id "org.a" library "a.dll"}"#;
    assert!(GameManifest::parse(&format!("version 1 plugins [{}]", entry.repeat(17))).is_err());
    for path in [
        "/a.dll",
        "../a.dll",
        "a/../b.dll",
        "./a.dll",
        "a\\b.dll",
        "C:/a.dll",
        "a.dll:stream",
        "a.dll.",
        "a.dll ",
        "CON.dll",
        "a//b.dll",
        "NUL/a.dll",
        "a.exe",
        "",
    ] {
        let quoted = tot::json::to_string(&tot::Value::String(path.into()));
        assert!(
            GameManifest::parse(&format!(
                "version 1 plugins [{{id \"org.a\" library {quoted}}}]"
            ))
            .is_err(),
            "{path}"
        );
    }
    assert!(
        GameManifest::parse(&format!(
            "version 1 extra {}null{}",
            "[".repeat(129),
            "]".repeat(129)
        ))
        .is_err()
    );
}

#[test]
fn primary_paths_are_regular_unique_and_rooted() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("plugins")).unwrap();
    fs::write(root.path().join("plugins/a.dll"), "fixture").unwrap();
    let mut manifest =
        GameManifest::parse(r#"version 1 plugins [{id "org.a" library "plugins/a.dll"}]"#).unwrap();
    assert_eq!(
        manifest.resolve_libraries(root.path()).unwrap()[0],
        root.path().join("plugins/a.dll").canonicalize().unwrap()
    );
    manifest.plugins.push(PluginDeclaration {
        id: "org.b".into(),
        library: "plugins/a.dll".into(),
    });
    assert!(
        manifest
            .resolve_libraries(root.path())
            .unwrap_err()
            .to_string()
            .contains("duplicate library")
    );
    manifest.plugins.pop();
    manifest.plugins[0].library = "plugins/missing.dll".into();
    assert!(manifest.resolve_libraries(root.path()).is_err());
    fs::create_dir(root.path().join("plugins/missing.dll")).unwrap();
    assert!(manifest.resolve_libraries(root.path()).is_err());
    assert!(GameManifest::load(std::path::Path::new("relative")).is_err());
    fs::create_dir(root.path().join("game.tot")).unwrap();
    assert!(GameManifest::load(root.path()).is_err());
}

#[test]
#[cfg(windows)]
fn library_directory_junctions_cannot_escape_the_bundle() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("a.dll"), "fixture").unwrap();
    let junction = root.path().join("plugins");
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest =
        GameManifest::parse(r#"version 1 plugins [{id "org.a" library "plugins/a.dll"}]"#).unwrap();
    let result = manifest.resolve_libraries(root.path());
    // Remove only this junction, never recursively traverse the destination.
    fs::remove_dir(&junction).unwrap();
    assert!(result.unwrap_err().to_string().contains("reparse"));
    assert_eq!(
        fs::read_to_string(outside.path().join("a.dll")).unwrap(),
        "fixture"
    );
}

#[test]
#[cfg(feature = "scripting")]
fn safe_runtime_load_never_executes_declared_libraries() {
    use protogine::{runtime::GameRuntime, scripting::ScriptLimits};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.luau"), "error('must not run')").unwrap();
    fs::write(
        root.path().join("game.tot"),
        r#"version 1 plugins [{id "org.a" library "missing.dll"}]"#,
    )
    .unwrap();
    for seed in [None, Some(0)] {
        let result = match seed {
            None => GameRuntime::load(root.path(), ScriptLimits::default()),
            Some(seed) => GameRuntime::load_seeded(root.path(), ScriptLimits::default(), seed),
        };
        let error = result.err().unwrap();
        assert!(error.message.contains("load_trusted"), "{error}");
    }
    fs::write(root.path().join("game.tot"), "version 7").unwrap();
    assert!(
        GameRuntime::load(root.path(), ScriptLimits::default())
            .err()
            .unwrap()
            .message
            .contains("version")
    );
}
