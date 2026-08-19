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
3. Render the real ownership grid in a native `wgpu` window with pan and zoom.
4. Validate the core headlessly against the checked-in web scenarios.

The second slice ports `src/tactical-grid.js`: typed unit snapshots, stable
row-major cell traversal, antimeridian wrapping, cell aggregates, and filtered
same-side pair enumeration. `mw-tools` replays the same JSON fixture through
the JavaScript and Rust implementations and benchmarks a 4,800-unit stress
case.

Normal `mw-native` startup remains map-only. The opt-in `--demo-units` mode
finds a real adjacent-country land border in the decoded scenario, feeds
resolved orders through the native tick, and renders its immutable snapshots.
This exercises the integration without adding synthetic work to normal viewer
startup.

The third core slice ports the resolved per-unit hot path from the browser:
final movement-distance multiplication, ordered coast deflection, stuck-target
signaling, geographic clamping and wrapping, proximity damage, direct combat,
formation and equipment losses, combined-arms modifiers, and guarded
knockback. The fixture runner replays the same ordered operations through the
JavaScript reference and Rust.

Target selection and strategic context remain on the caller side of this
boundary. The browser still resolves plans, waypoints, retreat, terrain and
country modifiers, defense context, and whether a pair should fight. Rust then
applies the resolved movement or combat operation with the browser's immediate
mutation order. This keeps the migration exact without copying the entire
shared-state simulation loop at once.

The current slice intentionally excludes AI planning, territory and
occupation, economy and surrender, air/naval simulation, the map editor,
online/community features, satellite tiles, and full HUD parity.

The fourth slice connects these boundaries into `mw-core::Simulation`. Each
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

## Rendering model

The world map is a regular geographic grid. Ownership IDs are uploaded as an
integer GPU texture and converted to colors in WGSL. Borders are detected from
neighboring ownership IDs in the shader. Later passes will consume immutable
frame snapshots for units, cities, frontlines, labels, and effects.

## Migration order

1. Scenario codec and direction field. **Complete.**
2. Tactical spatial grid and unit-neighbor queries. **Complete in `mw-core`.**
3. Resolved unit movement and immediate pair combat. **Complete in `mw-core`.**
4. Native tick orchestration: resolved-order adapter, tactical contact
   dispatch, immutable unit snapshots, and renderer consumption. **Complete.**
5. Economy, occupation, surrender, and AI jobs.
6. Native UI/editor/community parity only after the simulation benchmark shows
   the native core is worth continuing.
