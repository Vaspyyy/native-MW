use mw_core::{
    DecodedScenario, GridSpec, InfluenceRuntimeState, NativeRuntime, NativeRuntimeCheckpointState,
    NativeWarBootstrapConfig, ProductionConfig, RuntimeCheckpoint, Simulation, StrategicSimulation,
    TerritoryControl, TerritoryMaps, bootstrap_native_war,
};
use serde_json::json;

fn compact_scenario() -> DecodedScenario {
    let grid = GridSpec::world(60.0).unwrap();
    assert_eq!((grid.width, grid.height), (6, 3));

    let world_control = (0..grid.height)
        .flat_map(|_| (0..grid.width).map(|x| if x < 3 { 1 } else { 2 }))
        .collect::<Vec<_>>();

    DecodedScenario {
        metadata: json!({
            "countries": [
                {"id": 1, "name": "West", "gdp": 1_000.0, "population": 100_000.0},
                {"id": 2, "name": "East", "gdp": 1_000.0, "population": 100_000.0}
            ],
            "cities": [
                {
                    "id": 1,
                    "name": "West Capital",
                    "lat": 0.0,
                    "lng": -90.0,
                    "pop": 1_000.0,
                    "ownerId": 1,
                    "isCapital": true
                },
                {
                    "id": 2,
                    "name": "East Capital",
                    "lat": 0.0,
                    "lng": 90.0,
                    "pop": 1_000.0,
                    "ownerId": 2,
                    "isCapital": true
                }
            ]
        }),
        source: grid,
        target: grid,
        entry_count: world_control.len() as u32,
        de_jure: world_control.clone(),
        land: vec![1; world_control.len()],
        biome: vec![0; world_control.len()],
        province: vec![0; world_control.len()],
        world_control,
    }
}

fn runtime() -> NativeRuntime {
    let production = ProductionConfig {
        min_expected_army_units: 6,
        ..ProductionConfig::default()
    };
    let mut bootstrapped = bootstrap_native_war(
        &compact_scenario(),
        &NativeWarBootstrapConfig {
            sides: vec![vec![1], vec![2]],
            hostility: None,
            production,
            war_grace_end: u64::MAX,
        },
    )
    .unwrap();
    let mut state = bootstrapped.checkpoint_state().unwrap();
    // The deliberately coarse test grid needs a correspondingly wide stamp;
    // browser-scale defaults are smaller than a 60-degree cell.
    for policy in &mut state.unit_policies {
        policy.influence.as_mut().unwrap().radius = 50.0;
    }
    state.battlefield = None;
    restored_runtime(state)
}

fn restored_runtime(state: NativeRuntimeCheckpointState) -> NativeRuntime {
    let runtime_config = state.runtime_config;
    let simulation = Simulation::new(state.simulation_config, state.units).unwrap();
    let mut territory =
        TerritoryControl::restore(state.territory_config, state.territory_committed_state).unwrap();
    if let Some(influence_runtime) = state.influence_runtime {
        territory
            .restore_influence_runtime(influence_runtime)
            .unwrap();
    }
    let strategic =
        StrategicSimulation::restore(state.strategic_cycle, state.economies, state.occupations)
            .unwrap();

    NativeRuntime::new(
        runtime_config,
        RuntimeCheckpoint {
            tick: state.tick,
            frame: state.frame,
            war_grace_end: state.war_grace_end,
            simulation,
            territory,
            strategic,
            scenario: state.scenario,
            diplomacy: state.diplomacy,
            unit_policies: state.unit_policies,
            battlefield: state.battlefield,
            objectives: state.objectives,
            prior_objective_by_unit: state.prior_objective_by_unit,
            front_prior_by_unit: state.front_prior_by_unit,
            last_front_refresh_tick: state.last_front_refresh_tick,
            casualties: state.casualties,
            casualties_by_victim: state.casualties_by_victim,
            side_dynamics: state.side_dynamics,
            operations: state.operations,
        },
    )
    .unwrap()
}

fn f32_bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn assert_maps_bit_exact(actual: &TerritoryMaps, expected: &TerritoryMaps) {
    assert_eq!(actual.land, expected.land);
    assert_eq!(actual.world_control, expected.world_control);
    assert_eq!(actual.de_jure, expected.de_jure);
    assert_eq!(actual.primary_occupier, expected.primary_occupier);
    assert_eq!(actual.dominant_side, expected.dominant_side);
    assert_eq!(f32_bits(&actual.occupation), f32_bits(&expected.occupation));
    assert_eq!(
        actual
            .side_influence
            .iter()
            .map(|side| f32_bits(side))
            .collect::<Vec<_>>(),
        expected
            .side_influence
            .iter()
            .map(|side| f32_bits(side))
            .collect::<Vec<_>>()
    );
}

fn assert_dynamics_checkpoint_eq(
    actual: &NativeRuntimeCheckpointState,
    expected: &NativeRuntimeCheckpointState,
) {
    assert_eq!(actual.tick, expected.tick);
    assert_eq!(actual.frame, expected.frame);
    assert_eq!(actual.units, expected.units);
    assert_maps_bit_exact(
        &actual.territory_config.maps,
        &expected.territory_config.maps,
    );
    assert_eq!(actual.influence_runtime, expected.influence_runtime);
}

fn browser_cohort(seed: f64) -> usize {
    let scaled = (seed.abs() * 2_147_483_647.0).floor();
    let stable_id = scaled.rem_euclid(4_294_967_296.0) as u32;
    stable_id as usize % 3
}

#[test]
fn influence_scheduler_observes_browser_cohort_and_combat_age_gate() {
    let mut runtime = runtime();
    let initial = runtime.latest_snapshot();
    assert_eq!(initial.frame_snapshot.units.len(), 12);
    assert_eq!(
        initial
            .frame_snapshot
            .units
            .iter()
            .filter(|unit| browser_cohort(unit.id as f64) == 1)
            .count(),
        4
    );

    // Scheduling sees the pre-step frame. Consequently lastCombatTick=0 is
    // ineligible at frames 0 through 5, even though each tick still publishes
    // its cohort phase.
    for tick in 1..=6 {
        let snapshot = runtime.step().unwrap();
        assert_eq!(snapshot.tick, tick);
        assert_eq!(snapshot.frame, tick);
        assert_eq!(snapshot.counters.influence.cohort, Some((tick % 3) as u8));
        assert_eq!(snapshot.counters.influence.sources, 0);
        assert!(
            snapshot
                .frame_snapshot
                .units
                .iter()
                .all(|unit| unit.last_combat_tick == 0)
        );
    }

    // Tick seven evaluates frame six. Cohort one is therefore the first cohort
    // allowed through the age gate, and IDs 1, 6, 7, and 12 are selected.
    let first_active = runtime.step().unwrap();
    assert_eq!(first_active.tick, 7);
    assert_eq!(first_active.counters.influence.cohort, Some(1));
    assert_eq!(first_active.counters.influence.sources, 4);
}

#[test]
fn influence_runtime_continues_bit_exactly_after_checkpoint_restore() {
    let mut uninterrupted = runtime();
    for _ in 0..8 {
        uninterrupted.step().unwrap();
    }

    let split = uninterrupted.checkpoint_state().unwrap();
    assert!(split.influence_runtime.is_some());
    let mut restored = restored_runtime(split);

    for _ in 0..8 {
        let expected_tick = uninterrupted.step().unwrap();
        let actual_tick = restored.step().unwrap();
        assert_eq!(actual_tick.tick, expected_tick.tick);
        assert_eq!(actual_tick.frame, expected_tick.frame);
        assert_eq!(
            actual_tick.frame_snapshot.units,
            expected_tick.frame_snapshot.units
        );
        assert_eq!(
            actual_tick.counters.influence,
            expected_tick.counters.influence
        );

        // Compare authoritative mutable state at the same save barrier after
        // every continued tick, including every stored f32 bit and FIFO state.
        let expected_barrier = uninterrupted.checkpoint_state().unwrap();
        let actual_barrier = restored.checkpoint_state().unwrap();
        assert_dynamics_checkpoint_eq(&actual_barrier, &expected_barrier);
    }
}

#[test]
fn checkpoint_capture_does_not_change_the_next_influence_result() {
    let mut baseline = runtime();
    let mut checkpointed = runtime();
    // Stop while an active cohort has seeded frontier work, so the save
    // barrier exercises non-empty queue state.
    for _ in 0..8 {
        let expected = baseline.step().unwrap();
        let actual = checkpointed.step().unwrap();
        assert_eq!(actual.frame_snapshot.units, expected.frame_snapshot.units);
        assert_eq!(actual.counters.influence, expected.counters.influence);
    }

    let captured = checkpointed.checkpoint_state().unwrap();
    assert!(captured.influence_runtime.is_some());

    let expected_tick = baseline.step().unwrap();
    let actual_tick = checkpointed.step().unwrap();
    assert!(expected_tick.counters.influence.diffusion_processed_items > 0);
    assert_eq!(actual_tick.tick, expected_tick.tick);
    assert_eq!(actual_tick.frame, expected_tick.frame);
    assert_eq!(
        actual_tick.frame_snapshot.units,
        expected_tick.frame_snapshot.units
    );
    assert_eq!(
        actual_tick.counters.influence,
        expected_tick.counters.influence
    );

    let expected_barrier = baseline.checkpoint_state().unwrap();
    let actual_barrier = checkpointed.checkpoint_state().unwrap();
    assert_dynamics_checkpoint_eq(&actual_barrier, &expected_barrier);
}

#[test]
fn diffusion_ignores_rows_for_retired_sides_kept_in_territory_topology() {
    let mut runtime = runtime();
    let mut state = runtime.checkpoint_state().unwrap();
    const CELL: usize = 7;

    // Capitulation keeps country-to-side attribution stable, but the browser
    // removes an emptied side from its live influence cache.
    state.diplomacy.active_sides = vec![0];
    state.territory_config.maps.dominant_side.fill(0);
    state.territory_config.maps.side_influence[0].fill(0.0);
    state.territory_config.maps.side_influence[1].fill(0.0);
    state.territory_config.maps.side_influence[1][CELL] = 0.8;
    state.influence_runtime = Some(InfluenceRuntimeState {
        regular_queue: vec![CELL],
        priority_queue: Vec::new(),
        queued_cells: vec![(CELL, 1)],
    });

    let mut resumed = restored_runtime(state);
    let before = resumed
        .checkpoint_state()
        .unwrap()
        .territory_config
        .maps
        .side_influence[1][CELL];
    let snapshot = resumed.step().unwrap();
    let after = resumed
        .checkpoint_state()
        .unwrap()
        .territory_config
        .maps
        .side_influence[1][CELL];

    assert_eq!(snapshot.counters.influence.diffusion_processed_items, 1);
    assert_eq!(after.to_bits(), before.to_bits());
}
