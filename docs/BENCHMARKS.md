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

The AI, territory, and strategic results below were measured in release mode
on 2026-08-20 (Europe/Berlin) on the same host and toolchain described above.

## AI order resolution

The deterministic AI workload contains 4,800 units, 32 ordered objectives, a
360 x 180 land/controller grid, a directed four-side hostility matrix, and
frontline direction fields. This generated workload resolves 202 contact
orders, 4 sticky assignments, 4,199 fresh front assignments, and 597
reinforcement assignments. Retreat, field, and hold semantics are covered by
the canonical fixture rather than claimed as stress-path coverage.

Each implementation used 5 warmups and 20 measured plans.

| Generated AI planning replay | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Median | 17.408 ms | 7.060 ms | 2.47x |

p95 was 24.299 ms for JavaScript and 7.451 ms for Rust. Both reports
produced the same full semantic order/assignment checksum
(`cb8b51d60227e1e7`); the checksum includes directions, movement and combat
modifiers, reasons, target IDs, and counters. The checked-in
`ai-orders-v1` fixture remains the more detailed correctness gate, including
permutation invariance, invalid-input cases, and exact order projection.

Reproduce the workload:

```bash
ai_fixture=$(mktemp --suffix=.mw-ai-orders.json)
node scripts/generate-ai-orders-stress.mjs 4800 32 > "$ai_fixture"
target/release/mw-tools ai-orders-bench "$ai_fixture" --repeat 20 --warmup 5 --json
node scripts/js-ai-orders-reference.mjs "$ai_fixture" bench --repeat=20 --warmup=5
```

## Territory control and census

The consequence-heavy territory workload uses a 2,400 x 1,200 grid, four side
influence planes, 4,800 deterministic sources mixing hostile offensive pressure
with same-side reinforcement, and 200 sparse dirty seed cells. It reports two
distinct paths: full source application plus census flush, and three persistent
ticks that each apply sources and flush a budgeted dirty-tile census through
atomic publication. Each implementation used 2 warmups and 7 measured samples
with a 16,384-item census chunk budget.

| Territory workload | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Full influence application and census flush | 422.143 ms | 150.932 ms | 2.80x |
| Three published influence/census ticks | 667.624 ms | 256.804 ms | 2.60x |

Full-path p95 was 427.854 ms for JavaScript and 154.672 ms for Rust;
persistent-path p95 was 679.212 ms and 257.323 ms respectively. Every
persistent sample committed all 3 generations, processed 4,411,392 census
items, produced 8 dominant-controller changes and 3,198 primary-credit
changes, and finished with zero remaining items, zero dirty tiles, and no
active generation. Both implementations produced the same ownership
projection checksum (`c618ba25`). The full logical ownership projection
(11,520,000 oracle bytes) and checksum are built outside the timer; this is an
integrity signal, not a native dirty-tile upload benchmark. The canonical
`territory-control-v1` fixture additionally checks exact Float32 maps,
dirty-tail census publication, reset/replace behavior, aggregate snapshots,
and tile payloads.

This slice ports deterministic source stamping, hostile influence decay,
control/credit attribution, census, and render invalidation. It does not yet
include the browser's separate frontier-diffusion pass or its active-combat
source exclusion; the JavaScript runner is the frozen cross-language contract
for this bounded slice, not a claim of complete browser territory-tick parity.

Reproduce the workload (the generated JSON is intentionally large because it
contains the dense browser-compatible maps):

```bash
territory_fixture=$(mktemp --suffix=.mw-territory.json)
node scripts/generate-territory-control-stress.mjs 2400 1200 4800 > "$territory_fixture"
target/release/mw-tools territory-control-bench "$territory_fixture" --repeat 7 --warmup 2 --ticks 3 --budget 16384 --json
node scripts/js-territory-control-reference.mjs "$territory_fixture" bench --repeat=7 --warmup=2 --ticks=3 --budget=16384
```

## Economy, occupation, and surrender orchestration

The strategic stress workload runs 100 sequential pay cycles over 512 country
economies and 256 occupations. Each replay exercises territory-derived income,
occupation due/yield, garrison and resistance updates, command-band evaluation,
and event publication. This steady-state workload processes 51,200 country
records, 25,600 occupation records, and 78 events; it deliberately produces no
capitulations or desertion commands. Those consequence paths, treaty results,
and atomic failures are covered by the canonical fixture. The Rust runner
constructs a fresh simulation per sample so cross-sample state does not leak
into timing.

Each implementation used 5 warmups and 20 measured fresh-state replays.

| Complete 100-cycle strategic replay | JavaScript reference | Rust release | Speedup |
|---|---:|---:|---:|
| Median | 154.109 ms | 21.128 ms | 7.29x |

p95 was 157.086 ms for JavaScript and 21.921 ms for Rust. Both reports
produced the same checksum (`751054b0b82f874d`). The canonical
`strategic-cycle-v1` fixture remains part of the parity gate and includes
capitulation, desertion, conflict resolution, duplicate/regressing replay
guards, stable territory markers, and errors proving that a failed cycle leaves
economy, occupation, cycle, and snapshot state unchanged.

Reproduce the workload:

```bash
strategic_fixture=$(mktemp --suffix=.mw-strategic.json)
node scripts/generate-strategic-cycle-stress.mjs 512 256 100 > "$strategic_fixture"
target/release/mw-tools strategic-cycle-bench "$strategic_fixture" --repeat 20 --warmup 5 --json
node scripts/js-strategic-cycle-reference.mjs "$strategic_fixture" bench --repeat=20 --warmup=5
```

## Production native runtime

The production runtime benchmark covers the entire migrated orchestration
slice in one owned process: production front refresh, AI contact/order
resolution, tactical movement/combat, casualty accounting, territory influence
and census, strategic pay-cycle derivation/settlement, immutable snapshot
publication, and FIFO render-delta draining. Scenario read/decompression,
production derivation, checkpoint validation, and runtime construction are
outside the timed region.

The generated full-cap workload contains 4,800 units (2,400 per side) spread
along the real eastern Russia-China front in the Modern 2022 MWSC scenario. It
uses a `baselineReplay` checkpoint starting at tick 598 so a three-tick sample
includes the tick-600 strategic boundary. This boundary is synthetic and
non-resumable; it is suitable for repeatable measurements, not for loading a
mid-war browser save.

Two complete release validation runs used 3 warmups and 9 measured samples.
The values below are the median of the two run medians; p95 is the more
conservative of the two observed p95 values.

| Complete three-tick runtime sample | Rust release |
|---|---:|
| Fresh checkpoint median | 156.23 ms (52.08 ms/tick) |
| Fresh checkpoint p95 | 166.34 ms |
| Persistent runtime median | 120.08 ms (40.03 ms/tick) |
| Persistent runtime p95 | 159.05 ms |

Both runs produced the same semantic benchmark checksum
(`e3a00ede1aef3d2e`), fresh final-state checksum
(`76a31ed4472b0d44`), and persistent final-state checksum
(`225367174534d753`). A representative active tick retained 4,708 authoritative
front slots, assigned 92 reinforcements, traversed about 385,000 hostile
tactical candidates, accepted 46,052 contacts, and evaluated 128,007 territory
source/cell applications. No work was skipped to obtain the timing.

The fresh path reconstructs the checkpoint for every sample, so all nine
samples include front bootstrap and the tick-600 strategic boundary. The
persistent path advances one runtime for 27 measured ticks; it amortizes
bootstrap and includes the strategic boundary only when the live clock reaches
it. Neither number includes scenario decode, JSON serialization, or GPU upload.

This complete single-threaded tick does **not** meet a 16.67 ms 60 Hz frame
budget at the 4,800-unit cap. Its measured persistent 40.03 ms/tick also misses
the 33.33 ms 30 Hz budget. The measured next targets are direct
territory-source stamping (roughly 13 ms/tick) and checked pair-combat dispatch
(roughly 5 ms/tick).

`mw-native` now gives `NativeRuntime` to a named dedicated simulation thread,
so a slow tick no longer executes on the presentation thread. This isolates
rendering from tick latency but does not improve the measured simulation rate.
The worker sends each tick's immutable snapshot and territory deltas as one
bounded, lossless FIFO publication. Render drains retain every delta in order
and collapse only intermediate snapshots, after complete publications have
arrived, to the newest snapshot. Teardown explicitly stops and joins the
worker; initialization, worker, render, panic, and headless validation failures
exit nonzero.

The report also publishes milliseconds per tick, completed steps, gate state,
render updates drained, and deterministic final-state checksums. The benchmark
fails if two untimed fresh executions diverge or if the requested fresh sample
hits the strategic-effects gate. A persistent run never acknowledges that
gate silently: any desertion, surrender, or conflict-resolution command ends
the available persistent sample.

Reproduce production inspection, the canonical deterministic replay, and the
4,800-unit workload:

```bash
scenario=../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
target/release/mw-tools production-inspect "$scenario" --grid-res 0.15 --json
target/release/mw-tools native-runtime-fixture "$scenario" fixtures/native-runtime-checkpoint-v1.json --json

runtime_fixture=$(mktemp --suffix=.mw-native-runtime.json)
node scripts/generate-native-runtime-stress.mjs 2400 3 > "$runtime_fixture"
target/release/mw-tools native-runtime-bench "$scenario" "$runtime_fixture" --ticks 3 --repeat 9 --warmup 3 --json
```

Run an exact browser-exported `postStartWar` checkpoint in the production
viewer or validate an exact number of steps without a window:

```bash
checkpoint=/path/to/native-runtime-checkpoint.json
target/release/mw-native --runtime-checkpoint "$checkpoint" "$scenario"
target/release/mw-native --runtime-checkpoint "$checkpoint" --headless --ticks 5 --json "$scenario"
```

`postStartWar` is the separate production-resumable checkpoint boundary. It is
accepted only at tick/frame/strategic-cycle zero with exact RLE land,
world-control, and de-jure maps, zero casualties, no occupations, and
deployment-adjusted starting economies. The native runtime recomputes the
browser's logical-tick influence ramp and deterministic radius/delta noise from
the exported unit seed. Other terrain-, urban-, cohesion-, and live-state
modifiers are resolved at handoff and become native-owned inputs; this is a
production boundary, not a claim that the remaining browser tick has already
been ported. Both `mw-native` production modes require this exact-geography
`postStartWar` boundary and reject `baselineReplay`. Map-only viewing and the
small scenario-derived `--demo-units` runtime remain separate modes.
