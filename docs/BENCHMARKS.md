# Native port benchmarks

Unless a section says otherwise, measurements were captured on 2026-08-19
with an AMD Ryzen 7 5800X (8 cores / 16 threads), Rust 1.96.0, and Node 26.4.0.
Both implementations decoded or processed the same Modern 2022 scenario at
the web game's 0.15 degree target grid (2400x1200). Each result is the median
of nine warm runs.

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

This direct territory slice ports deterministic source stamping, hostile
influence decay, control/credit attribution, census, and render invalidation.
The higher-level native runtime now performs active-combat source exclusion,
but this standalone benchmark does not. Browser frontier diffusion and its
three-cohort source scheduler remain outside both runners, so the JavaScript
runner is a bounded contract rather than complete browser territory-tick
parity.

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

`NativeRuntime` now consumes those bounded commands in-process: desertion is
applied before capitulated-country unit removal, surrender allocation transfers
the live controller/influence planes and creates an occupation, and conflict
resolution publishes a clean terminal state. Victim-by-attacker personnel-loss
attribution is accumulated alongside country casualty totals and feeds the
deterministic allocation. No new timing is claimed for these consequence paths;
the table above measures the existing 100-cycle `StrategicSimulation` workload,
which deliberately emits no desertion or capitulation commands.

The production runtime also resolves browser-parity battlefield attrition after
staged influence and before AI and combat. Sea exposure, supply collapse, and
encirclement are evaluated from one coherent pre-movement battlefield image; all resulting personnel/equipment damage
is applied as one validated batch, with rollback on downstream failure. This is
deliberately not a claim that the browser's reverse-loop mutation order is
identical: native preserves the per-unit arithmetic while making the attrition
mutation atomic.

At a pay-cycle boundary, the settled command band is resolved back into each
surviving unit's refusal flag, return-home/self-defense behavior, influence
eligibility, and planning priors. The update is committed with the strategic
cycle and therefore affects the next native tick. Home fallback is the first
controlled land cell in stable row-major order after a controlled capital; the
browser instead selects a fallback through gameplay RNG. These policy and
attrition paths are correctness-covered runtime behavior, not included in the
steady-state table above.

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
resolution, tactical movement/combat, total and victim-by-attacker casualty
accounting, territory influence and census, strategic pay-cycle
derivation/settlement, immutable snapshot publication, and FIFO render-delta
draining. Scenario read/decompression, production derivation, checkpoint
validation, and runtime construction are outside the timed region.

The generated full-cap workload contains 4,800 units (2,400 per side) spread
along the real eastern Russia-China front in the Modern 2022 MWSC scenario. It
uses a `baselineReplay` checkpoint starting at tick 598 so a three-tick sample
includes the tick-600 strategic boundary. This boundary is synthetic and
non-resumable; it is suitable for repeatable measurements, not for loading a
mid-war browser save.

This comparison was captured on 2026-08-20. Its baseline came from unmodified
commit `95cf6f1` in the same optimization turn. The optimized result aggregates
five independent release runs; every run used 3 warmups, 9 measured samples,
and 3 ticks per sample. Optimized medians are the median of the five run
medians, while p95 is the conservative maximum observed across those runs.

| Complete three-tick runtime sample | `95cf6f1` baseline | Optimized | Median improvement |
|---|---:|---:|---:|
| Fresh checkpoint median | 158.711605 ms (52.903868 ms/tick) | 131.427569 ms (43.809190 ms/tick) | 17.2% |
| Fresh checkpoint p95 | 163.776105 ms | 143.111286 ms | — |
| Persistent runtime median | 120.929678 ms (40.309893 ms/tick) | 91.368914 ms (30.456305 ms/tick) | 24.4% |
| Persistent runtime p95 | 159.519743 ms | 131.182571 ms | — |

The optimized pass replaces hot-path ordered country-side lookup with a dense
table for the complete `u16` ID space, city lookup with a dense cell mask, and
per-call touched-cell sets with reusable bitset masks and vectors. Combat
dispatch validates the enclosing simulation boundary once, then mutates
accepted attacker/target pairs directly through prevalidated kernels instead
of cloning and rechecking each pair. The war-grace fast path bypasses proximity
contacts only; eligible direct combat still runs.

The semantic benchmark checksum remained `e3a00ede1aef3d2e`, the fresh
final-state checksum remained `76a31ed4472b0d44`, and the persistent
final-state checksum remained `225367174534d753`. A representative active tick
retained 4,708 authoritative front slots, assigned 92 reinforcements, traversed
about 385,000 hostile tactical candidates, accepted 46,052 contacts, and
evaluated 128,007 territory source/cell applications. No work was skipped to
obtain the timing.

The fresh path reconstructs the checkpoint for every sample, so all nine
samples in each run include front bootstrap and the tick-600 strategic
boundary. Each persistent run advances one runtime for 27 measured ticks; it
amortizes bootstrap and includes the strategic boundary only when the live
clock reaches it. Neither number includes scenario decode, JSON serialization,
or GPU upload.

The optimized persistent median of 30.456305 ms/tick meets the 33.33 ms 30 Hz
tick budget at the 4,800-unit cap. Its conservative p95 is 131.182571 ms per
three-tick sample, equivalent to about 43.7 ms/tick, so 30 Hz is not yet met at
that tail percentile. The complete tick also remains above the 16.67 ms 60 Hz
budget. Further performance work should be chosen from a fresh profile of the
optimized runtime; these results do not claim a new dominant hotspot.

The checkpoint-v3 continuation slice was also measured from an actual
browser-exported France-versus-Spain state: 78 live units, a 2,400 by 1,200
target grid (2.88 million cells), and 579 pending regular frontier entries.
Five independent release runs each used 3 warmups, 11 measured samples, and 30
ticks per sample. Medians below are the median of the five run medians; p95 is
the conservative maximum observed across the runs.

| Live v3 30-tick sample | Median sample | Median per tick | Worst p95 sample | Worst p95 per tick |
|---|---:|---:|---:|---:|
| Fresh checkpoint | 95.393 ms | 3.180 ms | 111.127 ms | 3.704 ms |
| Persistent runtime | 101.057 ms | 3.369 ms | 117.823 ms | 3.927 ms |

This timing exercises three-cohort source selection, pending frontier
diffusion, exact Float32 map updates, AI/combat orchestration, immutable
publication, and the transactional clone of the 2.88 MB dense frontier-state
map. All five runs retained checksum `9516cd17a8e5c4b0` and persistent final
checksum `3a48ceb116a55ec6`.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/browser-v3.json \
  --ticks 30 --repeat 11 --warmup 3 --json
```

`mw-native` now gives `NativeRuntime` to a named dedicated simulation thread,
so a slow tick no longer executes on the presentation thread. This isolates
rendering from tick latency but does not improve the measured simulation rate.
The worker sends each tick's immutable snapshot and territory deltas as one
bounded, lossless FIFO publication. Render drains retain every delta in order
and collapse only intermediate snapshots, after complete publications have
arrived, to the newest snapshot. Teardown explicitly stops and joins the
worker; initialization, worker, render, panic, and headless validation failures
exit nonzero.

The report also publishes milliseconds per tick, completed steps, terminal
state, render updates drained, and deterministic final-state checksums. The
benchmark fails if two untimed fresh executions diverge or if the requested
sample cannot remain runnable for its full step count. Desertion and surrender
commands are now applied during the pay cycle and do not end a sample. A
resolved conflict deliberately ends later stepping after its final publication;
a legacy checkpoint already containing unapplied strategic commands still
trips the safety gate rather than being acknowledged silently. The measured
full-cap fixture stays `running`, so the table does not claim timings for
capitulation allocation or conflict termination.

Command-band evaluation in this measured slice settles strategic state. The
live runtime additionally refreshes changed units' `refuses_offense`,
return-home/self-defense order policy, influence gate, and assignment priors;
the timing should not be read as covering those correctness paths because this
stress fixture deliberately emits no band transitions.

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

Run an exact browser-exported v1 `postStartWar` or native v2-v10 `midWar`
checkpoint in the production viewer, or validate steps without a window:

```bash
checkpoint=/path/to/native-runtime-checkpoint.json
target/release/mw-native --runtime-checkpoint "$checkpoint" "$scenario"
target/release/mw-native --runtime-checkpoint "$checkpoint" --headless --ticks 5 --json "$scenario"
```

Native-only starts use repeated `--side` selectors (ID or unique
case-insensitive name), deterministic all-Army bootstrap, and exact-step saves:

```bash
target/release/mw-native --side Germany,France --side Poland,Belgium --headless --ticks 20 --tick-ms 1 --save-checkpoint /tmp/mw-v10.json "$scenario"
target/release/mw-native --runtime-checkpoint /tmp/mw-v10.json --headless --ticks 20 --tick-ms 1 --save-checkpoint /tmp/mw-v10-resumed.json "$scenario"
```

V1 `postStartWar` is accepted only at tick/frame/strategic-cycle zero with
exact RLE land, world-control, and de-jure maps, zero casualties, no
occupations, and deployment-adjusted starting economies. Its strict contract
is unchanged, and v1 remains the browser export default.

V2 `midWar` is requested explicitly with
`window.nativeRuntimeCheckpoint({ version: 2, steps })` after simulation has
advanced. It is a quiescent save barrier: the browser flushes territory census
work and rejects an active or dirty census. The checkpoint preserves exact
Float32 bits for occupation and every side-influence plane, current control,
primary/dominant attribution, territory revisions, the committed census,
occupations, and nested victim-by-attacker casualties. Immutable baseline
geography is still carried and used for production derivation before the live
territory overlay is restored.

V3 is requested with `window.nativeRuntimeCheckpoint({ version: 3, steps })`.
It retains the complete v2 payload and adds the exact pending priority and
regular influence-frontier queues plus their dense queued states. This makes
history-dependent diffusion resumable instead of reconstructing work from map
planes.

V4 is requested with `window.nativeRuntimeCheckpoint({ version: 4, steps })`.
It retains v3 and adds the stable-side personnel pools, bounded momentum
history, four-state phase, three-state posture, and nullable captured browser
desperation/reaction posture override. V5 adds observer-scoped operational AI,
including contacts, task forces, routes, and desperation state. V6 adds live
naval/transport execution, persistent defender reactions, airfields, and air
wings, including ordered per-country air-operations funding coverage. V7 retains
v6 and adds the exact per-side naval reassessment
clocks plus next native operation sequence. Coastal topology and reusable BFS
scratch are derived from immutable land and remain outside the serialized
contract. New native wars save v10. V8 adds required nullable per-unit supply
collapse markers for exact operational-feedback continuation; v9 adds the
Mulberry32 gameplay cursor and complete side-level recruitable reserves; v10
adds persistent aircraft logistics and monotonic unit/wing allocators. Legacy runtimes
choose the newest schema their owned continuation
state supports. Checkpoint encoding/decoding and restore remain outside the timed
benchmark region.

Both `mw-native` production modes accept resumable v1 `postStartWar` and v2-v10
`midWar` while rejecting `baselineReplay`. Native-written mid-war saves carry
the current objectives, AI assignment priors, frontline layout priors, and
last refresh tick, which makes split and uninterrupted native runs exactly
comparable across a refresh boundary. Browser mid-war exports omit that optional
block and intentionally begin from a fresh deterministic front layout. The
native runtime recomputes the browser's logical-tick influence ramp and
deterministic radius/delta noise from the exported unit seed. A strict optional
`native-battlefield-v1` block lets it also recompute terrain, city, formation,
encirclement combat/retreat, armor-support, cohesion, local repulsion, and
prior-combat influence policy from current positions and maps. Encirclement
history now also feeds the bounded browser-parity attrition path, including sea
exposure and supply collapse. Attrition is applied as a pre-planning atomic
batch, whereas the browser reverse loop can interleave damage mutation with
later unit visits. Native command-band changes refresh per-unit refusal,
return-home/self-defense behavior, influence eligibility, and planning priors at
the pay-cycle commit; those effects begin on the next native tick. Home
fallback is deterministic first-controlled-cell selection rather than the
browser's RNG reservoir. V4 additionally stages momentum/phase/posture before
planning, consumes phase and defensive posture in the same tick, and commits
combat/attrition/desertion personnel loss transactionally. V8 persists the
browser supply-collapse marker, applies its inclusive 15-tick and 35%
task-force reaction window, holds regrouping forces as defense during a side
collapse, and suppresses shared-task-force repulsion only when neither unit has
a hostile within 0.6 degrees. Observer-scoped posture intel and live country
desperation overrides are already staged. V9 adds transactionally staged naval
exile draws in reverse stable unit order, non-casualty reserve recovery, and
exact RNG/reserve split continuation. V10 recalculates air-operations funding
after each economy cycle, buys and distributes fighter/strike replacements,
and consumes the personnel reserve plus country treasury for RNG-stable land
recruitment. New formations enter the immutable end-of-tick unit snapshot and
remain deployment-inactive for 30 ticks.
Native frames advance once per logical runtime step, so frame-window mechanics
after handoff follow native cadence rather than a browser speed mode that
batches several ticks into one RAF frame. Old saves without the block retain
their frozen resolved inputs.

Browser v2-v6 handoffs carry the exact Float32 terrain plane. Standalone stock
MWSC files lack `mountainData`, so native bootstrap explicitly disables
mountains and uses flat terrain. The full-cap timings above use the frozen
stress fixture and therefore do not measure the live resolver or the new
influence/side-dynamics schedulers; they are not a claim of complete
browser-tick parity. A v2-v10 restore rebuilds private territory summaries;
partial census work and
render queues are not serialized. V3 separately preserves pending frontier
work. Map-only viewing and the small scenario-derived `--demo-units` runtime
remain separate modes.

Native save/reload equivalence assumes the canonical runtime configuration
used by the CLI. The writer rejects custom cadence/kernel or noncanonical
territory topology instead of silently restoring defaults. It also skips a
requested save after clean `ConflictResolved` termination because mid-war
checkpoints represent resumable running state.

### Live checkpoint v6 execution

Measured on 2026-08-22 on the same host and Rust toolchain. The checkpoint came
from a 1,000-tick native Germany+France versus Poland+Belgium run on the current
Modern 2022 scenario. It contained 294 live units, four airfields, eight wings,
four country-funding records, live influence/control/census state, side
dynamics, and operational AI. Each sample advanced 30 ticks; the run used two
warmups and five measured samples.

| Live v6 30-tick sample | Median sample | Median per tick | p95 sample | p95 per tick |
|---|---:|---:|---:|---:|
| Fresh checkpoint | 181.671 ms | 6.056 ms | 183.537 ms | 6.118 ms |
| Persistent runtime | 180.238 ms | 6.008 ms | 182.291 ms | 6.076 ms |

All five persistent samples completed all 150 requested ticks with final
checksum `a95d7861cf76c239`. The cross-language release gate also matched a
20+20 checkpoint continuation to an uninterrupted 40-tick run exactly.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/native-v6.json \
  --ticks 30 --repeat 5 --warmup 2 --json
```

### Native checkpoint v7 naval origination

Measured on 2026-08-22 on the same host and Rust toolchain. A native-only United
Kingdom versus Iceland war reached tick 1,000 with 163 live units, two
independently originated invasions, 28 persisted sea waypoints per invasion,
and two active defender reactions. Both invasions were in `TRANSIT`. The
planner had consumed operation sequences 1 through 3 and persisted the next
sequence as 4.

| Live v7 30-tick sample | Median sample | Median per tick | p95 sample | p95 per tick |
|---|---:|---:|---:|---:|
| Fresh checkpoint | 149.023 ms | 4.967 ms | 161.903 ms | 5.397 ms |
| Persistent runtime | 149.923 ms | 4.997 ms | 153.490 ms | 5.116 ms |

All five persistent samples completed all 150 requested ticks with final
checksum `e89419fe6b87b15e`. The production release gate also compares a
checkpointed 1,000+100-tick run with an uninterrupted 1,100-tick run byte for
byte after removing the nonsemantic `steps` request field. On the same run,
end-to-end release wall time including decode, bootstrap, simulation, and save
encoding was 4.596 seconds for 1,000 ticks and 5.025 seconds for 1,100 ticks.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/native-v7.json \
  --ticks 30 --repeat 5 --warmup 2 --json
```

### Native checkpoint v8 operational feedback

Measured on 2026-08-22 with the same United Kingdom versus Iceland setup after
1,000 ticks: 163 live units, routed native invasions, and active defender
reactions. The hostile-proximity pass reuses the tactical grid; persistent
median time remains below the earlier v7 sample.

| Live v8 30-tick sample | Median sample | Median per tick | p95 sample | p95 per tick |
|---|---:|---:|---:|---:|
| Fresh checkpoint | 133.713 ms | 4.457 ms | 134.084 ms | 4.469 ms |
| Persistent runtime | 132.104 ms | 4.403 ms | 134.771 ms | 4.492 ms |

All five persistent samples completed all 150 requested ticks with final
checksum `e89419fe6b87b15e`. The production parity gate also passed exact v8
20+20 versus 40-tick continuation and the routed naval 1,000+100 versus
1,100-tick comparison.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/native-v8.json \
  --ticks 30 --repeat 5 --warmup 2 --json
```

### Native checkpoint v9 replay-safe naval exile

Measured on 2026-08-22 with the same United Kingdom versus Iceland setup after
1,000 ticks. The checkpoint contained 163 live units, the exact Mulberry32
cursor, and a two-side recruitable-reserve vector. No sovereign with a live
at-sea formation had zero controlled land during these samples, so this is the
steady-state eligibility-scan cost; focused tests separately force both the
army and armor exile-hit paths and exact split continuation.

| Live v9 30-tick sample | Median sample | Median per tick | p95 sample | p95 per tick |
|---|---:|---:|---:|---:|
| Fresh checkpoint | 145.692 ms | 4.856 ms | 145.828 ms | 4.861 ms |
| Persistent runtime | 144.962 ms | 4.832 ms | 151.045 ms | 5.035 ms |

All five persistent samples completed all 150 requested ticks with final
checksum `957ab3e946db0f81`. The production parity gate passed exact v9 20+20
versus 40-tick continuation and routed naval 1,000+100 versus 1,100; both
comparisons include the serialized RNG cursor and personnel reserves.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/native-v9.json \
  --ticks 30 --repeat 5 --warmup 2 --json
```

### Native checkpoint v10 recruitment and air-logistics continuation

Measured on 2026-08-22 with the same United Kingdom versus Iceland setup. The
pay-cycle checkpoint starts at tick 590 with 316 land units, four wings/400
aircraft, and a 424,840-person side reserve, so every fresh 30-tick sample
crosses the tick-600 economy/air settlement while continuing per-tick
recruitment. The tick-1,000 steady checkpoint contains 364 live units; its
30-tick samples exercise recruitment and persisted logistics without crossing a
pay cycle.

| Live v10 30-tick sample | Median sample | Median per tick | p95 sample | p95 per tick |
|---|---:|---:|---:|---:|
| Tick 590 fresh (crosses pay cycle) | 117.605 ms | 3.920 ms | 117.764 ms | 3.925 ms |
| Tick 590 persistent | 119.195 ms | 3.973 ms | 127.729 ms | 4.258 ms |
| Tick 1,000 fresh steady state | 123.454 ms | 4.115 ms | 123.798 ms | 4.127 ms |
| Tick 1,000 persistent steady state | 123.535 ms | 4.118 ms | 132.856 ms | 4.429 ms |

All samples completed. The tick-590 run ended with persistent checksum
`270a03f5499e36c1`; the tick-1,000 run ended with `74e79bedf56c549f`. The full
production gate passed v10 20+20 versus 40-tick continuation, v6 legacy
fallback saving, and routed naval 1,000+100 versus 1,100 continuation.

```bash
target/release/mw-tools native-runtime-bench "$scenario" /path/to/native-v10.json \
  --ticks 30 --repeat 5 --warmup 2 --json
```
