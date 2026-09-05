//! Run three fixed ticks without a window: cargo run --example script_host
//! --no-default-features --features scripting -- examples/games/lifecycle
//! Add a data-directory argument to grant writes (created by this example).

use protogine::{input::InputSnapshot, runtime::GameRuntime, scripting::ScriptLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = args
        .next()
        .ok_or("usage: script_host <game-directory> [data-directory]")?;
    let data = args.next();
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let root = std::fs::canonicalize(root)?;
    let data = if let Some(data) = data {
        std::fs::create_dir_all(&data)?;
        Some(std::fs::canonicalize(data)?)
    } else {
        None
    };
    #[cfg(feature = "native-plugins")]
    // SAFETY: This development CLI runs the explicitly selected, trusted bundle
    // and its declared plugins. It is not a native-code sandbox.
    let mut host = unsafe {
        GameRuntime::load_trusted(&root, data.as_deref(), ScriptLimits::default(), None)
    }?;
    #[cfg(not(feature = "native-plugins"))]
    let mut host = match data {
        Some(data) => GameRuntime::load_with_data_root(&root, &data, ScriptLimits::default())?,
        None => GameRuntime::load(&root, ScriptLimits::default())?,
    };
    let result = host.init();
    print_logs(&mut host);
    result?;
    for _ in 0..3 {
        let result = host.step(InputSnapshot::default());
        print_logs(&mut host);
        result?;
        let result = host.draw(0.0);
        print_logs(&mut host);
        result?;
    }
    let result = host.shutdown();
    print_logs(&mut host);
    result?;
    Ok(())
}

fn print_logs(host: &mut GameRuntime) {
    for message in host.take_logs() {
        println!("{message}");
    }
}
