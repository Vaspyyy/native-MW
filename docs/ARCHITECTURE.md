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

The tactical grid is not executed during `mw-native` viewer startup. The viewer
does not own units or a simulation tick yet, so doing that would add synthetic
startup work rather than integrate real gameplay. It will enter the native
runtime with the unit movement/combat slice.

This slice intentionally excludes battle rules, AI planning, the map editor,
online/community features, satellite tiles, and full HUD parity.

## Rendering model

The world map is a regular geographic grid. Ownership IDs are uploaded as an
integer GPU texture and converted to colors in WGSL. Borders are detected from
neighboring ownership IDs in the shader. Later passes will consume immutable
frame snapshots for units, cities, frontlines, labels, and effects.

## Migration order

1. Scenario codec and direction field. **Complete.**
2. Tactical spatial grid and unit-neighbor queries. **Complete in `mw-core`.**
3. Deterministic unit movement/combat slice. **Next.**
4. Economy, occupation, surrender, and AI jobs.
5. Native UI/editor/community parity only after the simulation benchmark shows
   the native core is worth continuing.
