# Native Modern Wars architecture

The native port is performance-first and parity-driven. It does not mirror the
browser runtime's shared mutable `main.js` structure.

## Hard boundaries

- `mw-core` owns deterministic, serializable simulation state and algorithms.
  It has no renderer, window, DOM, or platform dependencies.
- `mw-native` owns the window, input mapping, GPU resources, camera, frame
  presentation, and the named production runtime worker. The renderer reads
  immutable publications; it does not own game rules. Its window-free mode
  exercises the same worker for exact-step production validation.
- `mw-tools` owns parity fixtures, fixture inspection, and benchmarks, and
  exposes the strict checkpoint adapter shared with `mw-native`.
- Existing `.mwsc.gz` scenarios are the compatibility boundary while the web
  and native versions coexist.

## Completed vertical slices

1. Decode MWSC v2 scenarios with the same dense arrays as the JavaScript codec.
2. Port the deterministic frontline direction-field BFS with preserved scan and
   neighbor ordering.
3. Port tactical indexing, resolved movement/combat, and whole-tick unit
   orchestration.
4. Publish immutable unit snapshots and render them over the real ownership
   grid in a native `wgpu` window.
5. Resolve deterministic AI orders from immutable tactical, territory, and
   frontline inputs.
6. Apply territory influence, publish dirty render tiles, and incrementally
   commit country and side census snapshots.
7. Settle economy, occupation, resistance, capitulation, desertion, and treaty
   consequences as one atomic strategic pay cycle.
8. Derive production country/city/economy inputs and production front layouts
   from an MWSC scenario, then connect the migrated kernels under one native
   tick owner.
9. Load exact-geography `postStartWar` checkpoints into `mw-native`, run their
   simulation on a dedicated worker, and present immutable runtime publications
   without blocking rendering on a tick.

Every parity-ported kernel from tactical indexing onward has a checked-in JSON
contract, a JavaScript reference runner, and a Rust fixture runner.
`scripts/verify-scenario-parity.sh` is the cross-language correctness gate and
also replays the production `NativeRuntime` fixture twice to enforce native
determinism. The browser checkpoint exporter and native loader share the v1
handoff schema, but the complete browser tick is not presented as a
cross-language reference implementation. Benchmarks use generated
production-shaped fixtures rather than the small canonical cases.

The tactical-grid slice ports `src/tactical-grid.js`: typed unit snapshots, stable
row-major cell traversal, antimeridian wrapping, cell aggregates, and filtered
same-side pair enumeration. `mw-tools` replays the same JSON fixture through
the JavaScript and Rust implementations and benchmarks a 4,800-unit stress
case.

Normal `mw-native` startup remains map-only. The opt-in `--demo-units` mode
finds a real adjacent-country land border in the decoded scenario and
constructs a small explicit runtime. `--runtime-checkpoint PATH` instead loads
the shared strict checkpoint adapter and requires an exact-geography,
production-resumable `postStartWar` handoff. The viewer rejects
`baselineReplay`; that boundary remains limited to deterministic fixtures and
benchmarks. `--headless --ticks N` runs the same checkpoint and worker for
exactly `N` successful steps without constructing a window or GPU device.

The movement/combat slice ports the resolved per-unit hot path from the browser:
final movement-distance multiplication, ordered coast deflection, stuck-target
signaling, geographic clamping and wrapping, proximity damage, direct combat,
formation and equipment losses, combined-arms modifiers, and guarded
knockback. The fixture runner replays the same ordered operations through the
JavaScript reference and Rust.

Terrain and country modifiers remain explicit caller inputs to the movement and
combat kernels. The AI planner now owns deterministic contact selection,
retreat decisions, sticky/capacity-limited frontline assignment,
reinforcement, and fallback field movement. It returns both resolved unit
orders and the assignment records needed by the next planning tick; it owns no
clock, random source, or hidden assignment cache. That makes the output
permutation-stable and keeps strategic policy outside the hot unit kernel.

The native-tick slice connects these boundaries into `mw-core::Simulation`. Each
logical tick rebuilds one tactical snapshot, consumes caller-resolved orders,
executes units in reverse stable-storage order, applies every directed hostile
proximity contact immediately, attempts direct combat against the preferred or
first stable-ID target, falls back to resolved movement, and removes defeated
units only at the cleanup barrier. Missing orders hold; stale casualty orders
and vanished preferred targets are ignored like the browser loop.

`FrameSnapshot` owns ID-sorted unit data and ordered combat/removal artifacts
through reference-counted immutable slices. The renderer can retain a frame
while the simulation advances without reading mutable gameplay state. The GPU
overlay uploads a snapshot only when a new one is published and reuses its CPU
and GPU instance capacity.

## Production orchestration boundary

`derive_scenario_production` converts decoded MWSC countries, current control,
de-jure ownership, cities, population, and GDP into stable country records,
city records, economy seeds, capital cells, initial-control baselines, and
expected force sizes. Current control is authoritative for the starting owned
cell and unresolved-city fallback counts; de-jure ownership remains a separate
validated input. No browser DOM or mutable `main.js` state enters this layer.

The production front-layout kernel scans the dense dominant-side map in stable
row-major order, creates directed-hostility segments, allocates capacity-limited
slots, and publishes ordered AI objectives plus sticky prior assignments. A
side receives slots only on a direction in which that side is hostile, so an
asymmetric diplomacy matrix does not silently consume reverse-front capacity.

`NativeRuntime` is the sole mutable owner of the live production slice. One
logical step executes these boundaries in order:

1. refresh the production front layout when due;
2. resolve AI contacts and orders from immutable inputs;
3. execute movement and immediate combat through `Simulation`;
4. derive casualties and stamp surviving active-unit influence;
5. advance or flush the territory census; and
6. run the atomic strategic cycle at its pay boundary.

Units still inside their deployment delay are published to the renderer, but
are excluded from front slots, AI planning, tactical contacts, movement,
combat, and territory influence until activation. The runtime publishes the
new cross-kernel snapshot only after the step succeeds. Consumers receive an
`Arc<RuntimeSnapshot>` and cannot observe its simulation, territory, or
strategic components mutating underneath them. Territory upload payloads are
also immutable and leave the runtime through a FIFO queue, so a later dirty
tile cannot overtake an earlier one.

In `mw-native`, the named `mw-native-runtime` thread has exclusive mutable
ownership of `NativeRuntime`. Each initial state or completed tick is sent as
one atomic publication containing its immutable snapshot and every territory
render delta produced before that snapshot. The publication channel is bounded
and lossless: a full FIFO backpressures simulation instead of dropping or
reordering territory work. A renderer drain applies every delta in FIFO order
and may retain only the newest snapshot among the complete publications it
received. The separate latest-snapshot mailbox is newest-wins telemetry; it is
not used to splice a snapshot onto unrelated delta state.

Application teardown sends an explicit stop request and joins the worker. A
blocked full-FIFO publication remains cancellable, and initialization, worker,
render, panic, and exact-step failures are surfaced as nonzero process exits.
This moves tick latency off the presentation thread but does not reduce the
work itself: the measured full-cap persistent runtime is about 40 ms/tick and
therefore still misses a 33.33 ms 30 Hz budget as well as 60 Hz.

The browser/native handoff is versioned as
`native-runtime-checkpoint-v1` and makes its semantic boundary explicit:

- `postStartWar` is the only production-resumable boundary in v1. It represents
  the state immediately after browser `startWar()` and before the first
  simulation tick. The loader requires tick `0`, frame `0`, strategic cycle
  `0`, no occupations, explicit zero casualties, deployment-adjusted starting
  economies, and exact RLE snapshots of the browser's active land,
  world-control, and de-jure grids. The MWSC hash still pins metadata and city
  identity; the checkpoint maps are authoritative for editor-derived or
  prederived geography.
- `baselineReplay` is a synthetic fixture/benchmark boundary. It may begin at a
  later clock or pay-cycle edge, but it still reconstructs scenario-baseline
  territory and therefore is deliberately reported as non-resumable. It must
  never be presented as a mid-war save or production continuation.

The exporter writes unvaried influence radius/delta inputs plus the original
browser unit seed. `NativeRuntime` recomputes the logical-tick mobilization ramp
and deterministic radius/delta noise on every step. Terrain, urban defense,
formation support, cohesion, encirclement, and similar resolved AI/combat
modifiers are still captured at the handoff and then owned by native policy;
porting their live resolvers remains later work.

This distinction is necessary because checkpoint v1 does not serialize live
side-influence planes, primary/dominant controller planes, or an in-progress
census. Adding those maps is a future mid-war checkpoint version, not an
implicit relaxation of v1 validation.

## Territory and strategic state

`TerritoryControl` owns the mutable dense control maps. Influence is stored and
updated with browser-compatible Float32 rounding, hostile decay, reclaim rules,
primary occupier credit, and controller hysteresis. Mutations mark census and
render tiles instead of forcing a whole-map rescan or texture upload.

The current territory boundary covers direct source stamping and attribution.
The browser's separate frontier-diffusion pass and active-combat exclusion are
still upstream work, so territory parity means parity with the checked-in
bounded contract, not with every operation in the browser's full tick.

The census processes deterministic tile-local work under an item budget. A
generation is published only after every dirty tile, its cities, and any dirty
tail created during the scan have been folded into one coherent result.
Consumers receive reference-counted immutable `TerritorySnapshot` values;
renderers separately receive compact `TerritoryRenderUpdate` tile payloads.
No partially processed census is externally visible.

`StrategicSimulation` consumes territory-derived aggregates tagged with census
generation, commit sequence, and freshness at the 600-tick pay boundary. It
first prepares occupation due/yield and garrison requirements, then settles
country treasuries and command bands, applies funding-dependent resistance,
evaluates capitulation and desertion, and finally resolves the global conflict.
All work happens in private copies. Validation or arithmetic failure leaves the
previous economy, occupation, cycle counter, and published snapshot unchanged.
Ticks must advance strictly, while territory generation and commit markers may
stay stable when no new census was necessary but may never regress. This makes
replayed commands fail before treasury or resistance state can be settled twice.

The strategic snapshot is the command boundary for higher-level gameplay:
desertion and surrender are explicit commands, while budget, command-band,
resistance, capitulation, and treaty changes are explicit events. Applying
territory transfers and deleting units remains outside the current runtime
slice, so the core does not mutate another subsystem behind its snapshot
boundary. If a strategic publication contains any desertion, surrender, or
conflict-resolution command, `NativeRuntime` enters
`AwaitingStrategicEffects` and refuses another step. Resumption requires an
authoritative replacement checkpoint with every consequence already applied;
there is intentionally no receipt or acknowledge-and-continue escape hatch.

## Rendering model

The world map is a regular geographic grid. Ownership IDs are uploaded as an
integer GPU texture and converted to colors in WGSL. Borders are detected from
neighboring ownership IDs in the shader. After initial upload, territory
changes are transferred as bounded FIFO dirty tiles. Units are rendered from
reference-counted immutable frame snapshots; later passes can use the same
publication model for cities, frontlines, labels, and effects.

## Migration order

1. Scenario codec and direction field. **Complete.**
2. Tactical spatial grid and unit-neighbor queries. **Complete in `mw-core`.**
3. Resolved unit movement and immediate pair combat. **Complete in `mw-core`.**
4. Native tick orchestration: resolved-order adapter, tactical contact
   dispatch, immutable unit snapshots, and renderer consumption. **Complete.**
5. Native AI order resolution and assignment publication. **Complete in
   `mw-core`; connected through both the demo and production runtime paths.**
6. Territory influence, control attribution, dirty render tiles, and atomic
   census snapshots. **Complete in `mw-core`; connected to the demo and strict
   production-checkpoint renderer paths.**
7. Economy, occupation, surrender, and atomic strategic orchestration.
   **Complete in `mw-core` and connected under `NativeRuntime`.**
8. Derive production scenario inputs and front objectives, then connect AI,
   simulation, territory, and strategic kernels under one runtime owner.
   **Complete for the bounded v1 checkpoint contract and runtime.**
9. Load production checkpoints in `mw-native`. **Complete for strict,
   exact-geography `postStartWar` viewer and exact-step headless operation;
   `baselineReplay` is deliberately rejected.**
10. Move production simulation onto a dedicated thread, keeping presentation
    on immutable publications and every territory delta lossless. **Complete.**
11. Native UI/editor/community parity only after the simulation benchmark shows
    the native core is worth continuing.

The next checkpoint/runtime work is a new version carrying live influence,
controller, and census state plus strategic consequence application for
resumable mid-war play; v1 validation is not relaxed to simulate this.

Air/naval simulation, the full gameplay HUD, map editor, online/community
features, and satellite-map parity are still outside the native port.
