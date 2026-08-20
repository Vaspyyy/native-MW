# Native Modern Wars architecture

The native port is performance-first and parity-driven. It does not mirror the
browser runtime's shared mutable `main.js` structure.

## Hard boundaries

- `mw-core` owns deterministic, serializable simulation state and algorithms.
  It has no renderer, window, DOM, or platform dependencies.
- `mw-native` owns the window, input mapping, GPU resources, camera, and frame
  presentation. The renderer reads snapshots; it does not own game rules.
- `mw-tools` owns headless validation, fixture inspection, and benchmarks.
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

Every migrated simulation slice from tactical indexing onward has a checked-in
JSON contract, a JavaScript reference runner, and a Rust fixture runner.
`scripts/verify-scenario-parity.sh` is the cross-language correctness gate;
benchmarks use generated production-shaped fixtures rather than the small
canonical cases.

The tactical-grid slice ports `src/tactical-grid.js`: typed unit snapshots, stable
row-major cell traversal, antimeridian wrapping, cell aggregates, and filtered
same-side pair enumeration. `mw-tools` replays the same JSON fixture through
the JavaScript and Rust implementations and benchmarks a 4,800-unit stress
case.

Normal `mw-native` startup remains map-only. The opt-in `--demo-units` mode
finds a real adjacent-country land border in the decoded scenario, feeds
resolved orders through the native tick, and renders its immutable snapshots.
This exercises the integration without adding synthetic work to normal viewer
startup.

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
territory transfers and deleting units remains the caller's job, so the core
does not mutate another subsystem behind its snapshot boundary.

## Rendering model

The world map is a regular geographic grid. Ownership IDs are uploaded as an
integer GPU texture and converted to colors in WGSL. Borders are detected from
neighboring ownership IDs in the shader. After initial upload, territory
changes are transferred as bounded dirty tiles. Units are rendered from
immutable frame snapshots; later passes can use the same publication model for
cities, frontlines, labels, and effects.

## Migration order

1. Scenario codec and direction field. **Complete.**
2. Tactical spatial grid and unit-neighbor queries. **Complete in `mw-core`.**
3. Resolved unit movement and immediate pair combat. **Complete in `mw-core`.**
4. Native tick orchestration: resolved-order adapter, tactical contact
   dispatch, immutable unit snapshots, and renderer consumption. **Complete.**
5. Native AI order resolution and assignment publication. **Complete in
   `mw-core`; connected to the opt-in demo tick.**
6. Territory influence, control attribution, dirty render tiles, and atomic
   census snapshots. **Complete in `mw-core`; connected to the opt-in demo
   renderer path.**
7. Economy, occupation, surrender, and atomic strategic orchestration.
   **Complete in `mw-core`.**
8. Derive full production objectives, garrisons, and strategic commands from a
   loaded scenario, then replace the remaining browser orchestration adapters.
9. Native UI/editor/community parity only after the simulation benchmark shows
   the native core is worth continuing.

Air/naval simulation, the full gameplay HUD, map editor, online/community
features, and satellite-map parity are still outside the native port.
