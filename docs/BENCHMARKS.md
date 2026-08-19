# Port baseline benchmarks

Measured on 2026-08-19 with an AMD Ryzen 7 5800X (8 cores / 16 threads),
Rust 1.96.0, and Node 26.4.0. Both implementations decoded or processed the
same Modern 2022 scenario at the web game's 0.15 degree target grid
(2400x1200). Each result is the median of nine warm runs.

| Workload | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Gzip read + MWSC decode + dense maps + province IDs | 214.51 ms | 103.14 ms | 2.08x |
| Russia-China frontline direction field | 189.37 ms | 11.33 ms | 16.71x |

The direction benchmark executes the unchanged
`workers/simulation-worker.js` algorithm in Node's VM without worker-transfer
overhead. The Rust benchmark likewise excludes scenario loading. The decoder
benchmark includes file reading and gzip decompression for both implementations.

Correctness was checked independently from timing: all ownership, de-jure,
land, biome, province, latitude-direction, and longitude-direction arrays have
identical deterministic FNV-1a hashes between JavaScript and Rust.

## Tactical grid

Measured on 2026-08-20 on the same host and toolchain. The tactical workload
contains 4,800 deterministic units (the browser cap of
2,400 per side), with frontline bands, rear formations, and dense hotspots. It
produces 1,806 occupied side-cells, 29,686 candidate pairs, and 11,438 accepted
pairs per iteration. The table reports the median of three runs; each run used
20 warmups and 100 measured iterations.

| Workload | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Rebuild and aggregate tactical grid | 2.058 ms | 0.731 ms | 2.82x |
| Enumerate and filter same-side neighbor pairs | 1.511 ms | 0.550 ms | 2.75x |

Median p95 was 4.543 ms versus 0.740 ms for rebuild and 3.307 ms versus
0.557 ms for pair traversal. Both implementations produced the same counters,
callback order, and quantized-distance visit hash (`a7d88afc753ba623`). The
stable hash pass runs outside the timed interval.

Reproduce the parity matrix:

```bash
./scripts/verify-scenario-parity.sh
```

Reproduce the 0.15 degree timings:

```bash
cargo build --release -p mw-tools
target/release/mw-tools inspect ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz --grid-res 0.15 --repeat 9 --json
node scripts/benchmark-web-reference.mjs decode ../modern-wars ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz 0.15 9
target/release/mw-tools field-bench ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz --grid-res 0.15 --repeat 9 --json
node scripts/benchmark-web-reference.mjs field ../modern-wars ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz 0.15 9
```

Reproduce the tactical workload:

```bash
tactical_fixture=$(mktemp --suffix=.mw-tactical.json)
node scripts/generate-tactical-stress.mjs 2400 > "$tactical_fixture"
target/release/mw-tools tactical-bench "$tactical_fixture" --repeat 100 --warmup 20 --json
node scripts/js-tactical-reference.mjs bench ../modern-wars "$tactical_fixture" 100 20
```

## Resolved unit movement and combat

Measured on 2026-08-20 on the same host and toolchain. The deterministic
workload uses seed `0x4d575031`, 4,800 movement cases, and 4,800 combat units
executing 2,400 ordered proximity/direct operations. Each of five runs used 20
warmups and 100 measured iterations; the table reports the median of those five
per-run medians.

| Workload | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Final movement integration and coast handling | 0.559 ms | 0.452 ms | 1.24x |
| Fresh-state pair-combat fixture replay | 12.038 ms | 0.528 ms | 22.82x |

Median p95 was 0.657 ms versus 0.566 ms for movement and 12.818 ms
versus 0.594 ms for combat. Both implementations produced the same full stress
report at `1e-10` tolerance and the same untimed checksum
(`13660.317874844264`).

This is a resolved-operation fixture benchmark, not an isolated arithmetic
microbenchmark or a whole-game FPS claim. The timed combat path includes fresh
unit reconstruction, ID lookup maps, ordered mutation, sorted result
projection, formation/equipment loss reporting, and knockback in both
implementations. It excludes AI target selection, strategic modifiers,
tactical contact discovery, territory updates, rendering, and worker/message
overhead. The native-tick section below adds a complete fresh-state
orchestration replay; persistent steady-state ticking remains a separate future
measurement.

Reproduce the workload:

```bash
unit_fixture=$(mktemp --suffix=.mw-units.json)
node scripts/generate-unit-kernel-stress.mjs 2400 > "$unit_fixture"
target/release/mw-tools unit-bench "$unit_fixture" --repeat 100 --warmup 20 --json
node scripts/js-unit-kernel-reference.mjs bench ../modern-wars "$unit_fixture" 100 20
```

## Native tick orchestration

Measured on 2026-08-20 on the same host and toolchain. The deterministic
workload contains 4,800 units (2,400 per side) spread across isolated tactical
cells. One fixture replay produces 3,200 proximity events, 800 direct
engagements, and 4,000 resolved movement attempts. Each of five runs used 20
warmups and 100 measured replays; the table reports the median per-run median
and the median per-run p95.

| Complete one-tick fixture replay | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Median | 27.368 ms | 6.830 ms | 4.01x |
| p95 | 31.626 ms | 7.512 ms | 4.21x |

Both implementations consume the browser tactical-grid semantics, execute the
same reverse unit order and immediate mutation sequence, and produce matching
full reports at `1e-10` tolerance. The canonical two-step fixture additionally
covers direct combat, armor landing-penalty expiry, a removed unit's stale
order, directed hostility, multi-contact ordering, movement fallback, cleanup,
and ID-sorted immutable snapshots.

This measures a complete fresh-state fixture replay: unit/order adaptation,
tactical-grid construction, contact dispatch, movement/combat, cleanup, and
report projection are inside the timed interval; JSON serialization is outside
it in both languages. It is not a whole-game frame benchmark. Even with that
extra fixture setup, the Rust replay is below a 16.67 ms 60 Hz frame budget on
the measured machine.

Reproduce the workload:

```bash
tick_fixture=$(mktemp --suffix=.mw-native-tick.json)
node scripts/generate-native-tick-stress.mjs 2400 > "$tick_fixture"
target/release/mw-tools native-tick-bench "$tick_fixture" --repeat 100 --warmup 20 --json
node scripts/js-native-tick-reference.mjs bench ../modern-wars "$tick_fixture" 100 20
```
