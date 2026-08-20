# Native Modern Wars

Performance-first Rust port of Modern Wars. The web game remains the behavioral
reference while systems move into the renderer-independent `mw-core` crate.

## Current milestone

- MWSC v2 scenario decoding and grid remapping
- deterministic frontline direction-field generation
- deterministic tactical spatial grid and same-side neighbor traversal
- browser-parity final unit movement, coast handling, and pair combat
- deterministic native tick orchestration over resolved orders and tactical contacts
- deterministic AI contact, retreat, frontline, reinforcement, and field orders
- Float32 territory influence, controller attribution, dirty-tile rendering, and
  incremental census publication
- atomic economy, occupation, resistance, capitulation, desertion, and treaty cycles
- immutable unit snapshots and a native `wgpu` unit overlay
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
cargo run --release -p mw-tools -- tactical-fixture fixtures/tactical-grid-v1.json
cargo run --release -p mw-tools -- unit-fixture fixtures/movement-combat-v1.json
cargo run --release -p mw-tools -- native-tick-fixture fixtures/native-tick-v1.json
cargo run --release -p mw-tools -- ai-orders-fixture fixtures/ai-orders-v1.json
cargo run --release -p mw-tools -- territory-control-fixture fixtures/territory-control-v1.json
cargo run --release -p mw-tools -- strategic-cycle-fixture fixtures/strategic-cycle-v1.json
cargo run --release -p mw-native -- ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
```

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

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the migration boundary.
Measured Rust-versus-JavaScript results are recorded in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).
