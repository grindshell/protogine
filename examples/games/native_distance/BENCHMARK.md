# Distance-field benchmark

Recorded 2026-09-06 on BLD: Windows x64, AMD Ryzen 7 5800X (8 cores/16 threads),
Rust 1.95.0, Clang 20.1.6, mlua 0.12.1 with Luau JIT. Rust uses Cargo's release
profile; the independent C plugin uses `-O2`. No CPU affinity or frequency lock.

Re-measured after restoring clock checks at every VM interrupt. The previous
once-per-256 optimization allowed expensive built-ins to multiply the callback
timeout. Both methods run Luau, including native argument packing and result
decoding, so both pay for exact checking. This table supersedes the sampled
figures; the before/after comparison below records the cost.

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
Before/after CSV and check logs are under ignored `target/review-probes/`.

| Grid | Cells | Luau median | Native median | Luau p95 | Native p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1x1 | 1 | 24.800 | 26.200 | 39.300 | 41.000 |
| 4x4 | 16 | 33.050 | 30.000 | 47.700 | 48.800 |
| 8x8 | 64 | 52.400 | 34.800 | 69.700 | 49.500 |
| 16x16 | 256 | 133.100 | 61.500 | 151.100 | 77.700 |
| 32x32 | 1,024 | 469.150 | 161.300 | 507.200 | 183.600 |
| 64x64 | 4,096 | 1,809.250 | 569.750 | 2,046.000 | 675.300 |
| 128x128 | 16,384 | 7,394.600 | 2,241.850 | 7,648.300 | 2,455.100 |
| 256x256 | 65,536 | 29,145.150 | 8,762.550 | 31,248.900 | 9,710.200 |

Tiny fields are dominated by callback overhead and noise. At 1 cell native is
marginally worse; at 16 cells native has a lower median but slightly higher p95.
64 cells and above show lower native median and p95 in this run.
At 256x256 the complete native path is about 3.3x faster by median. These are
observations for this implementation/workload/machine, not a universal crossover
or cross-platform guarantee. A preliminary run under a different background load
measured 49.885/12.832 ms Luau/native at 256x256, illustrating host variability.
Re-run before making game-specific decisions; no zero-copy or direct ECS
interface is justified solely by these numbers.

## Deadline enforcement

Luau fires VM interrupts around calls and loop back-edges, but work between
interrupts is not uniformly cheap. Repeated `table.sort` calls on 100,000
elements took 700–703 ms to cancel with a 100 ms budget under sampling, compared
with 100–105 ms before that optimization. Exact checks now run at every VM
interrupt. An individual non-preemptible native operation can still overrun;
the next interrupt or host check observes the deadline.

Measured before and after the correction in the same session, using the same
benchmark and plugin. 256x256 medians, microseconds:

| Deadline checks | Luau median | Native median | Native advantage |
| --- | ---: | ---: | ---: |
| Once per 256 (rejected) | 13,201.700 | 3,731.800 | 3.54x |
| Every interrupt (current) | 29,145.150 | 8,762.550 | 3.33x |

At this size exact checks cost 2.21x in pure Luau and 2.35x in the complete
native wrapper. This is the cost of enforcing the callback deadline; the BFS,
C plugin and wrapper are unchanged. The benchmark's 10-second callback allowance
keeps this measurement separate from cancellation testing.
