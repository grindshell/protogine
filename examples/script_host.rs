//! Run fixed ticks without a window, with an optional bounded asset preload:
//! cargo run --example script_host --no-default-features --features scripting
//! -- examples/games/lifecycle
//!
//! Usage: script_host [--preload] [--ticks N] <game-directory> [data-directory]
//!
//! `--preload` drains every admitted image after init and after each measured
//! step, before that step's draw, through the same service and limits a frame
//! uses and under its own 10-second watchdog. That is the deterministic
//! readiness schedule capture mode uses. Without it, loading is staged across
//! the ticks and the asset trace below shows the work each tick performed.
//! Add a data directory to grant writes (created by this example).

use protogine::{
    assets::AssetCounters, input::InputSnapshot, runtime::GameRuntime, scripting::ScriptLimits,
};
use std::{ffi::OsString, time::Duration};

/// Independent of the script deadline, as the preload contract requires.
const PRELOAD_WATCHDOG: Duration = Duration::from_secs(10);

struct Options {
    preload: bool,
    ticks: u32,
    root: OsString,
    data: Option<OsString>,
}

fn parse() -> Result<Options, String> {
    const USAGE: &str =
        "usage: script_host [--preload] [--ticks N] <game-directory> [data-directory]";
    let mut options = Options {
        preload: false,
        ticks: 3,
        root: OsString::new(),
        data: None,
    };
    let mut positional = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--preload") => options.preload = true,
            Some("--ticks") => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--ticks needs a count\n{USAGE}"))?;
                options.ticks = value
                    .to_str()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| format!("--ticks needs a whole number\n{USAGE}"))?;
            }
            Some(flag) if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ => positional.push(argument),
        }
    }
    let mut positional = positional.into_iter();
    options.root = positional.next().ok_or(USAGE)?;
    options.data = positional.next();
    if positional.next().is_some() {
        return Err(format!("too many arguments\n{USAGE}"));
    }
    Ok(options)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse()?;
    let root = std::fs::canonicalize(&options.root)?;
    let data = if let Some(data) = &options.data {
        std::fs::create_dir_all(data)?;
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
    report(&host, "init");
    if options.preload {
        drain(&mut host)?;
        report(&host, "preload");
    }
    for tick in 1..=options.ticks {
        let result = host.step(InputSnapshot::default());
        print_logs(&mut host);
        result?;
        // An update-time request settles before the draw that would use it,
        // which is what makes a readiness trace repeatable.
        if options.preload {
            drain(&mut host)?;
        }
        let result = host.draw(0.0);
        print_logs(&mut host);
        result?;
        report(&host, &format!("tick {tick}"));
    }
    let result = host.shutdown();
    print_logs(&mut host);
    result?;
    Ok(())
}

/// The same service and limits a frame uses. It runs no callbacks, advances no
/// simulation and resets no script deadline.
fn drain(host: &mut GameRuntime) -> Result<(), Box<dyn std::error::Error>> {
    let settled = host.drain_assets(PRELOAD_WATCHDOG);
    print_logs(host);
    if !settled? {
        return Err("asset preload did not settle within the watchdog".into());
    }
    Ok(())
}

fn print_logs(host: &mut GameRuntime) {
    for message in host.take_logs() {
        println!("{message}");
    }
}

/// The completion trace. A game that requests no image prints nothing, so this
/// changes no existing sample's output.
fn report(host: &GameRuntime, stage: &str) {
    let Some(store) = host.assets() else {
        return;
    };
    let AssetCounters {
        requests,
        admitted,
        completed,
        failed,
        cancelled,
        service_passes,
        read_calls,
        bytes_read,
        decode_bands,
        ..
    } = store.counters();
    if requests == 0 {
        return;
    }
    println!(
        "assets [{stage}]: requests={requests} admitted={admitted} completed={completed} \
         failed={failed} cancelled={cancelled} pending={} passes={service_passes} \
         reads={read_calls} bytes={bytes_read} bands={decode_bands} resident={}",
        store.pending_jobs(),
        store.resident_bytes(),
    );
}
