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
- browser-compatible three-cohort influence scheduling, priority/regular
  frontier FIFOs, in-place Float32 diffusion, controller attribution,
  dirty-tile rendering, incremental census publication, and browser-cadence
  territorial cleanup (occupancy smoothing plus isolated-pocket/protrusion
  decay)
- atomic economy, occupation, resistance, capitulation, desertion, and treaty cycles
- in-process desertion, capitulation transfer, occupation seeding, and clean
  terminal conflict-resolution consequences
- one `NativeRuntime` owner connecting front layout -> AI -> simulation ->
  territory -> strategic settlement
- opt-in, bounded live battlefield resolution for current terrain/sea/city
  context, encirclement-driven combat/retreat policy, armor support, formation
  concentration, cohesion, local repulsion, pre-combat influence exclusion, and
  browser-parity sea/supply/encirclement attrition
- deterministic command-band feedback: pay-cycle bands refresh per-unit refusal,
  return-home, self-defense, influence eligibility, and planning policy
- transactional side dynamics: browser-cadence momentum history, four live war
  phases, visible-strength AI posture, combat/defense policy overlays, and
  casualty/desertion manpower continuation
- reference-counted immutable snapshots, FIFO territory render deltas, and a
  native `wgpu` unit overlay with atomic pending snapshot/territory presentation
- a dependency-free native sandbox observer HUD over immutable runtime
  publications, with renderer-local country selection and live territory,
  economy, forces, logistics, air-power, operation, and casualty summaries
- browser-matched sandbox playback controls in the top status bar: orange
  pause/green resume, clamped speed arrows, and a cycling 1x/2x/3x readout
- browser-style immutable world overlays for live airfields and wings, active
  land battles, and the selected side's task-force routes and hostile contacts
- browser-matched controller frontlines, population/zoom-filtered cities,
  curved country names, and live side-strength labels, all layered from
  immutable territory and unit publications
- a named dedicated simulation worker with bounded, lossless atomic
  publications and explicit stop/join shutdown
- versioned browser-to-native checkpoints: loadable v1-v14 state with strict
  native continuation of influence work, side dynamics,
  observer-scoped operational AI, naval planning/execution, air missions, and
  per-unit operational-feedback history, replay-safe gameplay RNG, and
  side-level recruitable personnel, aircraft and armor reserves, controlled
  airfield repair, capitulation cleanup, and monotonic unit/air-wing allocators
- deterministic naval invasion proposal and bounded sea-route generation plus
  invasion, supply, and fast-transport phase execution;
  full persistent defender-reaction recruitment/steering/cancellation; and
  fighter interception plus strike, return, rearm, airfield capture, live
  country funding coverage, and attributed aircraft/land casualties
- production checkpoint loading in the native viewer plus bounded-step,
  window-free worker validation with clean terminal-conflict completion
- optimized full-cap territory and combat hot paths using dense lookup tables,
  reusable cell tracking, and in-place prevalidated pair dispatch
- browser-matched sandbox unit markers: offline national flags, procedural
  armor and ships, zoom-density filtering, victory/encirclement/mountain
  indicators, and variable-strength formation badges
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
cargo run --release -p mw-tools -- native-tick-fixture fixtures/native-tick-v2.json
cargo run --release -p mw-tools -- ai-orders-fixture fixtures/ai-orders-v1.json
cargo run --release -p mw-tools -- territory-control-fixture fixtures/territory-control-v1.json
cargo run --release -p mw-tools -- strategic-cycle-fixture fixtures/strategic-cycle-v1.json
cargo run --release -p mw-tools -- native-runtime-fixture ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz fixtures/native-runtime-checkpoint-v1.json --json
cargo run --release -p mw-native -- ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
```

`native-runtime-fixture` accepts fourteen versioned checkpoint
boundaries. Checkpoint v1 retains `postStartWar`, which is
production-resumable only at tick, frame, and strategic cycle zero, and the
synthetic non-resumable `baselineReplay` fixture/benchmark boundary.
Checkpoint v2 adds `midWar`: a production-resumable save barrier carrying live
units, economies, occupations, total and victim-by-attacker casualties, every
territory plane, revision markers, and the last fully committed census.
Checkpoint v3 keeps that boundary and requires a strict `influenceRuntime`
block containing the frontier work needed for exact continuation.
Checkpoint v4 retains v3 and adds the strict `sideDynamics` state needed to
continue momentum sampling, phase, posture, and side personnel without reset.
Checkpoint v5 retains v4 and adds `operationalAi` (`native-operational-ai-v1`):
observer-scoped intel/contact decay, stable task-force membership/objectives and
route progress, country desperation memory, and deterministic override events.
Checkpoint v6 retains v5 and adds strict `operationalExecution` and `airPower`
blocks. They carry live naval/supply/transport phases, persistent defender
reactions, airfields, wing missions, targets, cooldowns, endurance, and rearm
timers, plus ordered per-country air-operations coverage, without reconstructing
history at load time.
Checkpoint v7 is native-authored and additionally requires `navalPlanning`
(`native-naval-planning-v1`): per-side staggered reassessment clocks and the
next deterministic operation sequence. Derived coastal topology and reusable
route-search buffers are rebuilt from immutable scenario land on load.
Checkpoint v8 retains v7 and requires every live battlefield unit's nullable
`supplyCollapsedTick`. The exact browser marker survives ticks outside the
supply scan and save/reload, while operational task forces evaluate its
inclusive 15-tick window at the browser's 35% member threshold.
Checkpoint v9 retains v8 and adds the browser's exact Mulberry32 gameplay RNG
cursor plus a complete side-level `personnelReserves` vector. Naval formations
whose sovereign has no controlled land consume one draw in reverse stable unit
order; a roll below 2% removes the formation before sea attrition and returns
live army personnel or armor crew without recording casualties.
Checkpoint v10 retains v9 and requires `native-reinforcement-v1`: ordered
country aircraft capacities/reserves, live air-operations funding, replacement
spending, and monotonic land-unit and air-wing ID cursors. Recruitment consumes
the side personnel reserve and treasury in browser RNG order; air operations,
replacement purchasing, wing reinforcement, and bounded missing-wing creation
settle after the mandatory 600-tick economy cycle.
Checkpoint v11 retains v10 and requires strict `materialLogistics`: an immutable
per-country material snapshot plus browser-ordered air funding, controlled-airfield
repair, armor/fighter/strike purchases, existing armor/wing reinforcement, and
bounded creation of one armor formation and one wing per role. Crew and treasury
bounds are transactional. Capitulation clears reserves and resolves aircraft
evacuation/loss; v10 remains loadable. This does not claim full UI parity.
Checkpoint v12 retains v11 and adds strategic missiles: default modern native
sandbox startup seeds browser-style silos; autonomous launches consume the
exact shared RNG; trails rise and fall through 40 points; hostile impacts apply
radial damage within 0.5 degrees; explosions last 30 frames; and missile state
is published immutably to observers. V12 continuation is exact. Older v1-v11
checkpoints remain loadable without missile state; this does not claim broader
full parity.
Checkpoint v13 retains the complete v12 state and adds a strict `runtimeClock`
block. It resumes the browser's foreground/background scheduler, 1x/2x/3x
speed, residual frame accumulator, and paused state exactly. Naval-operation
and defender-reaction lifecycle timers remain in browser-frame coordinates;
v1-v12 loads preserve their elapsed ages while upgrading to windowed browser
playback.
Checkpoint v14 retains the complete v13 state and requires a `gameTime` key.
The value is either `null` when the campaign calendar is disabled or a strict
`native-game-calendar-v1` state containing its Gregorian date, residual elapsed
milliseconds, and 500 ms day duration. New native wars may enable it with
`--start-date YYYY-MM-DD`; elapsed foreground/background presentation time is
scaled by the active 1x/2x/3x speed. A start year before 1942 resolves the silo
technology gate to false, so no silos are seeded. Older v1-v13 checkpoints
remain loadable without calendar state.

Dynamic-influence runtime ticks also run the browser's territorial cleanup
phases without adding a checkpoint schema: occupancy smoothing samples 5,000
cells at tick offset 83 of each 120-tick period, freezes its decisions before
applying them, and uses ascending numeric country ID to break equal counts.
Territorial-integrity cleanup samples at tick offset 67 of each 200-tick period
with `max(1000, floor(5000 / max(1, activeSides / 2)))` cells. It applies
sequential Float32 influence decay to enemy pockets and isolated friendly
protrusions. Both phases consume the shared gameplay RNG exactly, extend the
same sparse rollback transaction as influence, and report immutable per-tick
counters. Checkpoint v12 already persists the RNG cursor, live maps, and
frontier state needed for exact continuation. Legacy v1/v2 influence behavior
is intentionally unchanged.

The browser exports v1 by default for compatibility. After a war has advanced,
request the current v6 handoff explicitly from its console:

```js
await window.downloadNativeRuntimeCheckpoint({ version: 6, steps: 5 });
```

The mid-war exporter synchronously flushes census work and refuses to save if a
census remains active or dirty. It retains immutable baseline geography for
scenario production derivation; the loader derives those immutable baselines
first and only then overlays the committed live territory maps. V3 and v4
preserve the ordered priority/regular frontier queues, including observable
duplicate and stale entries. V4 also preserves every stable side's personnel,
ten-entry momentum window, current war phase, AI posture, and the nullable
captured browser desperation/reaction posture override. V5 additionally carries
the operational-AI wire contract; v6 continues its live naval, defender, and air
execution state transactionally with unit movement and combat. Task-force,
naval, and reaction membership is exclusive; claimed units remain eligible for
contact/retreat safety orders but do not consume generic frontline slots.
Native naval planning starts at tick 150, staggers sides by two ticks, and then
reassesses each side every 300 ticks. It samples at most 96 friendly and 96
hostile coastal cells, checks at most 12 invasion routes, and bounds each
four-neighbor water BFS at 120,000 visits. Like the browser reference, the BFS
does not connect the first and last grid columns. Native requires a staging
coast with five actually recruitable units before it creates an operation; this
bounded adaptation avoids emitting plans that the native executor cannot staff.
Generation-stamped route buffers are reused between checks and are deliberately
excluded from checkpoints because they cannot affect the deterministic result.

Mid-war exports may also carry a strict optional `native-battlefield-v1` block
with the exact Float32 terrain plane, urban centers, country primitives, and
every unit's encirclement/support/cohesion memory. When present, `NativeRuntime`
rebuilds position-dependent movement, combat, and influence policy every tick
from live maps and units. Checkpoints without the block remain valid and
continue under their frozen resolved-policy contract.

The browser v2-v6 handoff supplies that exact terrain plane. Stock standalone
MWSC files do not contain the browser's `mountainData`, so native-only bootstrap
explicitly disables mountain handling and uses flat terrain rather than
inventing elevation data.

Run a browser-exported production checkpoint in the native viewer, or exercise
the same dedicated runtime worker without creating a window or GPU device:

```bash
scenario=../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
checkpoint=/path/to/native-runtime-checkpoint.json
cargo run --release -p mw-native -- --runtime-checkpoint "$checkpoint" "$scenario"
cargo run --release -p mw-native -- --runtime-checkpoint "$checkpoint" --headless --ticks 5 --json "$scenario"
```

Both production paths accept exact-state v1 `postStartWar` and v2-v14 `midWar`
checkpoints. V1/v2 remain loadable under their legacy influence behavior; v3
restores the strict `influenceRuntime` state, v4 enables live side dynamics, v5
restores operational AI, v6 enables naval/defender/air execution, and native v7
also originates new naval operations. V8 retains exact recent supply-collapse
history and browser-equivalent task-force repulsion suppression; v9 adds exact
gameplay-RNG and naval-exile reserve continuation, and v10 adds reinforcement,
recruitment, and air-logistics continuation. They reject the synthetic, non-resumable
`baselineReplay` boundary. Normal startup remains a map-only viewer, and
`--demo-units` remains available as the small scenario-derived runtime.

Unit flags are resolved without runtime network access. Stock flag CDN URLs map
to the embedded 271-flag atlas, while scenario-embedded PNG, JPEG, and WebP data
flags are decoded into its reserved cells. Unknown external URLs fall back to a
matching atlas country name or the browser side color. Atlas provenance and its
MIT license are documented in `crates/mw-native/assets/flags/README.md`.

For an automated three-frame GPU/window smoke test:

```bash
cargo run --release -p mw-native -- --smoke ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
cargo run --release -p mw-native -- --smoke --demo-units ../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz
```

Native viewer controls:

- drag left mouse: pan
- mouse wheel: cursor-anchored zoom
- left click: select a country for the live observer panel, reveal its side's
  operation routes and contacts, and print its geographic cell
- click `⏸/▶`, `‹`, `1x/2x/3x`, or `›`: pause/resume and change live speed
- `Space`: pause or resume
- `H`: hide or show the observer panel
- `R`: reset camera
- `S`: save immediately when `--save-checkpoint PATH` is configured
- `Esc`: quit

The native map uses the browser's EPSG:3857 projection, `[20°, 0°]` default
center, zoom `3` start with the `2..12` Leaflet range, cursor-anchored
fractional wheel curve, and antimeridian world-copy behavior. Map materials,
country labels, units, operational overlays, and country picking all consume
the same renderer-local projection; runtime snapshots and checkpoints remain
in geographic latitude/longitude.

The panel is intentionally read-only sandbox/observer UI. It does not issue
unit orders or expose Commander Mode controls. Playback starts running at 1x,
matching the browser sandbox. The `--tick-ms` value is the native presentation
frame cadence. Foreground 1x admits one logical tick per frame, 2x admits two,
and 3x uses the browser's two-tick cap with a residual accumulator. An
unfocused window switches to the browser-style 100 ms background cadence and
drains all three 3x subticks. Foreground pause retains RAF frame advancement;
a paused background interval advances neither clock. Unfocused playback does
not submit GPU paints; refocusing forces one coherent catch-up presentation.

Pass `--start-date YYYY-MM-DD` with `--side` or `--demo-units` to enable the
strict Gregorian campaign calendar. One game day consumes 500 ms of elapsed
presentation time scaled by the current 1x/2x/3x speed; the date is shown in
the observer HUD. Exact-step `--headless --ticks` advances simulation steps but
not elapsed presentation time, so its date remains unchanged across save/resume.

Native-only startup accepts repeated `--side` selectors (country ID or unique
case-insensitive name) and uses deterministic all-Army bootstrap forces. Use
`--save-checkpoint PATH` for exact-step headless saves; windowed mode also
supports `S` and save-on-exit when a path is configured. New native-war saves
use v14 in windowed browser-clock mode, with nullable `gameTime`. Exact-step
headless saves use v12 unless `--start-date` enables the calendar and browser
clock, in which case they use v14; legacy runtimes select the newest schema
supported by their owned state.
All retain frontline objectives, assignment priors, and the refresh phase; v3+
retain frontier queues, v4+ retain momentum/phase/posture/personnel, v5 adds
operational AI, v6 adds naval/defender/air execution, v7 adds native naval
proposal cadence and operation identity, v8 adds exact operational-feedback
history, v9 adds the replay-safe RNG cursor and recruitable reserves, and v10
adds live aircraft logistics plus formation allocators. Stock MWSC bootstrap
uses the explicit flat-terrain fallback described above. This path does not yet cover every
browser AI/combat resolver or non-land force system.

Checkpoints v2-v14 are resumable-running-state formats. If a requested headless
or windowed save reaches `ConflictResolved`, the runtime finishes cleanly and
reports that the save was skipped instead of writing a terminal file that the
loader could not resume.

```bash
cargo run --release -p mw-native -- --side Germany --side Czechia --start-date 1939-09-01 --headless --ticks 2 --save-checkpoint /tmp/mw-v14.json "$scenario"
cargo run --release -p mw-native -- --runtime-checkpoint /tmp/mw-v14.json --headless --ticks 3 --save-checkpoint /tmp/mw-v14-resumed.json "$scenario"
```

`mw-native --demo-units`, production checkpoint viewing, native headless
validation, and the `mw-tools` runners all exercise the shared `NativeRuntime`.
Runtime modes give exclusive mutable ownership to the named
`mw-native-runtime` simulation thread. Each completed browser frame—or logical
step in headless compatibility mode—enters one bounded, lossless FIFO
publication containing its immutable snapshot and all associated territory
deltas. Paint admission follows the browser's 1x/2x/3x visual cadence of one,
two, or four runtime frames and a 12 ms simulation-work budget. Over-budget
atomic-commit and generic simulation frames defer painting; bounded starvation
and debounced zoom-settle force it. Pending territory deltas and the matching
newest immutable snapshot become presentation-visible atomically only when the
paint is admitted. Renderer drains may collapse intermediate snapshots only
after receiving complete publications; every territory delta remains ordered
and is applied. Shutdown explicitly requests stop and
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

This remains a bounded simulation port. Mid-war v2-v14 do not serialize a
partial census or render queues; v3+ preserve pending influence frontier work,
and v4+ preserve side dynamics. The surrender path does not yet include
the browser's
releasables, province-border smoothing, or treaty/UI presentation. The live battlefield resolver now applies
browser-parity attrition as one validated batch after staged influence and
before AI and combat: sea units take the naval attrition rule, supply collapse
and encirclement add their
bounded damage, and personnel/equipment losses are folded into runtime
casualties. This is deliberately a native adaptation of the browser's reverse
unit-loop interleaving, which can mutate one unit before a later unit is
visited; the native batch resolves one coherent pre-movement image, preserves
the same per-unit arithmetic, and makes the mutation atomic and rollback-safe.

At each 600-tick strategic boundary, economy settlement produces command bands
first. A changed band then refreshes every surviving unit's refusal cohort,
return-home/self-defense flags, influence refusal gate, and front/assignment
priors. Breakdown and mutiny use the controlled capital when available, then
the first controlled land cell in stable row-major order. That fallback remains
a deterministic first-cell adaptation and does not consume the gameplay stream.
The update is committed with the strategic cycle at the end of the
current native step, so the refreshed command order affects the next native
tick rather than the already-staged tick.

Checkpoint v8 completes browser-style operational feedback for the migrated
task-force boundary: supply-collapse markers persist through an inclusive
15-tick window, 35% formation collapse culminates and invalidates the current
intent, regrouping forces become defensive while their side is collapsing, and
same-task-force slots replace pair repulsion only when neither unit has a
hostile within 0.6 degrees. Observer-scoped posture intel and live country
desperation overrides already evolve in the same staged transaction.

Checkpoint v9 owns the browser's Mulberry32 state transactionally. Naval exile
checks preserve reverse stable unit order and branch-dependent draw counts;
successful checks remove the unit before attrition/planning/combat, recover
surviving army personnel or armor crew into the side reserve, and leave both
casualty ledgers unchanged. Failed ticks discard the staged cursor and reserve
changes, and immutable publications expose both for diagnostics.

Checkpoint v10 owns live reinforcement state transactionally. A 600-tick pay
cycle spends settled country treasuries on air operations and browser-bounded
fighter/strike replacement, then fills existing wings and creates at most one
missing wing per role and country. Every tick may recruit land formations from
the side personnel reserve using the browser's branch-dependent Mulberry32 draw
order; successful recruits publish immediately as immutable unit snapshots and
remain deployment-inactive for 30 ticks. Native settlement commits after the
current tactical step, so refreshed air funding affects the next air tick.

Checkpoint v11 material logistics runs in strict order: air funding, controlled
airfield repair, armor/fighter/strike purchases, existing armor/wing
reinforcement, then at most one armor formation and one wing per role. Crew and
treasury bounds, the immutable material snapshot, and capitulation reserve
cleanup plus aircraft evacuation/loss are included. V10 remains loadable; this
does not claim full UI parity.

Remaining bounded omissions include the full gameplay UI and broader browser
presentation parity. Windowed sandbox playback now advances `frame` once per
browser presentation boundary while all admitted logical subticks observe its
pre-increment value. Grace, active-combat, long-war, naval-operation, and
defender-reaction frame windows therefore retain browser cadence across speed
changes and v14 continuation. These contracts are not a claim of exact
full-browser tick parity outside the migrated sandbox systems.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the migration boundary.
Measured Rust-versus-JavaScript results are recorded in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).
