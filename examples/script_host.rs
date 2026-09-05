//! Run three fixed ticks without a window: cargo run --example script_host
//! --no-default-features --features scripting -- examples/games/lifecycle

use protogine::scripting::{ScriptHost, ScriptLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("usage: script_host <game-directory>")?;
    let mut host = ScriptHost::load(&std::fs::canonicalize(root)?, ScriptLimits::default())?;
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
