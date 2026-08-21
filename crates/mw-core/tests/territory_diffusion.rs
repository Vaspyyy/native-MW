use std::collections::{BTreeMap, BTreeSet};

use mw_core::{
    InfluenceRuntimeState, InfluenceSource, TerritoryConfig, TerritoryControl, TerritoryMaps,
};

const WIDTH: usize = 5;
const HEIGHT: usize = 5;
const CENTER: usize = 12;
const NEIGHBORHOOD: [usize; 5] = [CENTER, 11, 13, 7, 17];

fn maps() -> TerritoryMaps {
    let cells = WIDTH * HEIGHT;
    TerritoryMaps {
        land: vec![2; cells],
        world_control: vec![1; cells],
        de_jure: vec![1; cells],
        primary_occupier: vec![1; cells],
        dominant_side: vec![0; cells],
        occupation: vec![0.0; cells],
        side_influence: vec![vec![0.0; cells]; 2],
    }
}

fn config(maps: TerritoryMaps) -> TerritoryConfig {
    TerritoryConfig {
        width: WIDTH,
        height: HEIGHT,
        grid_resolution: 1.0,
        max_sides: 2,
        tile_size: WIDTH,
        maps,
        country_to_side: BTreeMap::from([(1, 0), (2, 1)]),
        hostility_matrix: vec![0, 1, 1, 0],
        cities: Vec::new(),
        protected_owner_ids: BTreeSet::new(),
        topology_revision: 1,
        world_revision: 1,
        city_revision: 1,
    }
}

fn queued_center() -> InfluenceRuntimeState {
    InfluenceRuntimeState {
        regular_queue: vec![CENTER],
        priority_queue: Vec::new(),
        queued_cells: vec![(CENTER, 1)],
    }
}

fn source(delta: f64) -> InfluenceSource {
    InfluenceSource {
        id: 7,
        side: 0,
        sovereign: 1,
        beneficiary: 1,
        lat: -88.0,
        lng: -178.0,
        radius: 0.5,
        delta,
        concentration_bonus: 1.0,
        owner_ally_country_ids: BTreeSet::from([1]),
        protected_owner_ids: BTreeSet::new(),
        rebel_de_jure: None,
        credit_de_jure: None,
        credit_de_jure_by_country: BTreeMap::new(),
        refuses_offense: false,
    }
}

fn assert_queue_neighborhood(state: &InfluenceRuntimeState, priority: bool) {
    let expected_cells = vec![
        (7, priority as u8 + 1),
        (11, priority as u8 + 1),
        (12, priority as u8 + 1),
        (13, priority as u8 + 1),
        (17, priority as u8 + 1),
    ];
    if priority {
        assert!(state.regular_queue.is_empty());
        assert_eq!(state.priority_queue, NEIGHBORHOOD);
    } else {
        assert_eq!(state.regular_queue, NEIGHBORHOOD);
        assert!(state.priority_queue.is_empty());
    }
    assert_eq!(state.queued_cells, expected_cells);
}

#[test]
fn browser_float32_fixture_diffuses_in_place_and_requeues_the_frontier() {
    let mut fixture = maps();
    fixture.dominant_side[11] = 1;
    fixture.side_influence[0][CENTER] = 0.8;
    fixture.side_influence[1][CENTER] = 0.3;
    for cell in [11, 13, 7, 17] {
        fixture.side_influence[0][cell] = 0.4;
        fixture.side_influence[1][cell] = 0.7;
    }

    let mut territory = TerritoryControl::new(config(fixture)).unwrap();
    territory
        .restore_influence_runtime(queued_center())
        .unwrap();

    let (_, diffusion) = territory.apply_influence_runtime(&[], &[0, 1], 1).unwrap();

    assert_eq!(diffusion.processed_items, 1);
    assert_eq!(diffusion.stale_entries, 0);
    assert_eq!(diffusion.requeued_cells, 1);
    assert_eq!(
        territory.side_influence(0).unwrap()[CENTER].to_bits(),
        0x3f2a_aaab
    );
    assert_eq!(
        territory.side_influence(1).unwrap()[CENTER].to_bits(),
        0x3e9f_49f5
    );
    assert_eq!(territory.dominant_side()[CENTER], 0);
    assert_eq!(territory.influence_runtime_state(), Some(queued_center()));
}

#[test]
fn queued_non_frontier_land_cell_is_smoothed_once_without_requeue() {
    let mut fixture = maps();
    fixture.side_influence[0][CENTER] = 0.8;

    let mut territory = TerritoryControl::new(config(fixture)).unwrap();
    territory
        .restore_influence_runtime(queued_center())
        .unwrap();

    let (_, diffusion) = territory.apply_influence_runtime(&[], &[0], 1).unwrap();

    let expected = (0.8_f64 * 0.75 + (0.8_f64 / 9.0) * 0.25) as f32;
    assert_eq!(diffusion.processed_items, 1);
    assert_eq!(diffusion.requeued_cells, 0);
    assert_eq!(
        territory.side_influence(0).unwrap()[CENTER].to_bits(),
        expected.to_bits()
    );
    assert_eq!(
        territory.influence_runtime_state(),
        Some(InfluenceRuntimeState::default())
    );
}

#[test]
fn controller_hysteresis_leaves_stale_occupation_until_the_threshold_is_crossed() {
    let prior_occupation = f32::from_bits(0x3e99_999a);

    let mut below = maps();
    below.dominant_side[11] = 1;
    below.dominant_side[CENTER] = 0;
    below.occupation[CENTER] = prior_occupation;
    below.side_influence[0][CENTER] = 0.60;
    below.side_influence[1][CENTER] = 0.70;
    let mut below = TerritoryControl::new(config(below)).unwrap();
    below.restore_influence_runtime(queued_center()).unwrap();
    below.apply_influence_runtime(&[], &[], 1).unwrap();

    assert_eq!(below.dominant_side()[CENTER], 0);
    assert_eq!(
        below.occupation()[CENTER].to_bits(),
        prior_occupation.to_bits()
    );

    let mut above = maps();
    above.dominant_side[11] = 1;
    above.dominant_side[CENTER] = 0;
    above.occupation[CENTER] = prior_occupation;
    above.side_influence[0][CENTER] = 0.60;
    above.side_influence[1][CENTER] = 0.76;
    let mut above = TerritoryControl::new(config(above)).unwrap();
    above.restore_influence_runtime(queued_center()).unwrap();
    above.apply_influence_runtime(&[], &[], 1).unwrap();

    assert_eq!(above.dominant_side()[CENTER], 1);
    assert_eq!(above.occupation()[CENTER].to_bits(), (-0.76_f32).to_bits());
}

#[test]
fn source_mutations_queue_regular_and_priority_neighborhoods_in_browser_order() {
    let mut regular = maps();
    regular.occupation[CENTER] = 0.6;
    regular.side_influence[0][CENTER] = 0.6;
    regular.side_influence[1][CENTER] = 0.2;
    let mut regular = TerritoryControl::new(config(regular)).unwrap();
    regular.enable_influence_runtime();
    let (result, _) = regular
        .apply_influence_runtime(&[source(0.1)], &[], 0)
        .unwrap();
    assert_eq!(result.controller_change_count, 0);
    assert_eq!(result.credit_change_count, 0);
    assert_queue_neighborhood(&regular.influence_runtime_state().unwrap(), false);

    let mut credit = maps();
    credit.world_control[CENTER] = 2;
    credit.de_jure[CENTER] = 2;
    credit.primary_occupier[CENTER] = 0;
    credit.occupation[CENTER] = 0.6;
    credit.side_influence[0][CENTER] = 0.6;
    credit.side_influence[1][CENTER] = 0.2;
    let mut credit = TerritoryControl::new(config(credit)).unwrap();
    credit.enable_influence_runtime();
    let (result, _) = credit
        .apply_influence_runtime(&[source(0.1)], &[], 0)
        .unwrap();
    assert_eq!(result.controller_change_count, 0);
    assert_eq!(result.credit_change_count, 1);
    assert_queue_neighborhood(&credit.influence_runtime_state().unwrap(), true);

    let mut controller = maps();
    controller.world_control[CENTER] = 2;
    controller.de_jure[CENTER] = 2;
    controller.dominant_side[CENTER] = 1;
    controller.occupation[CENTER] = -0.4;
    controller.side_influence[0][CENTER] = 0.2;
    controller.side_influence[1][CENTER] = 0.4;
    let mut controller = TerritoryControl::new(config(controller)).unwrap();
    controller.enable_influence_runtime();
    let (result, _) = controller
        .apply_influence_runtime(&[source(0.3)], &[], 0)
        .unwrap();
    assert_eq!(result.controller_change_count, 1);
    assert_eq!(result.credit_change_count, 0);
    assert_queue_neighborhood(&controller.influence_runtime_state().unwrap(), true);
}

#[test]
fn checkpointed_queue_resumes_bit_exactly_across_repeated_diffusion_steps() {
    let mut fixture = maps();
    fixture.dominant_side[11] = 1;
    fixture.side_influence[0][CENTER] = 0.8;
    fixture.side_influence[1][CENTER] = 0.3;
    for cell in [11, 13, 7, 17] {
        fixture.side_influence[0][cell] = 0.4;
        fixture.side_influence[1][cell] = 0.7;
    }

    let mut uninterrupted = TerritoryControl::new(config(fixture)).unwrap();
    uninterrupted
        .restore_influence_runtime(InfluenceRuntimeState {
            regular_queue: vec![CENTER, CENTER],
            priority_queue: Vec::new(),
            queued_cells: vec![(CENTER, 1)],
        })
        .unwrap();
    uninterrupted
        .apply_influence_runtime(&[], &[0, 1], 2)
        .unwrap();

    let checkpoint_maps = uninterrupted.checkpoint_maps();
    let checkpoint_queue = uninterrupted.influence_runtime_state().unwrap();
    let mut restored = TerritoryControl::new(config(checkpoint_maps)).unwrap();
    restored
        .restore_influence_runtime(checkpoint_queue.clone())
        .unwrap();
    assert_eq!(restored.influence_runtime_state(), Some(checkpoint_queue));

    for _ in 0..4 {
        let left = uninterrupted
            .apply_influence_runtime(&[], &[0, 1], 2)
            .unwrap();
        let right = restored.apply_influence_runtime(&[], &[0, 1], 2).unwrap();
        assert_eq!(left.1, right.1);
        assert_eq!(
            uninterrupted.primary_occupier(),
            restored.primary_occupier()
        );
        assert_eq!(uninterrupted.dominant_side(), restored.dominant_side());
        assert_eq!(
            uninterrupted
                .occupation()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            restored
                .occupation()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
        for side in 0..2 {
            assert_eq!(
                uninterrupted
                    .side_influence(side)
                    .unwrap()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                restored
                    .side_influence(side)
                    .unwrap()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(
            uninterrupted.influence_runtime_state(),
            restored.influence_runtime_state()
        );
    }
}
