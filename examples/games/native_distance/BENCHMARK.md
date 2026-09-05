# Distance-field benchmark

Recorded 2026-09-05 on BLD: Windows x64, AMD Ryzen 7 5800X (8 cores/16 threads),
Rust 1.95.0, Clang 20.1.6, mlua 0.12.1 with Luau JIT. Rust uses Cargo's release
profile; the independent C plugin uses `-O2`. No CPU affinity or frequency lock.

From the repository root:

```powershell
pwsh -NoProfile -File tools/build_native_distance.ps1
cargo run --offline --release --example native_benchmark -- target/native-distance-demo/game
```

[native_benchmark.rs](../../native_benchmark.rs) creates two fresh runtimes for
each grid size. Both load the same plugin and use the same context/clock setup;
one invokes the pure-Luau BFS, the other the typed native wrapper. Each callback
computes a complete open square grid from source 0. The batch size is cells per
distance field, with one native call per field. Init compares every result.

Time one `GameRuntime::step` with Rust Instant: context creation, Luau argument
validation, computation, result allocation, GC, result checks and empty kernel
systems are included for both paths. Native timing additionally includes schema
lookup, buffer allocation/packing, copying input into host scratch, zeroed output
scratch, C queue allocation/BFS, copying results back and decoding the array.
No buffers or results are reused. Rendering, DLL load, module load and init are
excluded. This measures the authoring API, not isolated C or FFI execution.

Discard 50 warm-up samples per method, then retain 200. Alternate pure/native
execution order each iteration; leave GC enabled. Median averages the two middle
samples; p95 uses nearest rank. Values below are microseconds from the final run;
CSV and local gate/capture evidence are under ignored `target/phase5-proof/`.

| Grid | Cells | Luau median | Native median | Luau p95 | Native p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1x1 | 1 | 29.450 | 30.850 | 51.100 | 64.000 |
| 4x4 | 16 | 64.300 | 61.800 | 104.000 | 118.000 |
| 8x8 | 64 | 60.500 | 43.300 | 101.600 | 85.500 |
| 16x16 | 256 | 156.800 | 79.850 | 249.400 | 138.800 |
| 32x32 | 1,024 | 659.700 | 240.950 | 752.200 | 297.400 |
| 64x64 | 4,096 | 2,038.150 | 643.250 | 2,626.900 | 886.500 |
| 128x128 | 16,384 | 8,416.000 | 2,431.850 | 10,150.900 | 3,047.500 |
| 256x256 | 65,536 | 30,636.250 | 8,999.100 | 34,767.000 | 10,220.300 |

Tiny fields are dominated by callback overhead and noise. At 16 cells the native
median is narrowly lower but p95 is worse; 64 cells and above show lower median
and p95 in this run. At 256x256 the complete native path is about 3.4x faster by
median. These are observations for this implementation/workload/machine, not a
universal crossover or cross-platform guarantee. A preliminary run under a
different background load measured 49.885/12.832 ms Luau/native at 256x256,
illustrating host variability. Re-run before making game-specific decisions;
no zero-copy or direct ECS interface is justified solely by these numbers.
