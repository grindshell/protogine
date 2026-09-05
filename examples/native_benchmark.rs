//! End-to-end distance wrapper timing. Use a release build and a trusted bundle.
use protogine::{input::InputSnapshot, runtime::GameRuntime, scripting::ScriptLimits};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

fn runtime(bundle: &Path, side: usize, method: &str) -> (tempfile::TempDir, GameRuntime) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("plugins")).unwrap();
    for file in ["game.tot", "distance.luau", "plugins/grid_distance.dll"] {
        fs::copy(bundle.join(file), root.path().join(file)).unwrap();
    }
    let call = if method == "native" {
        "distance.native(ctx, side, side, 0, tiles)"
    } else {
        "distance.pure(side, side, 0, tiles)"
    };
    fs::write(
        root.path().join("main.luau"),
        format!(
            r#"
        local distance = require('./distance')
        local side = {side}
        local tiles = table.create(side*side, 0)
        return {{
            init=function(ctx)
                local a=distance.native(ctx,side,side,0,tiles)
                local b=distance.pure(side,side,0,tiles)
                for i=1,#a do assert(a[i] == b[i]) end
            end,
            update=function(ctx)
                local result = {call}
                assert(result[1] == 0 and result[side*side] == 2*side-2)
            end,
        }}
    "#
        ),
    )
    .unwrap();
    let limits = ScriptLimits {
        startup_timeout: Duration::from_secs(10),
        callback_timeout: Duration::from_secs(10),
        ..ScriptLimits::default()
    };
    // SAFETY: This benchmark explicitly executes the caller-selected trusted example bundle.
    let mut runtime =
        unsafe { GameRuntime::load_trusted(root.path(), None, limits, Some(0)) }.unwrap();
    runtime.init().unwrap();
    (root, runtime)
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("benchmark requires --release");
        std::process::exit(2);
    }
    let bundle = std::env::args_os()
        .nth(1)
        .expect("usage: native_benchmark <trusted game directory>");
    let bundle = fs::canonicalize(bundle).unwrap();
    println!("cells,side,method,median_us,p95_us");
    for side in [1, 4, 8, 16, 32, 64, 128, 256] {
        let (_pure_root, pure) = runtime(&bundle, side, "pure");
        let (_native_root, native) = runtime(&bundle, side, "native");
        let mut runtimes = [pure, native];
        let mut samples = [Vec::new(), Vec::new()];
        // Alternate order to reduce systematic drift; GC remains enabled and timed.
        for iteration in 0..250 {
            for offset in 0..2 {
                let index = (iteration + offset) % 2;
                let start = Instant::now();
                runtimes[index].step(InputSnapshot::default()).unwrap();
                let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0;
                if iteration >= 50 {
                    samples[index].push(elapsed);
                }
            }
        }
        for (index, method) in ["pure", "native"].into_iter().enumerate() {
            samples[index].sort_by(f64::total_cmp);
            println!(
                "{},{side},{method},{:.3},{:.3}",
                side * side,
                (samples[index][99] + samples[index][100]) / 2.0,
                samples[index][189]
            );
            runtimes[index].shutdown().unwrap();
        }
    }
}
