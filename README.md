# Native Modern Wars

Performance-first Rust port of Modern Wars. The web game remains the behavioral
reference while systems move into the renderer-independent `mw-core` crate.

## Current milestone

- MWSC v2 scenario decoding and grid remapping
- deterministic frontline direction-field generation
- deterministic production front segmentation, slots, and objectives
- deterministic tactical spatial grid and same-side neighbor traversal
- browser-parity final unit movement, coast handling, and pair combat
- deterministic native tick orchestration over resolved orders and tactical contacts
- deterministic AI contact, retreat, frontline, reinforcement, and field orders
- production derivation from MWSC countries, current control, cities, and GDP
- Float32 territory influence, controller attribution, dirty-tile rendering, and
  incremental census publication
- atomic economy, occupation, resistance, capitulation, desertion, and treaty cycles
- in-process desertion, capitulation transfer, occupation seeding, and clean
  terminal conflict-resolution consequences
- one `NativeRuntime` owner connecting front layout -> AI -> simulation ->
  territory -> strategic settlement
- reference-counted immutable snapshots, FIFO territory render deltas, and a
  native `wgpu` unit overlay
- a named dedicated simulation worker with bounded, lossless atomic
  publications and explicit stop/join shutdown
- versioned browser-to-native checkpoints: strict v1 post-`startWar()`
  compatibility plus v2 quiescent mid-war continuation with exact live
  influence, control, census, and nested casualty-attribution state
- production checkpoint loading in the native viewer plus bounded-step,
  window-free worker validation with clean terminal-conflict completion
- optimized full-cap territory and combat hot paths using dense lookup tables,
  reusable cell tracking, and in-place prevalidated pair dispatch
- headless JavaScript parity fixtures and timing for every migrated slice

The repository intentionally starts with the existing browser scenarios rather
than copying 50,000+ lines of JavaScript into Rust-shaped files.

## Build and test

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --workspace
```

## Run against the current web fixture

From this directory:

```bash
cargo run --release -p mw-tools -- inspect ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz --grid-res 0.15
cargo run --release -p mw-tools -- production-inspect ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz --grid-res 0.15 --country Germany
cargo run --release -p mw-tools -- tactical-fixture fixtures/tactical-grid-v1.json
cargo run --release -p mw-tools -- unit-fixture fixtures/movement-combat-v1.json
cargo run --release -p mw-tools -- native-tick-fixture fixtures/native-tick-v1.json
cargo run --release -p mw-tools -- ai-orders-fixture fixtures/ai-orders-v1.json
cargo run --release -p mw-tools -- territory-control-fixture fixtures/territory-control-v1.json
cargo run --release -p mw-tools -- strategic-cycle-fixture fixtures/strategic-cycle-v1.json
cargo run --release -p mw-tools -- native-runtime-fixture ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz fixtures/native-runtime-checkpoint-v1.json --json
cargo run --release -p mw-native -- ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
```

`native-runtime-fixture` accepts three deliberately different checkpoint
boundaries. Checkpoint v1 retains `postStartWar`, which is
production-resumable only at tick, frame, and strategic cycle zero, and the
synthetic non-resumable `baselineReplay` fixture/benchmark boundary.
Checkpoint v2 adds `midWar`: a production-resumable save barrier carrying live
units, economies, occupations, total and victim-by-attacker casualties, every
territory plane, revision markers, and the last fully committed census.

The browser exports v1 by default for compatibility. After a war has advanced,
request v2 explicitly from its console:

```js
await window.downloadNativeRuntimeCheckpoint({ version: 2, steps: 5 });
```

The v2 exporter synchronously flushes territory work and refuses to save if a
census remains active or dirty. It retains immutable baseline geography for
scenario production derivation; the loader derives those immutable baselines
first and only then overlays the committed live territory maps.

Run a browser-exported production checkpoint in the native viewer, or exercise
the same dedicated runtime worker without creating a window or GPU device:

```bash
scenario=../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
checkpoint=/path/to/native-runtime-checkpoint.json
cargo run --release -p mw-native -- --runtime-checkpoint "$checkpoint" "$scenario"
cargo run --release -p mw-native -- --runtime-checkpoint "$checkpoint" --headless --ticks 5 --json "$scenario"
```

Both production paths accept exact-state v1 `postStartWar` and v2 `midWar`
checkpoints. They reject the synthetic, non-resumable `baselineReplay`
boundary. Normal startup remains a map-only viewer, and `--demo-units` remains
available as the small scenario-derived runtime.

For an automated three-frame GPU/window smoke test:

```bash
cargo run --release -p mw-native -- --smoke ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
cargo run --release -p mw-native -- --smoke --demo-units ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
```

Native viewer controls:

- drag left mouse: pan
- mouse wheel: cursor-anchored zoom
- `R`: reset camera
- left click: print the selected country and geographic cell
- `Esc`: quit

`mw-native --demo-units`, production checkpoint viewing, native headless
validation, and the `mw-tools` runners all exercise the shared `NativeRuntime`.
Runtime modes give exclusive mutable ownership to the named
`mw-native-runtime` simulation thread. Each completed tick enters one bounded,
lossless FIFO publication containing its immutable snapshot and all associated
territory deltas. Renderer drains may collapse intermediate snapshots to the
newest snapshot only after receiving complete publications; every territory
delta remains ordered and is applied. Shutdown explicitly requests stop and
joins the worker, and initialization, worker, render, panic, and headless
step-limit failures produce a nonzero process exit.

Threading keeps the measured runtime work off the presentation thread. The
optimized full-cap persistent runtime has a 30.456 ms/tick median, 24.4% below
the same-turn 40.310 ms/tick baseline, so it meets the 33.33 ms 30 Hz median
budget. Its conservative p95 is still 131.183 ms per three-tick sample, or
about 43.7 ms/tick, and it does not meet a 16.67 ms 60 Hz budget.

The hot path maps every `u16` country ID to a side through a dense lookup,
checks cities through a dense cell mask, and records influenced cells in
reused masks and vectors. Combat configuration is validated at the simulation
boundary, after which accepted attacker/target pairs are mutated directly by
the prevalidated kernels. War-grace bypass applies only to proximity contacts;
eligible direct engagements still execute during grace.

Run the complete cross-language parity matrix against the adjacent web checkout:

```bash
./scripts/verify-scenario-parity.sh
```

Generate and benchmark the 4,800-unit movement/combat workload:

```bash
node scripts/generate-unit-kernel-stress.mjs 2400 > /tmp/mw-unit-stress.json
target/release/mw-tools unit-bench /tmp/mw-unit-stress.json --repeat 100 --warmup 20 --json
node scripts/js-unit-kernel-reference.mjs bench ../modern-wars /tmp/mw-unit-stress.json 100 20
```

Generate and benchmark the complete 4,800-unit native tick (tactical contact
discovery, immediate combat, resolved movement, cleanup, and snapshot):

```bash
node scripts/generate-native-tick-stress.mjs 2400 > /tmp/mw-native-tick-stress.json
target/release/mw-tools native-tick-bench /tmp/mw-native-tick-stress.json --repeat 100 --warmup 20 --json
node scripts/js-native-tick-reference.mjs bench ../modern-wars /tmp/mw-native-tick-stress.json 100 20
```

Generate and benchmark deterministic AI order resolution for 4,800 units and
32 ordered frontline objectives:

```bash
node scripts/generate-ai-orders-stress.mjs 4800 32 > /tmp/mw-ai-orders-stress.json
target/release/mw-tools ai-orders-bench /tmp/mw-ai-orders-stress.json --repeat 100 --warmup 20 --json
node scripts/js-ai-orders-reference.mjs /tmp/mw-ai-orders-stress.json bench --repeat=100 --warmup=20
```

Generate and benchmark territory influence plus its full and persistent
tile-local census paths on a 2,400 x 1,200 grid:

```bash
node scripts/generate-territory-control-stress.mjs 2400 1200 4800 > /tmp/mw-territory-stress.json
target/release/mw-tools territory-control-bench /tmp/mw-territory-stress.json --repeat 7 --warmup 2 --ticks 3 --budget 16384 --json
node scripts/js-territory-control-reference.mjs /tmp/mw-territory-stress.json bench --repeat=7 --warmup=2 --ticks=3 --budget=16384
```

Generate and benchmark 100 atomic strategic pay cycles over 512 countries and
256 occupations:

```bash
node scripts/generate-strategic-cycle-stress.mjs 512 256 100 > /tmp/mw-strategic-stress.json
target/release/mw-tools strategic-cycle-bench /tmp/mw-strategic-stress.json --repeat 20 --warmup 5 --json
node scripts/js-strategic-cycle-reference.mjs /tmp/mw-strategic-stress.json bench --repeat=20 --warmup=5
```

Inspect production derivation and benchmark the complete production runtime at
the 4,800-unit browser cap:

```bash
target/release/mw-tools production-inspect ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz --grid-res 0.15 --json
node scripts/generate-native-runtime-stress.mjs 2400 3 > /tmp/mw-native-runtime-stress.json
target/release/mw-tools native-runtime-fixture ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz /tmp/mw-native-runtime-stress.json --json
target/release/mw-tools native-runtime-bench ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz /tmp/mw-native-runtime-stress.json --ticks 3 --repeat 9 --warmup 3 --json
```

At a strategic boundary, `NativeRuntime` now prepares settlement privately,
applies desertion first, then removes a capitulated country's surviving units,
allocates and transfers its live territory, seeds the resulting occupation,
and commits only after those staged consequences validate. A surrender
invalidates native front priors so the next tick rebuilds them from the new
map. Conflict resolution publishes a final immutable result and enters a clean
terminal state instead of requiring an external acknowledge-and-continue
step.

This remains a bounded simulation port. Mid-war v2 does not serialize a
partial census, render queues, or private front-assignment history; those are
rebuilt at the save/load boundary. The surrender path does not yet include the
browser's releasables, province-border smoothing, equipment-reserve or air
cleanup, or treaty/UI presentation. Frontier diffusion, active-combat source
exclusion, air/naval simulation, and the full gameplay UI remain outside this
slice.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the migration boundary.
Measured Rust-versus-JavaScript results are recorded in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).
