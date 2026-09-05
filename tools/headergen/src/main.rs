use std::{env, fs, path::Path};

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    assert!(
        args.is_empty() || args == ["--check"],
        "usage: protogine-headergen [--check]"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = cbindgen::Config::from_file(root.join("sdk/cbindgen.toml")).unwrap();
    let bindings = cbindgen::Builder::new()
        .with_src(root.join("sdk/src/lib.rs"))
        .with_config(config)
        .generate()
        .expect("generate C ABI header");
    let mut bytes = Vec::new();
    bindings.write(&mut bytes);
    // cbindgen's function prefix does not apply to Rust extern declarations.
    // Decorate the generated bootstrap declaration without duplicating its ABI.
    let generated = String::from_utf8(bytes).unwrap();
    let declaration = "extern uint32_t protogine_plugin_query(";
    assert_eq!(generated.matches(declaration).count(), 1);
    let bytes = generated
        .replace(
            declaration,
            "PG_PLUGIN_EXPORT uint32_t protogine_plugin_query(",
        )
        .into_bytes();
    let path = root.join("include/protogine_plugin.h");
    if args.is_empty() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
    } else {
        assert_eq!(
            fs::read(&path).expect("generated header is missing"),
            bytes,
            "C header drift: run cargo run -p protogine-headergen"
        );
    }
    println!(
        "{}: {}",
        if args.is_empty() {
            "Generated"
        } else {
            "Verified"
        },
        path.display()
    );
}
