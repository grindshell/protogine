# Distance-field benchmark

Recorded 2026-09-05 on BLD: Windows x64, AMD Ryzen 7 5800X (8 cores/16 threads),
Rust 1.95.0, Clang 20.1.6, mlua 0.12.1 with Luau JIT. Rust uses Cargo's release
profile; the independent C plugin uses `-O2`. No CPU affinity or frequency lock.

Re-measured the same day after the VM interrupt stopped sampling the clock on
every Luau back-edge and call. Both methods run Luau — the native path also
validates, packs and decodes there — so both got about 2.3x faster and the
native advantage is essentially unchanged. See the interrupt-sampling note below
for the controlled comparison; the earlier table is superseded, not comparable.

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
| 1x1 | 1 | 27.100 | 28.450 | 40.600 | 45.600 |
| 4x4 | 16 | 28.000 | 27.850 | 43.000 | 42.600 |
| 8x8 | 64 | 37.100 | 29.550 | 54.000 | 43.400 |
| 16x16 | 256 | 76.000 | 39.800 | 103.200 | 58.100 |
| 32x32 | 1,024 | 229.850 | 84.750 | 293.000 | 119.600 |
| 64x64 | 4,096 | 829.500 | 247.000 | 897.300 | 300.700 |
| 128x128 | 16,384 | 3,415.500 | 972.700 | 4,289.900 | 1,433.400 |
| 256x256 | 65,536 | 13,253.450 | 3,836.500 | 16,303.300 | 4,789.400 |

Tiny fields are dominated by callback overhead and noise. At 1 cell native is
marginally worse; at 16 cells the two are within noise of each other on both
median and p95. 64 cells and above show lower native median and p95 in this run.
At 256x256 the complete native path is about 3.5x faster by median. These are
observations for this implementation/workload/machine, not a universal crossover
or cross-platform guarantee. A preliminary run under a different background load
measured 49.885/12.832 ms Luau/native at 256x256, illustrating host variability.
Re-run before making game-specific decisions; no zero-copy or direct ECS
interface is justified solely by these numbers.

## Interrupt sampling

Luau fires a VM interrupt on every loop back-edge and call. The host's interrupt
enforces the callback deadline, and reading the clock there once per interrupt
cost more than the BFS itself at large sizes. It now samples once per 256
interrupts; host-initiated operations still check exactly, so only pure bytecode
between samples can overrun its deadline.

Measured back to back in one session on this machine by rebuilding with the
stride set to check every interrupt. 256x256 medians, microseconds:

| Sampling | Luau median | Native median | Native advantage |
| --- | ---: | ---: | ---: |
| Every interrupt | 30,201.000 | 8,882.300 | 3.40x |
| Once per 256 | 13,090.500 | 3,747.950 | 3.49x |

The every-interrupt row reproduces the superseded table above, which is the
evidence that the two runs are comparable and that this change, rather than
ambient load, accounts for the difference. Both methods gain because both
marshal in Luau, so the native advantage is unchanged within run-to-run
variation while the absolute figures roughly halve. This measures clock-sampling
overhead in the authoring API; the BFS and the C plugin are unchanged.
