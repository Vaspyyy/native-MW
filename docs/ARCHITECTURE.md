# Native Modern Wars architecture

The native port is performance-first and parity-driven. It does not mirror the
browser runtime's shared mutable `main.js` structure.

## Hard boundaries

- `mw-core` owns deterministic, serializable simulation state and algorithms.
  It has no renderer, window, DOM, or platform dependencies.
- `mw-native` owns the window, input mapping, GPU resources, camera, frame
  presentation, and the named production runtime worker. The renderer reads
  immutable publications; it does not own game rules. Its window-free mode
  exercises the same worker for bounded-step production validation and clean
  terminal-conflict completion.
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
9. Load exact-geography v1 `postStartWar` and exact-live-state v2-v11 `midWar`
   checkpoints into `mw-native`, run their simulation on a dedicated worker,
   and present immutable runtime publications without blocking rendering on a
   tick.
10. Apply strategic commands inside the owned runtime: desertion, capitulation
    unit removal and territory allocation, occupation creation, and terminal
    conflict resolution.
11. Resolve the bounded live battlefield policy carried by optional v2 state:
    current terrain/city/control context, encirclement combat and retreat
    modifiers, local support/cohesion/repulsion, and pre-combat influence
    eligibility, plus browser-parity attrition as an atomic damage batch.
12. Feed settled command bands back into live per-unit policy: refusal cohorts,
    return-home/self-defense orders, influence eligibility, and front/assignment
    invalidation.
13. Run browser-compatible three-cohort influence scheduling and ordered
    priority/regular frontier diffusion, with exact state in checkpoint v3.
14. Stage live momentum, phase, posture, and personnel atomically, with exact
    continuation state in checkpoint v4.
15. Continue observer-scoped contacts, stable task forces, routes,
    desperation, and posture overrides in checkpoint v5.
16. Execute naval invasion, supply, and fast transport phases plus persistent
    defender reactions under the native tick transaction.
17. Execute fighter interception and strike missions, including return, rearm,
    airfield capture, persisted country funding coverage, and attributed
    aircraft/land casualties, and retain both execution systems exactly in
    checkpoint v6.
18. Originate native naval invasion, supply, and fast-transport operations from
    live scenario geography and unit state, then retain reassessment cadence and
    operation identity exactly in checkpoint v7.
19. Persist browser supply-collapse markers, apply their exact 15-tick
    task-force reaction window, hold regrouping forces as collapsing-side
    defense, and suppress same-task-force repulsion only outside hostile contact
    in checkpoint v8.
20. Own the browser Mulberry32 cursor transactionally, execute naval exile in
    reverse stable unit order before attrition, recover surviving personnel or
    armor crew without casualties, and persist cursor plus side reserves in v9.
21. Settle live air funding and fighter/strike replacement after each economy
    cycle, consume personnel reserves and treasury for browser-ordered land
    recruitment, publish new formations immutably, and persist exact logistics
    plus monotonic formation allocators in checkpoint v10.
22. Continue browser-ordered material logistics with airfield repair,
    armor/fighter/strike purchasing, formation reinforcement and bounded
    creation, capitulation cleanup, and immutable checkpoint v11 state.
23. Project immutable runtime publications into a compact read-only sandbox
    observer HUD. Country selection remains renderer-local and cannot mutate or
    issue commands to the simulation.
24. Mirror the browser sandbox's top status bar and `pause / down / speed / up`
    playback strip with exact 1x/2x/3x selection semantics.

Every parity-ported kernel from tactical indexing onward has a checked-in JSON
contract, a JavaScript reference runner, and a Rust fixture runner.
`scripts/verify-scenario-parity.sh` is the cross-language correctness gate and
also replays the production `NativeRuntime` fixture twice to enforce native
determinism. It also runs the browser checkpoint exporter smoke tests and the
native loader/territory-restore tests without depending on an ad hoc live
checkpoint artifact. The browser checkpoint exporter and native loader share
strict v1 through v6 handoff schemas; native v7 extends that state with naval
planning cadence, native v8 adds operational-feedback history, native v9 adds
replay-safe gameplay RNG plus recruitable reserves, native v10 adds live
reinforcement state, and native v11 adds material logistics. The complete browser tick is not
presented as a cross-language reference implementation. Benchmarks use
generated production-shaped fixtures rather than the small canonical cases.

The tactical-grid slice ports `src/tactical-grid.js`: typed unit snapshots, stable
row-major cell traversal, antimeridian wrapping, cell aggregates, and filtered
same-side pair enumeration. `mw-tools` replays the same JSON fixture through
the JavaScript and Rust implementations and benchmarks a 4,800-unit stress
case.

Normal `mw-native` startup remains map-only. The opt-in `--demo-units` mode
finds a real adjacent-country land border in the decoded scenario and
constructs a small explicit runtime. `--runtime-checkpoint PATH` instead loads
the shared strict checkpoint adapter and requires either an exact-geography v1
`postStartWar` handoff or an exact-live-state v2-v11 `midWar` handoff. The viewer
rejects `baselineReplay`; that boundary remains limited to deterministic
fixtures and benchmarks. `--headless --ticks N` runs the same checkpoint and
worker for up to `N` successful steps without constructing a window or GPU
device; a resolved conflict is reported as a clean terminal completion.

The movement/combat slice ports the resolved per-unit hot path from the browser:
final movement-distance multiplication, ordered coast deflection, stuck-target
signaling, geographic clamping and wrapping, proximity damage, direct combat,
formation and equipment losses, combined-arms modifiers, and guarded
knockback. The fixture runner replays the same ordered operations through the
JavaScript reference and Rust.

The optimized orchestration validates combat configuration at the simulation
boundary and then dispatches accepted pairs through internal prevalidated
kernels. Those kernels borrow the two live unit records mutably and in place,
avoiding temporary pair clones and repeated per-contact validation while the
checked public combat API remains unchanged. War-grace short-circuiting skips
only the proximity-contact loop; target selection and eligible direct combat
still run, preserving the browser behavior.

Terrain and country modifiers remain explicit inputs to the movement and combat
kernels. The optional live battlefield layer now derives a bounded set of those
inputs from current maps and units before invoking the kernels; older
checkpoints continue supplying their frozen resolved inputs. The AI planner
owns deterministic contact selection, retreat decisions,
sticky/capacity-limited frontline assignment, reinforcement, and fallback
field movement. It returns both resolved unit orders and the assignment records
needed by the next planning tick; it owns no clock, random source, or hidden
assignment cache. That makes the output permutation-stable and keeps policy
resolution outside the hot unit kernel.

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

The native sandbox observer retains the enclosing immutable `RuntimeSnapshot`
and derives its panel model without reaching into the worker. Its country
selection is presentation-only. Live economy records are published every tick
alongside territory, casualties, personnel, reinforcement, material, air, and
operational state; the procedural bitmap HUD is drawn after the map and units.

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

`NativeRuntime` is the sole mutable owner of the live production slice. With
influence-dynamics state, one logical step processes the priority frontier FIFO
before the regular FIFO, diffuses each side in place with Float32 rounding, and
then stamps the selected browser-compatible source cohort. With a live
battlefield block, it samples pre-movement units and maps and uses the prior
combat marker for influence eligibility before deriving current
controller-dependent policy. It subsequently refreshes the production front
layout when due, resolves AI contacts/orders and local cohesion/repulsion,
executes movement and immediate combat, derives casualties, advances or flushes
territory census work, and performs any due atomic strategic settlement before
publishing. Influence maps and queue mutations are one transaction with the
downstream AI/combat stage and roll back together on failure. Checkpoints
without influence-dynamics state retain the legacy all-source behavior.

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

Windowed playback controls are asynchronous worker requests. Pause, resume,
and tick-interval changes take effect only at a published runtime boundary; a
control received while a completed tick is backpressured is deferred until
that publication is enqueued and installed as the latest snapshot. While
paused, the worker performs no steps but continues serving checkpoint and stop
requests. Resume schedules one immediate tick and never catches up wall time
spent paused. Headless exact-step execution does not send these controls.

Application teardown sends an explicit stop request and joins the worker. A
blocked full-FIFO publication remains cancellable, and initialization, worker,
render, panic, and headless step-limit failures are surfaced as nonzero process
exits.
This moves tick latency off the presentation thread. The subsequent hot-path
optimization reduced the full-cap persistent median from 40.310 to 30.456
ms/tick, meeting a 33.33 ms 30 Hz median budget. The conservative p95 remains
131.183 ms per three-tick sample, or about 43.7 ms/tick, so 30 Hz is not yet a
tail-latency guarantee and the runtime still misses a 16.67 ms 60 Hz budget.

The browser/native handoff has seven strict versions with explicit semantic
boundaries:

- `native-runtime-checkpoint-v1` retains `postStartWar` as its only
  production-resumable boundary. It represents
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
- `native-runtime-checkpoint-v2` uses only the `midWar` boundary. It carries
  current units, economies, occupations, total casualties, nested
  `casualtiesByVictim[victim][attacker]` attribution, the complete live land,
  world-control, de-jure, primary-occupier, dominant-side, occupation, and
  side-influence planes, plus topology/world/city revisions and the last
  committed census markers. Float32 territory planes are RLE-encoded by their
  exact `u32` bit patterns. Occupation magnitudes retain those bits while their
  sign is normalized to the compact native side-index parity.
- `native-runtime-checkpoint-v3` retains the complete v2 mid-war payload and
  requires a strict `influenceRuntime` block. That block preserves pending
  priority and regular frontier FIFO entries, including history-dependent
  duplicates and entries that may be stale against the queued-state table.
  This state cannot be reconstructed from the influence planes alone.
- `native-runtime-checkpoint-v4` retains the complete v3 payload and requires
  `native-side-dynamics-v1`. That block contains one ordered record per stable
  compact side: initial/current personnel, up to ten `{frame, controlled}`
  momentum samples, the current four-state war phase, and the current
  offensive/balanced/defensive posture. A required nullable `postureOverride`
  preserves the browser's higher-priority last-stand, offensive-desperation, or
  defender-reaction decision. V4 also requires the live battlefield block
  because phase and posture are consumed by that resolver.

Checkpoint v5 retains v4 and requires `native-operational-ai-v1`. Its
`operationalAi` block is observer-scoped and deterministic: intel decay/config
and contacts, stable task-force identity/membership/objective/route progress,
country desperation counters, and ordered posture override events.

Checkpoint v6 retains v5 and requires both
`native-operational-execution-v1` and `native-air-v2`. The execution block owns
naval invasion, supply, and fast-transport phase progress, transport flags,
routes, membership, and persistent defender reactions. The air block owns
airfields and their capture state plus ordered fighter/strike wings, current
targets, endurance, cooldowns, return fields, rearm timers, and one ordered
funding-coverage record for every stable country. Both are cloned and validated
inside the same rollback-protected step as tactical planning, movement, combat,
influence, and casualties, then exposed through immutable runtime snapshots.
Aircraft losses charge one aircrew casualty each to the owning country and the
hostile attacker attribution ledger.

Checkpoint v7 retains v6 and requires `native-naval-planning-v1`. It owns one
ordered reassessment clock per stable side and the next native operation
sequence. The planner begins at tick 150, staggers sides by two ticks, and then
reassesses every 300 ticks, at most one side per native step. A reassessment
samples no more than 96 friendly and 96 hostile coastal cells, tests no more
than 12 invasion routes, and caps each deterministic four-neighbor water BFS at
120,000 visits. The BFS intentionally keeps the browser's hard first/last
column boundary rather than wrapping the dateline. Native also requires five
actually recruitable units within three degrees of a staging coast before
creating an invasion; this bounded adaptation makes each emitted plan viable
for the native executor, whose membership is created up front rather than
recruited lazily by the browser unit loop. Generation-stamped route-search
buffers and derived coastal topology are rebuilt from immutable scenario land
and are not serialized. Planner, execution, AI, movement, combat, and
territorial state still commit under the same rollback-protected tick.

Checkpoint v8 retains v7 and makes recent operational feedback exact. Every
live battlefield unit carries a required nullable `supplyCollapsedTick`; ticks
outside the browser supply scan preserve it, and task forces use its inclusive
15-tick window with the browser's 35% threshold. A collapse culminates and
invalidates attacking intent, while regrouping forces remain defensive during a
side-wide collapsing phase. The same tactical spatial grid marks task-force
units with a hostile inside 0.6 degrees, so shared task-force slots suppress
pair repulsion completely only when both units are outside contact. These fields
and transitions are staged and restored with the rest of the runtime.

Checkpoint v9 retains v8 and requires `native-gameplay-rng-v1` with algorithm
`mulberry32`, its exact unsigned 32-bit cursor, and one finite non-negative
recruitable personnel reserve per stable side. Each eligible naval formation
whose sovereign controls zero land consumes one draw in reverse stable unit
order. A draw below 2% stages a healthy non-casualty removal and credits live
army personnel or armor crew before sea attrition. RNG, reserves, unit removal,
operational reference cleanup, and publication commit together; recoverable
step failure leaves the authoritative cursor and reserves unchanged.

Checkpoint v10 retains v9 and requires `native-reinforcement-v1`. It owns one
ordered record per stable country with fighter/strike capacity and reserve,
last air-operations due/coverage and replacement spend, plus never-reused land
unit and air-wing ID cursors. After mandatory economy settlement, native pays
air operations, buys at most one percent of capacity per role, reinforces live
wings in stable ID order, and may create one missing wing per role/country. The
per-tick recruitment stage consumes the shared personnel reserve and country
treasury using the browser's branch-dependent Mulberry32 ordering, then appends
new formations atomically. New units are present in the final immutable frame
snapshot but remain excluded from planning, movement, combat, and influence for
their 30-tick deployment window. Native pay-cycle settlement completes after
the current air/tactical work, so refreshed funding gates the next air tick.

Checkpoint v11 retains v10 and requires strict `materialLogistics`. The material
stage runs in browser order: air funding, controlled-airfield repair,
armor/fighter/strike purchases, existing armor/wing reinforcement, then at most
one armor formation and one wing per role. Crew and treasury bounds are committed
with one immutable material snapshot. Capitulation clears reserves and resolves
aircraft evacuation or loss. Native v10 checkpoints remain loadable; this is not
a claim of full UI parity.

The browser keeps v1 as the default export. V2 through v6 are explicit and act as
quiescent save barriers: they synchronously flush census work, then refuse the
export if any census generation or dirty tile remains. They do not
serialize partially processed tile work, private tile summaries, or pending
render deltas. On restore, `TerritoryControl` rebuilds those private aggregates
and queues a complete renderer replacement from the exact maps while retaining
the supplied committed generation, commit sequence, mutation sequence, and
work counters.

V2 through v11 also carry the immutable starting geography from v1. The loader
applies that baseline to the MWSC scenario and derives production records
before it overlays live territory. Consequently conquest cannot rewrite immutable core,
city-population, or economy baselines merely because the save was captured
mid-war. The stable side topology includes capitulated countries, while
`activeSides` must exactly match the non-capitulated economy states.

The exporter writes unvaried influence radius/delta inputs plus the original
browser unit seed. `NativeRuntime` recomputes the logical-tick mobilization ramp
and deterministic radius/delta noise on every step. Mid-war exports
additionally carry an optional, all-or-nothing `native-battlefield-v1` block:
exact terrain Float32 bits, urban centers, combat-versus-influence country primitives, and
per-unit encirclement, armor-support, ally-strength, and cohesion state. When
present, runtime samples the pre-movement unit/map state, resolves live
movement/combat/influence modifiers, resolves local support and cohesion, then
runs AI and simulation. Influence eligibility deliberately uses the prior
combat marker so a unit entering combat still stamps on that tick and becomes
suppressed on the following tick. Old checkpoints without the block keep their
frozen resolved-policy semantics.

This resolver is deliberately narrower than the complete browser tick.
Encirclement history feeds combat, retreat, and attrition; sea exposure,
supply collapse, and encirclement damage are calculated per unit after staged
influence and committed through one validated batch before AI and combat. The batch is an intentional
ordering adaptation: the browser's reverse unit loop can interleave attrition
mutation with later unit planning, while native computes all damage from one
pre-tick image and applies it atomically so a later failure can roll back the
whole step.

At a 600-tick pay boundary, `StrategicSimulation` settles treasuries and bands
before native stages desertion/capitulation effects. Native then resolves the
new band against every surviving unit, updates refusal/return-home/self-defense
and influence gates, and clears the affected unit's objective and assignment
priors. These changes are committed with the strategic result at the end of
the current step and are consumed by the next native tick. Breakdown/mutiny
home targets use a controlled capital, with the first controlled land cell in
stable row-major order as fallback; this replaces the browser's RNG reservoir
fallback with a replay-safe choice.

When v4 dynamics are present, each native step first clones the complete side
state. Tick `37 + 200n` samples the last committed country-control census using
the current pre-step frame. The phase resolver keeps the browser's exact
three-sample gate, ten-sample window, slope thresholds, trend counting, and
10% manpower collapse override. Posture is refreshed every tick from deployed
live formation strength with the browser's `> 1.5` offensive, `< 0.7`
defensive, and 15% manpower-defense thresholds. Native currently uses
observer-scoped known hostile strength whenever operational AI is present;
legacy checkpoints without that state retain authoritative visible strength.

The browser exports the pre-auto-posture country speed baseline for v4 so a
later native posture transition can both apply and remove the defensive cap.
Country desperation is resampled from live control, cities, personnel, and
stall history, and its posture overrides evolve transactionally with defender
reaction and recent supply collapse.
Inactive stable sides reset their resolved posture to `BALANCED` each tick,
matching the browser, without discarding their frozen history or captured
override.

The staged phase is applied to same-tick battlefield combat. Defensive posture
sets the AI's defensive-only planning overlay and browser severity-scaled speed
cap without altering command-band refusal state. Combat/attrition casualty
deltas and strategic desertion losses reduce the staged personnel pool;
capitulation-only unit removal does not consume the retired side's remaining
pool. The clone is installed only with the final successful runtime commit, so
any planning, simulation, influence, or strategic failure leaves history,
phase, posture, and personnel unchanged. V1-v3 omit this state and intentionally
retain their frozen phase/policy behavior.

Remaining omissions are full gameplay/UI parity and broader browser presentation
parity; equipment-reserve evolution is covered by v11 material logistics.

The native runtime advances `frame` once per successful logical step. The
browser can run multiple logical subticks before advancing its RAF-owned
`simFrameCount`, depending on speed and background cadence. A handoff preserves
the captured numeric frame, but subsequent grace, active-combat, and long-war
frame windows follow deterministic native cadence rather than reproducing a
different browser scheduling cadence.

Browser v2-v6 exports carry the exact terrain plane used by this resolver. The
standalone stock MWSC format has no `mountainData`; native-only bootstrap
therefore explicitly disables mountain handling and seeds flat terrain instead
of inferring it from unrelated scenario fields.

V1 validation is unchanged: live mid-war territory or nested casualty state is
rejected under the v1 schema. V2 requires `midWar`, immutable baseline
geography, complete live territory, advancing committed census markers, a
non-exhausted completed strategic-cycle coordinate, and exact active-side and
casualty coverage. V3 requires all of that plus `influenceRuntime`; older
schemas reject that field. The strategic cycle is intentionally
independent of `floor(tick / 600)` because browser God-mode can force a valid
economy cycle without advancing the simulation clock.

## Territory and strategic state

`TerritoryControl` owns the mutable dense control maps. Influence is stored and
updated with browser-compatible Float32 rounding, hostile decay, reclaim rules,
primary occupier credit, and controller hysteresis. Mutations mark census and
render tiles instead of forcing a whole-map rescan or texture upload.

Country-to-side resolution uses a dense table covering the complete `u16` ID
space, and city membership uses a dense cell mask. Influence application
deduplicates touched, controller-changed, and credit-changed cells with
persistent masks and reusable vectors; only recorded bits are cleared between
applications. The published cell lists retain their sorted, unique contract,
but ordered-tree lookup and allocation are removed from the source-stamping hot
loop.

The influence-dynamics path selects one stable unit cohort per logical tick,
rotates its bounded source scan, and triples the selected source delta to
represent the three-tick stride. Before those sources stamp, frontier work is
consumed from snapshotted priority and regular FIFO ends. Queue upgrades can
leave stale entries, and re-enqueueing can make an older duplicate observable;
both behaviors are preserved. Each processed cell updates side planes in side
order, in place, with browser-compatible Float32 rounding before controller
synchronization and source enqueueing.

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

The strategic snapshot remains an auditable command/event boundary, but
`NativeRuntime` now applies its bounded consequences inside the same owned pay
cycle. Settlement is prepared without mutating strategic state. A staged unit
simulation applies desertion first and then removes surviving units belonging
to a capitulated country. The surrender allocator preserves cells already
credited to a hostile physical occupier, distributes remaining victim-owned
land deterministically from victim-by-attacker casualty shares (with physical
control and nearest-force fallbacks), updates every live controller/influence
plane, advances territory revision/census markers, and registers the resulting
occupation under its primary annexer. Only after all of that validates does the
runtime commit strategic state and publish the cross-kernel snapshot.

A surrender clears native front objectives and assignment priors so the next
tick derives a fresh layout from the transferred map. Conflict resolution
publishes its decided kind and optional winner in the final immutable snapshot,
enters `ConflictResolved`, and cleanly stops later stepping. The legacy
`AwaitingStrategicEffects` state remains only as a safety gate when restoring an
older internally constructed strategic snapshot that already contains
unapplied commands; newly executed cycles do not use it.

The consequence port is intentionally bounded. It does not yet reproduce
browser releasables, province-border smoothing, or treaty/UI presentation.
Checkpoint v11 covers material reserve and aircraft cleanup. Browser-authored legacy v2 checkpoints
omit the optional native planner block and therefore start a fresh deterministic
front planning boundary. Native-authored v2-v11 saves include current objectives,
assignment priors, layout priors, and the last refresh tick. New native-war
saves use v11; legacy runtimes select the newest schema supported by their owned
continuation state.

The native writer accepts only the canonical runtime, simulation, territory
tile, city, protection, and contiguous-side configuration that the strict
mid-war loader can reconstruct. `ConflictResolved` is terminal rather than
resumable; a configured save is explicitly skipped at that state instead of producing a
false continuation artifact.

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
   **Complete in `mw-core`, including in-process unit/territory consequences,
   occupation seeding, and terminal conflict publication under
   `NativeRuntime`.**
8. Derive production scenario inputs and front objectives, then connect AI,
   simulation, territory, and strategic kernels under one runtime owner.
   **Complete for the bounded v1-v11 checkpoint contracts and runtime.**
9. Load production checkpoints in `mw-native`. **Complete for strict,
   exact-geography `postStartWar` and exact-live-state `midWar` viewer/headless
   operation; `baselineReplay` is deliberately rejected.**
10. Move production simulation onto a dedicated thread, keeping presentation
    on immutable publications and every territory delta lossless. **Complete.**
11. Optimize full-cap territory stamping and combat-pair dispatch without
    changing parity outputs. **Complete; the persistent median now meets the
    30 Hz tick budget, while p95 and 60 Hz do not.**
12. Add quiescent checkpoint v2 with exact live territory, committed census,
    and nested casualty attribution while preserving strict v1 compatibility.
    **Complete.**
13. Bootstrap a deterministic war directly from MWSC scenario countries and
    save/reload native mid-war checkpoints without a browser handoff.
    **Complete for all-Army forces through the viewer and exact-step headless
    paths, including history-dependent frontline and naval planner state.**
14. Recompute live battlefield terrain, urban, encirclement, support,
    concentration, cohesion, repulsion, and active-combat influence policy.
    **Complete for the bounded optional-mid-war/native-bootstrap resolver,
    including hostile-gated task-force repulsion suppression. Older checkpoints
    intentionally retain frozen-policy behavior.**
15. Port browser influence cohorts and priority/regular frontier diffusion,
    then persist the exact queue state in checkpoint v3. **Complete.**
16. Continue live momentum, phase, posture, and side personnel transactionally,
    then persist them in strict checkpoint v4. **Complete, with operational
    posture consuming observer-scoped hostile-strength intel when available.**
17. Continue operational AI in checkpoint v5, then execute naval transport,
    air operations, and full defender-reaction planning transactionally in
    checkpoint v6. **Complete for browser handoff and native continuation.**
18. Originate native naval plans and bounded sea routes, connect them to
    same-tick execution and defender reaction, and checkpoint their cadence in
    v7. **Complete.**
19. Persist browser supply-collapse history and exact task-force reactions,
    then apply hostile-gated shared-task-force repulsion in checkpoint v8.
    **Complete.**
20. Own the browser Mulberry32 gameplay cursor transactionally, apply reverse
    stable-order naval exile disbandment before attrition, recover live crew or
    personnel without casualties, and persist the cursor plus side reserves in
    checkpoint v9. **Complete.**
21. Settle aircraft-reserve production/replacement, consume side personnel and
    country treasury for recruitment, publish new unit snapshots, and persist
    exact continuation in checkpoint v10. **Complete.**
22. Settle browser-ordered material logistics, publish immutable armor and
    logistics snapshots, resolve capitulation air evacuation/loss, and persist
    exact continuation in checkpoint v11. **Complete.**
23. Add a read-only sandbox observer HUD over immutable runtime publications,
    with renderer-local country selection and no command path back into the
    worker. **Complete.**
24. Add the browser sandbox's top status bar and exact four-button playback
    strip, including tick-boundary pause/resume and live 1x/2x/3x cadence.
    **Complete.**
25. Native gameplay UI/editor/community parity remains later work after the
    remaining simulation boundaries are chosen and measured.

Player order controls, Commander Mode, the full gameplay HUD, map editor,
online/community features, and satellite-map parity are still outside
the native port. The migrated kernel and handoff contracts do not imply exact
full-browser tick parity.

Native-only startup accepts repeated `--side` selectors (numeric IDs or unique
case-insensitive names), with deterministic all-Army bootstrap forces.
`--save-checkpoint PATH` saves at an exact headless step boundary; windowed mode
also supports `S` and save-on-exit when configured. This remains native-only
and does not claim full browser AI/combat or non-land-force parity.
