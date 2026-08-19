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
