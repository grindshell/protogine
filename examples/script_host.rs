//! Run three fixed ticks without a window: cargo run --example script_host
//! --no-default-features --features scripting -- examples/games/lifecycle
//! Add a data-directory argument to grant writes (created by this example).

use protogine::scripting::{ScriptHost, ScriptLimits};

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
    let mut host = if let Some(data) = data {
        std::fs::create_dir_all(&data)?;
        ScriptHost::load_with_data_root(
            &root,
            &std::fs::canonicalize(data)?,
            ScriptLimits::default(),
        )?
    } else {
        ScriptHost::load(&root, ScriptLimits::default())?
    };
    host.init()?;
    print_logs(&mut host);
    for _ in 0..3 {
        host.update()?;
        print_logs(&mut host);
        host.draw(0.0)?;
        print_logs(&mut host);
    }
    host.shutdown()?;
    print_logs(&mut host);
    Ok(())
}

fn print_logs(host: &mut ScriptHost) {
    for message in host.take_logs() {
        println!("{message}");
    }
}
