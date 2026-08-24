//! Command-line adapter for the production native runtime.
//!
//! The checkpoint is intentionally policy-complete: scenarios supply geography,
//! cities, and economy baselines, while the checkpoint explicitly supplies the
//! active coalitions, directed hostility, live units, and per-unit policies.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    hint::black_box,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use mw_core::{
    ARMOR_PAYROLL_PER_100, AirPowerState, BATTLEFIELD_SCHEMA_VERSION, BattlefieldBuff,
    BattlefieldConfig, BattlefieldRuntimeState, BattlefieldUnitState, BattlefieldUrbanCenter,
    BattlefieldWarPhase, CombatConfig, CombatEvent, CombatLayer, CombatUnit, CommandBand,
    CommandHomeTarget, ConflictResolutionPlan, CountryBattlefieldPrimitives,
    DEFAULT_GAMEPLAY_RNG_SEED, DecodedScenario, EconomyState, FrontLayoutPrior, FrontObjective,
    GAMEPLAY_RNG_ALGORITHM, GAMEPLAY_RNG_SCHEMA_VERSION, GameplayRngState, GridSpec,
    NATIVE_RUNTIME_SCHEMA_VERSION, NativeRuntime, NavalPlanningState, OccupationState,
    OperationalExecutionState, OperationalRuntimeState, PAYROLL_PER_UNIT, ProductionConfig,
    ReinforcementState, ResolvedCombatModifiers, ResolvedMovementModifiers, RuntimeCheckpoint,
    RuntimeConfig, RuntimeDiplomacy, RuntimeSnapshot, RuntimeState, RuntimeUnitPolicy,
    STARTING_RESERVE_CYCLES, ScenarioProduction, Simulation, SimulationConfig, SimulationUnit,
    StrategicMissileState, StrategicSimulation, TARGET_STARTING_PAYROLL_SHARE, TerritoryCity,
    TerritoryCommittedState, TerritoryConfig, TerritoryControl, TerritoryMaps, UnitAiPolicy,
    UnitCommandPolicy, UnitInfluencePolicy, UnitKind, WorldGridView, browser_discipline,
    command_refusal_share, decode_mwsc_gzip, derive_scenario_production,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NATIVE_RUNTIME_CHECKPOINT_SCHEMA: &str = "native-runtime-checkpoint-v1";
pub const NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA: &str = "native-runtime-checkpoint-v2";
pub const NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA: &str = "native-runtime-checkpoint-v3";
pub const NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA: &str = "native-runtime-checkpoint-v4";
pub const NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA: &str = "native-runtime-checkpoint-v5";
pub const NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA: &str = "native-runtime-checkpoint-v6";
pub const NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA: &str = "native-runtime-checkpoint-v7";
pub const NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA: &str = "native-runtime-checkpoint-v8";
pub const NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA: &str = "native-runtime-checkpoint-v9";
pub const NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA: &str = "native-runtime-checkpoint-v10";
pub const NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA: &str = "native-runtime-checkpoint-v11";
pub const NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA: &str = "native-runtime-checkpoint-v12";
pub const NATIVE_SIDE_DYNAMICS_SCHEMA: &str = "native-side-dynamics-v1";
pub const NATIVE_INFLUENCE_RUNTIME_SCHEMA: &str = "native-influence-runtime-v1";

const INFLUENCE_REGULAR_QUEUE_LIMIT: usize = 16_384;
const INFLUENCE_PRIORITY_QUEUE_LIMIT: usize = 8_192;

#[derive(Clone, Debug, Serialize)]
pub struct NativeCheckpointWriteReport {
    pub path: String,
    pub bytes: usize,
    pub schema: &'static str,
}

/// Serialize a quiescent runtime using the strict versioned object consumed by the loader.
/// Core owns the barrier and snapshot extraction; this adapter owns JSON and filesystem policy.
pub fn write_runtime_checkpoint_v2(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    runtime: &mut NativeRuntime,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let state = runtime
        .checkpoint_state()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    write_runtime_checkpoint_state_v2_with_hash(&raw, baseline, &state, output, steps)
}

pub fn write_runtime_checkpoint_state_v2(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    write_runtime_checkpoint_state_v2_with_hash(&raw, baseline, state, output, steps)
}

/// Serialize a quiescent runtime with its history-dependent influence scheduler state.
pub fn write_runtime_checkpoint_v3(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    runtime: &mut NativeRuntime,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let state = runtime
        .checkpoint_state()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    write_runtime_checkpoint_state_v3_with_hash(&raw, baseline, &state, output, steps)
}

/// Explicitly named alias used by native frontends when selecting the v3 save contract.
pub fn write_native_runtime_checkpoint_v3(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    runtime: &mut NativeRuntime,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_v3(scenario_path, baseline, runtime, output, steps)
}

pub fn write_runtime_checkpoint_state_v3(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    write_runtime_checkpoint_state_v3_with_hash(&raw, baseline, state, output, steps)
}

/// Serialize a v4 checkpoint. The core runtime supplies the already validated
/// side-dynamics snapshot; this adapter validates its wire representation.
pub fn write_runtime_checkpoint_state_v4(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    state
        .battlefield
        .as_ref()
        .context("checkpoint-v4 writer requires live battlefield state")?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let influence_runtime = state
        .influence_runtime
        .as_ref()
        .context("checkpoint-v4 writer requires influence runtime state")?;
    let cell_count = state
        .territory_config
        .width
        .checked_mul(state.territory_config.height)
        .context("checkpoint-v4 territory cell count overflows")?;
    let influence_runtime = influence_runtime_fixture(influence_runtime, cell_count)?;
    let dynamics = state
        .side_dynamics
        .as_ref()
        .context("checkpoint-v4 writer requires side dynamics state")?;
    let side_dynamics = side_dynamics_fixture(dynamics)?;
    validate_side_dynamics(
        &side_dynamics,
        state.territory_config.max_sides,
        state.frame,
        u64::try_from(cell_count).context("checkpoint-v4 territory cell count exceeds u64")?,
    )?;
    write_runtime_checkpoint_state_with_hash(
        &raw,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
        Some(influence_runtime),
        Some(side_dynamics),
        None,
        None,
        None,
        None,
    )
}

/// Serialize a v5 checkpoint with the complete operational-AI continuation state.
pub fn write_runtime_checkpoint_state_v5(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    state
        .battlefield
        .as_ref()
        .context("checkpoint-v5 writer requires live battlefield state")?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let influence_runtime = state
        .influence_runtime
        .as_ref()
        .context("checkpoint-v5 writer requires influence runtime state")?;
    let cell_count = state
        .territory_config
        .width
        .checked_mul(state.territory_config.height)
        .context("checkpoint-v5 territory cell count overflows")?;
    let influence_runtime = influence_runtime_fixture(influence_runtime, cell_count)?;
    let dynamics = state
        .side_dynamics
        .as_ref()
        .context("checkpoint-v5 writer requires side dynamics state")?;
    let side_dynamics = side_dynamics_fixture(dynamics)?;
    validate_side_dynamics(
        &side_dynamics,
        state.territory_config.max_sides,
        state.frame,
        u64::try_from(cell_count).context("checkpoint-v5 territory cell count exceeds u64")?,
    )?;
    let operational_ai = state
        .operations
        .as_ref()
        .context("checkpoint-v5 writer requires operational AI state")?;
    let live_units = state
        .units
        .iter()
        .map(|unit| {
            usize::try_from(unit.combat.side)
                .context("unit side exceeds the checkpoint platform")
                .map(|side| (unit.combat.id, side))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let countries = state
        .scenario
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    validate_operational_ai(
        operational_ai,
        state.territory_config.max_sides,
        &live_units,
        &countries,
        state.tick,
        &state.diplomacy.hostility,
    )?;
    write_runtime_checkpoint_state_with_hash(
        &raw,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
        Some(influence_runtime),
        Some(side_dynamics),
        Some(operational_ai.clone()),
        None,
        None,
        None,
    )
}

/// Serialize a v6 checkpoint with persistent naval/defender execution and air-power state.
pub fn write_runtime_checkpoint_state_v6(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    state
        .battlefield
        .as_ref()
        .context("checkpoint-v6 writer requires live battlefield state")?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let influence_runtime = state
        .influence_runtime
        .as_ref()
        .context("checkpoint-v6 writer requires influence runtime state")?;
    let cell_count = state
        .territory_config
        .width
        .checked_mul(state.territory_config.height)
        .context("checkpoint-v6 territory cell count overflows")?;
    let influence_runtime = influence_runtime_fixture(influence_runtime, cell_count)?;
    let dynamics = state
        .side_dynamics
        .as_ref()
        .context("checkpoint-v6 writer requires side dynamics state")?;
    let side_dynamics = side_dynamics_fixture(dynamics)?;
    validate_side_dynamics(
        &side_dynamics,
        state.territory_config.max_sides,
        state.frame,
        u64::try_from(cell_count).context("checkpoint-v6 territory cell count exceeds u64")?,
    )?;
    let operational_ai = state
        .operations
        .as_ref()
        .context("checkpoint-v6 writer requires operational AI state")?;
    let live_units = state
        .units
        .iter()
        .map(|unit| {
            usize::try_from(unit.combat.side)
                .context("unit side exceeds the checkpoint platform")
                .map(|side| (unit.combat.id, side))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let countries = state
        .scenario
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    validate_operational_ai(
        operational_ai,
        state.territory_config.max_sides,
        &live_units,
        &countries,
        state.tick,
        &state.diplomacy.hostility,
    )?;
    let operational_execution = state
        .operational_execution
        .as_ref()
        .context("checkpoint-v6 writer requires operational execution state")?;
    operational_execution
        .validate_shape(state.tick, state.territory_config.max_sides)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let air_power = state
        .air_power
        .as_ref()
        .context("checkpoint-v6 writer requires air power state")?;
    air_power
        .validate()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    write_runtime_checkpoint_state_with_hash(
        &raw,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
        Some(influence_runtime),
        Some(side_dynamics),
        Some(operational_ai.clone()),
        Some(operational_execution.clone()),
        Some(air_power.clone()),
        None,
    )
}

/// Serialize a v7 checkpoint with the complete naval-planning continuation state.
pub fn write_runtime_checkpoint_state_v7(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA,
    )
}

/// Serialize a v8 checkpoint with persistent operational-feedback history.
pub fn write_runtime_checkpoint_state_v8(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
    )
}

/// Serialize a v9 checkpoint with replay-safe gameplay RNG and personnel reserves.
pub fn write_runtime_checkpoint_state_v9(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA,
    )
}

/// Serialize a v10 checkpoint with persistent reinforcement and logistics state.
pub fn write_runtime_checkpoint_state_v10(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA,
    )
}

/// Serialize a v11 checkpoint with exact material-logistics continuation.
pub fn write_runtime_checkpoint_state_v11(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA,
    )
}

/// Serialize a v12 checkpoint with strategic missile continuation state.
pub fn write_runtime_checkpoint_state_v12(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_v7_through_v12(
        scenario_path,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA,
    )
}

fn write_runtime_checkpoint_state_v7_through_v12(
    scenario_path: &Path,
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
    schema: &'static str,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    state
        .battlefield
        .as_ref()
        .context("checkpoint-v7 writer requires live battlefield state")?;
    let raw = fs::read(scenario_path)
        .with_context(|| format!("failed to read {}", scenario_path.display()))?;
    let influence_runtime = state
        .influence_runtime
        .as_ref()
        .context("checkpoint-v7 writer requires influence runtime state")?;
    let cell_count = state
        .territory_config
        .width
        .checked_mul(state.territory_config.height)
        .context("checkpoint-v7 territory cell count overflows")?;
    let influence_runtime = influence_runtime_fixture(influence_runtime, cell_count)?;
    let dynamics = state
        .side_dynamics
        .as_ref()
        .context("checkpoint-v7 writer requires side dynamics state")?;
    let side_dynamics = side_dynamics_fixture(dynamics)?;
    validate_side_dynamics(
        &side_dynamics,
        state.territory_config.max_sides,
        state.frame,
        u64::try_from(cell_count).context("checkpoint-v7 territory cell count exceeds u64")?,
    )?;
    let operational_ai = state
        .operations
        .as_ref()
        .context("checkpoint-v7 writer requires operational AI state")?;
    let live_units = state
        .units
        .iter()
        .map(|unit| {
            usize::try_from(unit.combat.side)
                .context("unit side exceeds the checkpoint platform")
                .map(|side| (unit.combat.id, side))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let countries = state
        .scenario
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    validate_operational_ai(
        operational_ai,
        state.territory_config.max_sides,
        &live_units,
        &countries,
        state.tick,
        &state.diplomacy.hostility,
    )?;
    let operational_execution = state
        .operational_execution
        .as_ref()
        .context("checkpoint-v7 writer requires operational execution state")?;
    operational_execution
        .validate_shape(state.tick, state.territory_config.max_sides)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let air_power = state
        .air_power
        .as_ref()
        .context("checkpoint-v7 writer requires air power state")?;
    air_power
        .validate()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let naval_planning = state
        .naval_planning
        .as_ref()
        .context("checkpoint-v7 writer requires naval planning state")?;
    naval_planning
        .validate_with_execution(state.territory_config.max_sides, operational_execution)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    write_runtime_checkpoint_state_with_hash(
        &raw,
        baseline,
        state,
        output,
        steps,
        schema,
        Some(influence_runtime),
        Some(side_dynamics),
        Some(operational_ai.clone()),
        Some(operational_execution.clone()),
        Some(air_power.clone()),
        Some(naval_planning.clone()),
    )
}

fn side_dynamics_fixture(
    dynamics: &BTreeMap<usize, mw_core::SideDynamics>,
) -> Result<SideDynamicsFixture> {
    let mut sides = Vec::with_capacity(dynamics.len());
    for (&index, side) in dynamics {
        if side.side_index != index {
            bail!("side dynamics map key does not match its embedded side index");
        }
        sides.push(SideDynamicsSideFixture {
            side_index: u16::try_from(index).context("side dynamics index exceeds u16")?,
            initial_personnel: side.initial_personnel,
            personnel: side.current_personnel,
            momentum_history: side
                .momentum_samples
                .iter()
                .map(|sample| SideDynamicsSampleFixture {
                    frame: sample.frame,
                    controlled: sample.controlled,
                })
                .collect(),
            war_phase: match side.phase {
                mw_core::WarPhase::Advancing => SideDynamicsWarPhaseFixture::Advancing,
                mw_core::WarPhase::Stalemate => SideDynamicsWarPhaseFixture::Stalemate,
                mw_core::WarPhase::Retreating => SideDynamicsWarPhaseFixture::Retreating,
                mw_core::WarPhase::Collapsing => SideDynamicsWarPhaseFixture::Collapsing,
            },
            posture: match side.posture {
                mw_core::WarPosture::Offensive => SideDynamicsPostureFixture::Offensive,
                mw_core::WarPosture::Balanced => SideDynamicsPostureFixture::Balanced,
                mw_core::WarPosture::Defensive => SideDynamicsPostureFixture::Defensive,
            },
            posture_override: side.posture_override.map(|posture| match posture {
                mw_core::WarPosture::Offensive => SideDynamicsPostureFixture::Offensive,
                mw_core::WarPosture::Balanced => SideDynamicsPostureFixture::Balanced,
                mw_core::WarPosture::Defensive => SideDynamicsPostureFixture::Defensive,
            }),
        });
    }
    Ok(SideDynamicsFixture {
        schema: NATIVE_SIDE_DYNAMICS_SCHEMA.to_owned(),
        sides,
    })
}

fn write_runtime_checkpoint_state_v2_with_hash(
    raw: &[u8],
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    write_runtime_checkpoint_state_with_hash(
        raw,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn write_runtime_checkpoint_state_v3_with_hash(
    raw: &[u8],
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
) -> Result<NativeCheckpointWriteReport> {
    let influence_runtime = state
        .influence_runtime
        .as_ref()
        .context("checkpoint-v3 writer requires influence runtime state")?;
    let cell_count = state
        .territory_config
        .width
        .checked_mul(state.territory_config.height)
        .context("checkpoint-v3 territory cell count overflows")?;
    let influence_runtime = influence_runtime_fixture(influence_runtime, cell_count)?;
    write_runtime_checkpoint_state_with_hash(
        raw,
        baseline,
        state,
        output,
        steps,
        NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
        Some(influence_runtime),
        None,
        None,
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_runtime_checkpoint_state_with_hash(
    raw: &[u8],
    baseline: &DecodedScenario,
    state: &mw_core::NativeRuntimeCheckpointState,
    output: &Path,
    steps: usize,
    schema: &'static str,
    influence_runtime: Option<InfluenceRuntimeFixture>,
    side_dynamics: Option<SideDynamicsFixture>,
    operational_ai: Option<OperationalRuntimeState>,
    operational_execution: Option<OperationalExecutionState>,
    air_power: Option<AirPowerState>,
    naval_planning: Option<NavalPlanningState>,
) -> Result<NativeCheckpointWriteReport> {
    validate_checkpoint_write_steps(steps)?;
    if state.runtime_config != RuntimeConfig::default() {
        bail!("checkpoint-v2 writer only supports the canonical runtime configuration");
    }
    if state.simulation_config != SimulationConfig::default() {
        bail!("checkpoint-v2 writer only supports the canonical simulation configuration");
    }
    if baseline.target.width != state.territory_config.width
        || baseline.target.height != state.territory_config.height
        || baseline.target.grid_res != state.territory_config.grid_resolution
        || state.scenario.grid != baseline.target
    {
        bail!("checkpoint baseline and live runtime grid do not match");
    }
    let expected_production = derive_scenario_production(baseline, &ProductionConfig::default())?;
    if state.scenario != expected_production {
        bail!("checkpoint runtime production baseline does not match the supplied scenario");
    }
    if state.territory_config.tile_size != TERRITORY_TILE_SIZE {
        bail!("checkpoint-v2 writer only supports the canonical territory tile size");
    }
    let declared_sides = state
        .territory_config
        .country_to_side
        .values()
        .copied()
        .collect::<BTreeSet<_>>();
    if state.territory_config.max_sides == 0
        || declared_sides.len() != state.territory_config.max_sides
        || !declared_sides
            .iter()
            .copied()
            .eq(0..state.territory_config.max_sides)
    {
        bail!("checkpoint-v2 writer requires contiguous, nonempty territory sides");
    }
    if state.strategic_cycle == u64::MAX {
        bail!("checkpoint-v2 strategic cycle must leave room for a later cycle");
    }
    let economy_ids = state
        .economies
        .iter()
        .map(|economy| economy.country_id)
        .collect::<BTreeSet<_>>();
    if economy_ids.len() != state.economies.len()
        || economy_ids
            != state
                .territory_config
                .country_to_side
                .keys()
                .copied()
                .collect()
    {
        bail!("checkpoint-v2 economies must exactly cover declared countries");
    }
    let expected_active_sides = state
        .economies
        .iter()
        .filter(|economy| !economy.capitulated)
        .map(|economy| {
            state
                .territory_config
                .country_to_side
                .get(&economy.country_id)
                .copied()
                .with_context(|| {
                    format!(
                        "economy {} is absent from checkpoint side topology",
                        economy.country_id
                    )
                })
                .and_then(|side| u16::try_from(side).context("active side index exceeds u16"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let actual_active_sides = state
        .diplomacy
        .active_sides
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if actual_active_sides.len() != state.diplomacy.active_sides.len()
        || actual_active_sides != expected_active_sides
    {
        bail!("checkpoint-v2 active sides must exactly match non-capitulated economies");
    }
    if state.territory_config.hostility_matrix != state.diplomacy.hostility {
        bail!("checkpoint diplomacy and territory hostility matrices disagree");
    }
    let expected_protected = state
        .territory_config
        .country_to_side
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    if state.territory_config.protected_owner_ids != expected_protected {
        bail!("checkpoint-v2 writer requires coalition countries as protected owners");
    }
    let expected_cities = state
        .scenario
        .cities
        .iter()
        .map(|city| TerritoryCity {
            id: city.city_id,
            cell: city.cell,
            owner: city.owner_id,
            population: city.population,
            capital: city.capital,
        })
        .collect::<Vec<_>>();
    if state.territory_config.cities != expected_cities {
        bail!("checkpoint-v2 writer requires scenario-derived territory cities");
    }
    let name = scenario_name(baseline);
    let mut sides: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
    let mut coalitions: BTreeMap<usize, BTreeSet<u16>> = BTreeMap::new();
    for (&country, &side) in state.territory_config.country_to_side.iter() {
        sides
            .entry(u16::try_from(side).context("side index exceeds u16")?)
            .or_default()
            .push(country);
        coalitions.entry(side).or_default().insert(country);
    }
    let sides: Vec<Value> = sides
        .into_iter()
        .map(|(side_index, mut country_ids)| {
            country_ids.sort_unstable();
            json!({"sideIndex": side_index, "countryIds": country_ids})
        })
        .collect();
    let planner = planner_fixture(state)?;
    let unit_ids = state
        .units
        .iter()
        .map(|unit| unit.combat.id)
        .collect::<BTreeSet<_>>();
    let battlefield = state
        .battlefield
        .as_ref()
        .map(|battlefield| {
            let world = WorldGridView::new(
                state.territory_config.grid_resolution,
                state.territory_config.width,
                state.territory_config.height,
                &state.territory_config.maps.land,
            )?;
            let live_unit_ids = state
                .units
                .iter()
                .map(|unit| unit.combat.id)
                .collect::<Vec<_>>();
            battlefield.validate(
                world,
                state.territory_config.max_sides,
                &state.territory_config.country_to_side,
                &live_unit_ids,
            )?;
            battlefield_fixture(battlefield, state.tick)
        })
        .transpose()?;
    validate_planner_fixture(
        &planner,
        state.tick,
        state.territory_config.maps.side_influence.len(),
        &unit_ids,
    )?;
    let policies = state
        .unit_policies
        .iter()
        .map(|policy| (policy.unit_id, policy))
        .collect::<BTreeMap<_, _>>();
    if policies.len() != state.unit_policies.len() || policies.len() != state.units.len() {
        bail!("runtime checkpoint unit policy coverage is invalid");
    }
    let units = state
        .units
        .iter()
        .map(|unit| {
            let policy = policies.get(&unit.combat.id).with_context(|| {
                format!("unit {} is missing its runtime policy", unit.combat.id)
            })?;
            let side = usize::try_from(unit.combat.side)
                .context("unit side exceeds the checkpoint platform")?;
            let coalition = coalitions.get(&side).with_context(|| {
                format!("unit {} references unknown side {side}", unit.combat.id)
            })?;
            if policy
                .influence
                .as_ref()
                .is_some_and(|influence| &influence.owner_ally_country_ids != coalition)
            {
                bail!(
                    "unit {} has owner-allies which checkpoint-v2 cannot derive from its coalition",
                    unit.combat.id
                );
            }
            Ok(unit_json(unit, policy))
        })
        .collect::<Result<Vec<_>>>()?;
    let maps = &state.territory_config.maps;
    let mut body = json!({
        "schema": schema,
        "checkpointBoundary": "midWar",
        "scenario": {"sha256": sha256_hex(raw), "name": name, "gridRes": state.territory_config.grid_resolution},
        "geography": {"landRuns": rle(&baseline.land), "worldControlRuns": rle(&baseline.world_control), "deJureRuns": rle(&baseline.de_jure)},
        "sides": sides,
        "activeSides": state.diplomacy.active_sides,
        "hostilityMatrix": state.diplomacy.hostility,
        "tick": state.tick, "frame": state.frame, "warGraceEnd": state.war_grace_end,
        "strategicCycle": state.strategic_cycle, "steps": steps,
        "planner": planner,
        "units": units,
        "economies": state.economies,
        "occupations": state.occupations,
        "casualties": covered_casualties(&state.casualties, &state.territory_config.country_to_side),
        "casualtiesByVictim": covered_nested_casualties(&state.casualties_by_victim, &state.territory_config.country_to_side),
        "territory": {"encoding": "rle-bits-v1", "maps": {
            "landRuns": rle(&maps.land), "worldControlRuns": rle(&maps.world_control), "deJureRuns": rle(&maps.de_jure),
            "primaryOccupierRuns": rle(&maps.primary_occupier), "dominantSideRuns": rle(&maps.dominant_side),
            "occupationBitsRuns": rle(&maps.occupation.iter().map(|v| v.to_bits()).collect::<Vec<_>>()),
            "sideInfluenceBitsRuns": maps.side_influence.iter().map(|row| rle(&row.iter().map(|v| v.to_bits()).collect::<Vec<_>>())).collect::<Vec<_>>()
        }, "revisions": {"topologyRevision": state.territory_config.topology_revision, "worldRevision": state.territory_config.world_revision, "cityRevision": state.territory_config.city_revision},
        "committedCensus": state.territory_committed_state}
    });
    if let Some(battlefield) = battlefield {
        let mut battlefield = serde_json::to_value(battlefield)?;
        if !matches!(
            schema,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        ) && let Some(units) = battlefield.get_mut("units").and_then(Value::as_array_mut)
        {
            for unit in units {
                if let Some(unit) = unit.as_object_mut() {
                    unit.remove("supplyCollapsedTick");
                }
            }
        }
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert("battlefield".to_owned(), battlefield);
    }
    if let Some(influence_runtime) = influence_runtime {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "influenceRuntime".to_owned(),
                serde_json::to_value(influence_runtime)?,
            );
    }
    if let Some(side_dynamics) = side_dynamics {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "sideDynamics".to_owned(),
                serde_json::to_value(side_dynamics)?,
            );
    }
    if let Some(operational_ai) = operational_ai {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "operationalAi".to_owned(),
                serde_json::to_value(operational_ai)?,
            );
    }
    if let Some(operational_execution) = operational_execution {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "operationalExecution".to_owned(),
                serde_json::to_value(operational_execution)?,
            );
    }
    if let Some(air_power) = air_power {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert("airPower".to_owned(), serde_json::to_value(air_power)?);
    }
    if let Some(naval_planning) = naval_planning {
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "navalPlanning".to_owned(),
                serde_json::to_value(naval_planning)?,
            );
    }
    if matches!(
        schema,
        NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) {
        if state.personnel_reserves.len() != state.territory_config.max_sides {
            bail!("personnel reserves must exactly cover every stable side");
        }
        let reserves = (0..state.territory_config.max_sides)
            .map(|side| {
                state
                    .personnel_reserves
                    .get(&side)
                    .copied()
                    .with_context(|| format!("personnel reserve is missing side {side}"))
            })
            .collect::<Result<Vec<_>>>()?;
        if reserves
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            bail!("personnel reserves must be finite and non-negative");
        }
        let object = body.as_object_mut().expect("checkpoint body is an object");
        object.insert(
            "gameplayRng".to_owned(),
            json!({
                "schema": GAMEPLAY_RNG_SCHEMA_VERSION,
                "algorithm": GAMEPLAY_RNG_ALGORITHM,
                "state": state.gameplay_rng.state,
            }),
        );
        object.insert("personnelReserves".to_owned(), json!(reserves));
    }
    if matches!(
        schema,
        NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) {
        let reinforcement = state
            .reinforcement
            .as_ref()
            .context("checkpoint-v10+ writer requires reinforcement state")?;
        let air_power = state
            .air_power
            .as_ref()
            .context("checkpoint-v10+ writer requires air power state")?;
        reinforcement
            .validate(
                air_power,
                &state.territory_config.country_to_side,
                state.territory_config.max_sides,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert(
                "reinforcement".to_owned(),
                serde_json::to_value(reinforcement)?,
            );
    }
    if matches!(
        schema,
        NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) {
        let material = state
            .material_logistics
            .as_ref()
            .context("checkpoint-v11 writer requires material logistics state")?;
        material
            .validate(
                &state.units,
                &state.territory_config.country_to_side,
                state.territory_config.max_sides,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        body.as_object_mut()
            .expect("checkpoint body is an object")
            .insert("materialLogistics".into(), serde_json::to_value(material)?);
        if schema == NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA {
            let missiles = state
                .strategic_missiles
                .as_ref()
                .context("checkpoint-v12 writer requires strategic missile state")?;
            missiles
                .validate(state.territory_config.max_sides)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            body.as_object_mut()
                .expect("checkpoint body is an object")
                .insert("strategicMissiles".into(), serde_json::to_value(missiles)?);
        }
    }
    if matches!(
        schema,
        NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) {
        validate_checkpoint_v5_required_nullable_fields(&body)
            .context("checkpoint writer omitted required nullable operational fields")?;
        validate_checkpoint_v8_supply_collapse_fields(&body)
            .context("checkpoint writer emitted invalid supply-collapse history fields")?;
        let fixture: RuntimeCheckpointFixture = serde_json::from_value(body.clone())
            .context("checkpoint writer produced an invalid wire object")?;
        validate_checkpoint_shape(&fixture)
            .context("checkpoint writer produced an invalid checkpoint shape")?;
    }
    let bytes = serde_json::to_vec(&body)?;
    let parent = checkpoint_output_parent(output);
    if !parent.exists() {
        bail!("output parent does not exist: {}", parent.display());
    }
    let (tmp, mut f) = create_checkpoint_temp(output)?;
    let write_result = (|| -> Result<()> {
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    drop(f);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("failed to write {}", output.display()));
    }
    let result =
        fs::rename(&tmp, output).with_context(|| format!("failed to install {}", output.display()));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync output directory {}", parent.display()))?;
    Ok(NativeCheckpointWriteReport {
        path: output.display().to_string(),
        bytes: bytes.len(),
        schema,
    })
}

fn deserialize_optional_operational_ai<'de, D>(
    deserializer: D,
) -> Result<Option<OperationalRuntimeState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<OperationalRuntimeState>::deserialize(deserializer)?;
    if value.is_none() {
        return Err(serde::de::Error::custom(
            "operationalAi must be an object, not null",
        ));
    }
    Ok(value)
}

fn deserialize_optional_operational_execution<'de, D>(
    deserializer: D,
) -> Result<Option<OperationalExecutionState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<OperationalExecutionState>::deserialize(deserializer)?;
    if value.is_none() {
        return Err(serde::de::Error::custom(
            "operationalExecution must be an object, not null",
        ));
    }
    Ok(value)
}

fn deserialize_optional_air_power<'de, D>(
    deserializer: D,
) -> Result<Option<AirPowerState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<AirPowerState>::deserialize(deserializer)?;
    if value.is_none() {
        return Err(serde::de::Error::custom(
            "airPower must be an object, not null",
        ));
    }
    Ok(value)
}

fn deserialize_optional_naval_planning<'de, D>(
    deserializer: D,
) -> Result<Option<NavalPlanningState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<NavalPlanningState>::deserialize(deserializer)?;
    if value.is_none() {
        return Err(serde::de::Error::custom(
            "navalPlanning must be an object, not null",
        ));
    }
    Ok(value)
}

fn validate_operational_ai(
    operations: &OperationalRuntimeState,
    side_count: usize,
    live_units: &BTreeMap<u64, usize>,
    countries: &BTreeSet<u16>,
    tick: u64,
    hostility: &[u8],
) -> Result<()> {
    operations
        .validate(side_count, live_units, countries, tick)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if hostility.len() != side_count.saturating_mul(side_count) {
        bail!("operationalAi cannot validate against a malformed hostilityMatrix");
    }
    for side in &operations.sides {
        let expected = (0..side_count)
            .filter(|other| hostility[side.side_index * side_count + *other] == 1)
            .collect::<Vec<_>>();
        if side.hostile_side_indices != expected {
            bail!("operationalAi hostile side coverage disagrees with hostilityMatrix");
        }
    }
    Ok(())
}

fn require_object_key<'a>(object: &'a Value, key: &str, path: &str) -> Result<&'a Value> {
    object
        .as_object()
        .with_context(|| format!("{path} must be an object"))?
        .get(key)
        .with_context(|| format!("{path}.{key} is required (use null when unset)"))
}

fn validate_checkpoint_v5_required_nullable_fields(checkpoint: &Value) -> Result<()> {
    let operational = require_object_key(checkpoint, "operationalAi", "checkpoint")?;
    let sides = require_object_key(operational, "sides", "checkpoint.operationalAi")?
        .as_array()
        .context("checkpoint.operationalAi.sides must be an array")?;
    for (index, side) in sides.iter().enumerate() {
        let path = format!("checkpoint.operationalAi.sides[{index}]");
        let override_value = require_object_key(side, "override", &path)?;
        if !override_value.is_null() {
            require_object_key(override_value, "expiresTick", &format!("{path}.override"))?;
        }
        let intel = require_object_key(side, "intel", &path)?;
        let contacts = require_object_key(intel, "contacts", &format!("{path}.intel"))?
            .as_array()
            .with_context(|| format!("{path}.intel.contacts must be an array"))?;
        for (contact_index, contact) in contacts.iter().enumerate() {
            require_object_key(
                contact,
                "countryId",
                &format!("{path}.intel.contacts[{contact_index}]"),
            )?;
        }
    }

    let task_forces = require_object_key(operational, "taskForces", "checkpoint.operationalAi")?
        .as_array()
        .context("checkpoint.operationalAi.taskForces must be an array")?;
    for (index, task_force) in task_forces.iter().enumerate() {
        let path = format!("checkpoint.operationalAi.taskForces[{index}]");
        for key in [
            "theaterId",
            "target",
            "stagingAnchor",
            "withdrawalAnchor",
            "completionReason",
            "outcome",
            "parentTaskForceId",
            "supplyInvalidatedTick",
        ] {
            require_object_key(task_force, key, &path)?;
        }
    }

    let desperation = require_object_key(
        operational,
        "countryDesperation",
        "checkpoint.operationalAi",
    )?
    .as_array()
    .context("checkpoint.operationalAi.countryDesperation must be an array")?;
    for (index, country) in desperation.iter().enumerate() {
        let path = format!("checkpoint.operationalAi.countryDesperation[{index}]");
        for key in ["initialCities", "initialManpower", "previousControlled"] {
            require_object_key(country, key, &path)?;
        }
    }

    let events = require_object_key(operational, "overrideEvents", "checkpoint.operationalAi")?
        .as_array()
        .context("checkpoint.operationalAi.overrideEvents must be an array")?;
    for (index, event) in events.iter().enumerate() {
        let path = format!("checkpoint.operationalAi.overrideEvents[{index}]");
        require_object_key(event, "posture", &path)?;
        require_object_key(event, "expiresTick", &path)?;
    }
    Ok(())
}

fn validate_checkpoint_v8_supply_collapse_fields(checkpoint: &Value) -> Result<()> {
    let schema = checkpoint
        .get("schema")
        .and_then(Value::as_str)
        .context("checkpoint.schema must be a string")?;
    let Some(units) = checkpoint
        .get("battlefield")
        .and_then(|battlefield| battlefield.get("units"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for (index, unit) in units.iter().enumerate() {
        let object = unit
            .as_object()
            .with_context(|| format!("checkpoint.battlefield.units[{index}] must be an object"))?;
        let present = object.contains_key("supplyCollapsedTick");
        if matches!(
            schema,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        ) && !present
        {
            bail!(
                "checkpoint.battlefield.units[{index}].supplyCollapsedTick is required (use null when unset)"
            );
        }
        if !matches!(
            schema,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        ) && present
        {
            bail!("{schema} cannot contain checkpoint-v8 battlefield supplyCollapse history");
        }
    }
    Ok(())
}

fn validate_checkpoint_write_steps(steps: usize) -> Result<()> {
    if steps == 0 {
        bail!("checkpoint save requires at least one continuation step");
    }
    Ok(())
}

static CHECKPOINT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_checkpoint_temp(output: &Path) -> Result<(PathBuf, fs::File)> {
    let parent = checkpoint_output_parent(output);
    let output_name = output
        .file_name()
        .context("checkpoint output path must name a file")?;
    for _ in 0..1_024 {
        let sequence = CHECKPOINT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(output_name);
        name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let path = parent.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create temporary checkpoint beside {}",
                        output.display()
                    )
                });
            }
        }
    }
    bail!(
        "could not allocate a unique temporary checkpoint beside {}",
        output.display()
    )
}

fn checkpoint_output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn planner_fixture(state: &mw_core::NativeRuntimeCheckpointState) -> Result<PlannerFixture> {
    let front_prior_by_unit = state
        .front_prior_by_unit
        .iter()
        .map(|(&unit_id, prior)| {
            if unit_id != prior.unit_id {
                bail!(
                    "front planner map key {unit_id} disagrees with embedded unit {}",
                    prior.unit_id
                );
            }
            Ok(FrontPriorFixture::from(prior))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PlannerFixture {
        objectives: state
            .objectives
            .iter()
            .map(PlannerObjectiveFixture::from)
            .collect(),
        prior_objective_by_unit: state.prior_objective_by_unit.clone(),
        front_prior_by_unit,
        last_front_refresh_tick: state.last_front_refresh_tick,
    })
}

fn validate_planner_fixture(
    planner: &PlannerFixture,
    tick: u64,
    side_count: usize,
    unit_ids: &BTreeSet<u64>,
) -> Result<()> {
    if planner
        .last_front_refresh_tick
        .is_some_and(|refresh_tick| refresh_tick > tick)
    {
        bail!("planner.lastFrontRefreshTick cannot be newer than the checkpoint tick");
    }
    if planner.last_front_refresh_tick.is_none() && !planner.front_prior_by_unit.is_empty() {
        bail!("planner front priors require a completed front refresh tick");
    }

    let mut objective_ids = BTreeSet::new();
    for objective in &planner.objectives {
        if !objective_ids.insert(objective.id) {
            bail!("planner objective ids must be unique");
        }
        if objective.side_pair[0] == objective.side_pair[1]
            || objective
                .side_pair
                .iter()
                .any(|side| usize::from(*side) >= side_count)
        {
            bail!("planner objectives must reference two distinct declared sides");
        }
        FrontObjective::new(
            objective.id,
            objective.side_pair,
            objective.segment_id,
            objective.lat,
            objective.lng,
            objective.capacity,
            objective.priority,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }

    for (&unit_id, &objective_id) in &planner.prior_objective_by_unit {
        if !unit_ids.contains(&unit_id) {
            bail!("planner prior assignment references unknown unit {unit_id}");
        }
        if !objective_ids.contains(&objective_id) {
            bail!("planner prior assignment references unknown objective {objective_id}");
        }
    }

    let mut prior_unit_ids = BTreeSet::new();
    for prior in &planner.front_prior_by_unit {
        if !prior_unit_ids.insert(prior.unit_id) {
            bail!("planner front priors must contain unique unit ids");
        }
        if prior.pair_key.is_empty() {
            bail!("planner front prior pairKey must not be empty");
        }
        if !unit_ids.contains(&prior.unit_id) {
            bail!(
                "planner front prior references unknown unit {}",
                prior.unit_id
            );
        }
        if !objective_ids.contains(&prior.objective_id) {
            bail!(
                "planner front prior references unknown objective {}",
                prior.objective_id
            );
        }
    }
    Ok(())
}

fn rle<T: Copy + PartialEq + serde::Serialize>(values: &[T]) -> Vec<(u64, T)> {
    let mut out = Vec::new();
    for &value in values {
        if let Some((n, prior)) = out.last_mut()
            && *prior == value
        {
            *n += 1;
            continue;
        }
        out.push((1, value));
    }
    out
}

fn battlefield_fixture(
    battlefield: &BattlefieldRuntimeState,
    checkpoint_tick: u64,
) -> Result<BattlefieldFixture> {
    if let Some((&unit_id, state)) = battlefield.units.iter().find(|(_, state)| {
        state
            .armor_support_last_tick
            .is_some_and(|tick| tick > checkpoint_tick)
    }) {
        bail!(
            "battlefield unit {unit_id} armor support tick {:?} is newer than checkpoint tick {checkpoint_tick}",
            state.armor_support_last_tick
        );
    }
    if let Some((&unit_id, state)) = battlefield.units.iter().find(|(_, state)| {
        state
            .supply_collapsed_tick
            .is_some_and(|tick| tick > checkpoint_tick)
    }) {
        bail!(
            "battlefield unit {unit_id} supply collapse tick {:?} is newer than checkpoint tick {checkpoint_tick}",
            state.supply_collapsed_tick
        );
    }
    Ok(BattlefieldFixture {
        schema: BATTLEFIELD_SCHEMA_VERSION.to_owned(),
        mountains_enabled: battlefield.mountains_enabled,
        terrain_intensity_bits_runs: rle(&battlefield
            .terrain_intensity
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()),
        urban_centers: battlefield
            .urban_centers
            .iter()
            .copied()
            .map(BattlefieldUrbanCenterFixture::from)
            .collect(),
        config: battlefield.config.into(),
        countries: battlefield
            .countries
            .iter()
            .map(|(&country_id, &country)| {
                BattlefieldCountryFixture::from_primitives(country_id, country)
            })
            .collect(),
        units: battlefield
            .units
            .iter()
            .map(|(&unit_id, &state)| BattlefieldUnitStateFixture::from_state(unit_id, state))
            .collect(),
    })
}

fn unit_json(unit: &SimulationUnit, policy: &RuntimeUnitPolicy) -> Value {
    let ai = &policy.ai;
    let ai = json!({
        "baseSpeed": ai.base_speed,
        "terrainSpeedMultiplier": ai.movement.terrain_speed_multiplier,
        "speedMultiplier": ai.movement.speed_multiplier,
        "planSpeedMultiplier": ai.movement.plan_speed_multiplier,
        "neutralPenalty": ai.movement.neutral_penalty,
        "pushReadiness": ai.movement.push_readiness,
        "dealtMultiplier": ai.combat.dealt_multiplier,
        "takenMultiplier": ai.combat.taken_multiplier,
        "defenseBonus": ai.combat.defense_bonus,
        "longWarDefense": ai.combat.long_war_defense,
        "mountain": ai.combat.mountain,
        "urban": ai.combat.urban,
        "isReserve": ai.is_reserve,
        "reinforcementEligible": ai.reinforcement_eligible,
        "encircled": ai.encircled,
        "deployUntilTick": ai.deploy_until_tick,
        "garrisonExcluded": ai.garrison_excluded,
    });
    let influence = policy.influence.as_ref().map(|influence| {
        json!({
            "radius": influence.radius,
            "delta": influence.delta,
            "concentrationBonus": influence.concentration_bonus,
            "beneficiaryCountryId": influence.beneficiary,
            "protectedOwnerIds": influence.protected_owner_ids,
            "rebelDeJure": influence.rebel_de_jure,
            "creditDeJure": influence.credit_de_jure,
            "creditDeJureByCountry": influence.credit_de_jure_by_country,
            "refusesOffense": influence.refuses_offense,
            "temporalSeed": influence.browser_temporal_seed,
        })
    });
    let command = policy.command;
    let command = json!({
        "band": command.band,
        "discipline": command.discipline,
        "refusesOffense": command.refuses_offense,
        "returnHome": command.return_home,
        "selfDefenseOnly": command.self_defense_only,
        "homeTarget": command.home_target.map(|target| json!({
            "cell": target.cell,
            "lat": target.lat,
            "lng": target.lng,
        })),
        "transitionCycle": command.transition_cycle,
    });
    json!({"id":unit.combat.id,"side":unit.combat.side,"countryId":unit.combat.sovereign,"kind":format!("{:?}",unit.combat.kind).to_ascii_lowercase(),"lat":unit.combat.lat,"lng":unit.combat.lng,"health":unit.combat.health,"maxHealth":unit.combat.max_health,"personnel":unit.combat.personnel,"personnelCapacity":unit.combat.personnel_capacity,"equipment":unit.combat.equipment,"maxEquipment":unit.combat.max_equipment,"quality":unit.combat.quality,"transport":unit.combat.transport,"armorSupported":unit.combat.armor_supported,"landingPenaltyActive":unit.combat.landing_penalty_active,"atSea":unit.combat.at_sea,"lastCombatTick":unit.combat.last_combat_tick,"victoryBoostTicks":unit.combat.victory_boost_ticks,"dirLat":unit.dir_lat,"dirLng":unit.dir_lng,"coastStuckTicks":unit.coast_stuck_ticks,"armorLandingPenaltyUntilTick":unit.armor_landing_penalty_until_tick,"isSupport":unit.is_support,"allyWeight":unit.ally_weight,"aiPolicy":ai,"commandPolicy":command,"influencePolicy":influence})
}

fn covered_casualties(
    src: &BTreeMap<u16, f64>,
    countries: &BTreeMap<u16, usize>,
) -> BTreeMap<u16, f64> {
    countries
        .keys()
        .map(|&id| (id, src.get(&id).copied().unwrap_or(0.0)))
        .collect()
}
fn covered_nested_casualties(
    src: &BTreeMap<u16, BTreeMap<u16, f64>>,
    countries: &BTreeMap<u16, usize>,
) -> BTreeMap<u16, BTreeMap<u16, f64>> {
    countries
        .keys()
        .map(|&v| {
            (
                v,
                countries
                    .keys()
                    .filter(|&&a| a != v)
                    .map(|&a| {
                        (
                            a,
                            src.get(&v).and_then(|m| m.get(&a)).copied().unwrap_or(0.0),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

const DEFAULT_GRID_RESOLUTION: f64 = 0.15;
const DEFAULT_BENCH_REPEAT: usize = 7;
const DEFAULT_BENCH_WARMUP: usize = 2;
const TERRITORY_TILE_SIZE: usize = 32;
const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;
const PERSONNEL_PER_FORMATION: f64 = 1_000.0;

pub fn run_production_inspect_command(args: Vec<String>) -> Result<()> {
    let options = parse_production_inspect_args(args)?;
    let raw = read_file(&options.scenario_path)?;
    let sha256 = sha256_hex(&raw);
    let target = GridSpec::world(options.grid_res)?;
    let decoded = decode_mwsc_gzip(&raw, Some(target))?;
    let production = derive_scenario_production(&decoded, &ProductionConfig::default())?;
    let scenario_name = scenario_name(&decoded);
    let selected = options
        .country
        .as_deref()
        .map(|selector| selected_country_report(selector, &production))
        .transpose()?;

    let report = ProductionInspectReport {
        schema: mw_core::PRODUCTION_SCHEMA_VERSION,
        scenario: ScenarioIdentityReport {
            sha256,
            name: scenario_name,
            source_grid: GridReport::from(decoded.source),
            target_grid: GridReport::from(decoded.target),
            entry_count: decoded.entry_count,
        },
        counters: ProductionCountersReport::from(&production),
        selected_country: selected,
    };
    print_json(&report, options.compact)
}

pub fn run_fixture_command(args: Vec<String>) -> Result<()> {
    let options = parse_fixture_args(args)?;
    let prepared = prepare_runtime(&options.scenario_path, &options.checkpoint_path)?;
    let requested_steps = options.ticks.unwrap_or(prepared.checkpoint.steps);
    let execution = execute_fixture(&prepared, requested_steps)?;
    let body = RuntimeFixtureReportBody {
        schema: prepared.schema(),
        runtime_schema: NATIVE_RUNTIME_SCHEMA_VERSION,
        checkpoint_boundary: prepared.boundary_report(),
        scenario: prepared.identity_report(),
        checkpoint: prepared.checkpoint_report(),
        production: prepared.production_report(),
        requested_steps,
        completed_steps: execution.steps.len(),
        render_updates_drained: execution.render_updates_drained,
        initial: execution.initial,
        steps: execution.steps,
    };
    let checksum = checksum_serializable(&body)?;
    print_json(&RuntimeFixtureReport { body, checksum }, options.compact)
}

pub fn run_bench_command(args: Vec<String>) -> Result<()> {
    let options = parse_bench_args(args)?;
    let prepared = prepare_runtime(&options.scenario_path, &options.checkpoint_path)?;
    let ticks = options.ticks.unwrap_or(prepared.checkpoint.steps);

    // Establish determinism outside the timed samples. Scenario parsing,
    // decompression, production derivation, and runtime construction are all
    // deliberately excluded from the reported kernel time.
    let left = execute_benchmark_fresh(&prepared, ticks)?;
    let right = execute_benchmark_fresh(&prepared, ticks)?;
    if left.completed_steps != right.completed_steps
        || left.gated != right.gated
        || left.checksum != right.checksum
    {
        bail!("fresh native runtime executions are not deterministic");
    }
    if left.completed_steps != ticks {
        bail!(
            "native runtime reached its strategic-effects gate after {} of {ticks} requested ticks",
            left.completed_steps
        );
    }

    for _ in 0..options.warmup {
        black_box(execute_benchmark_fresh(&prepared, ticks)?);
    }

    let mut fresh_samples = Vec::with_capacity(options.repeat);
    let mut fresh_checksum = None;
    let mut fresh_render_updates = 0_usize;
    for _ in 0..options.repeat {
        let sample = execute_benchmark_fresh(&prepared, ticks)?;
        if sample.completed_steps != ticks || sample.gated {
            bail!("fresh runtime benchmark was interrupted by strategic effects");
        }
        if fresh_checksum
            .as_ref()
            .is_some_and(|expected| expected != &sample.checksum)
        {
            bail!("fresh runtime benchmark produced divergent checksums");
        }
        fresh_checksum.get_or_insert_with(|| sample.checksum.clone());
        fresh_render_updates = fresh_render_updates.saturating_add(sample.render_updates_drained);
        fresh_samples.push(sample.elapsed_ms);
    }
    let fresh = benchmark_mode_report(
        &fresh_samples,
        options.repeat,
        ticks,
        fresh_render_updates,
        fresh_checksum.context("benchmark produced no fresh checksum")?,
    );

    // This second mode measures a truly persistent runtime rather than a fresh
    // checkpoint per sample. It is published only while the strategic-effects
    // gate remains clear; callers must never silently acknowledge commands.
    let persistent = execute_persistent_benchmark(&prepared, &options, ticks)?;
    let body = RuntimeBenchReportBody {
        schema: prepared.schema(),
        runtime_schema: NATIVE_RUNTIME_SCHEMA_VERSION,
        mode: "bench",
        checkpoint_boundary: prepared.boundary_report(),
        geography: prepared.geography_report(),
        scenario: prepared.identity_report(),
        units: prepared.checkpoint.units.len(),
        sides: prepared.checkpoint.sides.len(),
        countries: prepared.country_to_side.len(),
        repeat: options.repeat,
        warmup: options.warmup,
        ticks_per_sample: ticks,
        fresh,
        persistent,
    };
    let checksum = benchmark_semantic_checksum(&body)?;
    print_json(&RuntimeBenchReport { body, checksum }, options.compact)
}

fn benchmark_semantic_checksum(body: &RuntimeBenchReportBody) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SemanticBenchmark<'a> {
        schema: &'a str,
        runtime_schema: &'a str,
        checkpoint_boundary: CheckpointBoundary,
        scenario_sha256: &'a str,
        units: usize,
        sides: usize,
        countries: usize,
        repeat: usize,
        ticks_per_sample: usize,
        fresh_state_checksum: &'a str,
        persistent_available: bool,
        persistent_gated: bool,
        persistent_completed_samples: usize,
        persistent_completed_ticks: usize,
        persistent_state_checksum: &'a str,
    }

    checksum_serializable(&SemanticBenchmark {
        schema: body.schema,
        runtime_schema: body.runtime_schema,
        checkpoint_boundary: body.checkpoint_boundary.kind,
        scenario_sha256: &body.scenario.sha256,
        units: body.units,
        sides: body.sides,
        countries: body.countries,
        repeat: body.repeat,
        ticks_per_sample: body.ticks_per_sample,
        fresh_state_checksum: &body.fresh.checksum,
        persistent_available: body.persistent.available,
        persistent_gated: body.persistent.gated,
        persistent_completed_samples: body.persistent.completed_samples,
        persistent_completed_ticks: body.persistent.completed_ticks,
        persistent_state_checksum: &body.persistent.final_checksum,
    })
}

#[derive(Debug)]
struct ProductionInspectOptions {
    scenario_path: PathBuf,
    grid_res: f64,
    country: Option<String>,
    compact: bool,
}

fn parse_production_inspect_args(args: Vec<String>) -> Result<ProductionInspectOptions> {
    let mut scenario_path = None;
    let mut grid_res = DEFAULT_GRID_RESOLUTION;
    let mut country = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--grid-res" => {
                index += 1;
                grid_res = args
                    .get(index)
                    .context("--grid-res needs a value")?
                    .parse()
                    .context("invalid --grid-res")?;
            }
            "--country" => {
                index += 1;
                country = Some(
                    args.get(index)
                        .context("--country needs an id or name")?
                        .clone(),
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown production-inspect option {flag:?}"),
            value if scenario_path.is_none() => scenario_path = Some(PathBuf::from(value)),
            _ => bail!("production-inspect accepts exactly one scenario path"),
        }
        index += 1;
    }
    if !grid_res.is_finite() || grid_res <= 0.0 {
        bail!("--grid-res must be finite and positive");
    }
    Ok(ProductionInspectOptions {
        scenario_path: scenario_path.context(
            "usage: production-inspect <scenario.mwsc.gz> [--grid-res N] [--country ID|NAME] [--json]",
        )?,
        grid_res,
        country,
        compact,
    })
}

#[derive(Debug)]
struct FixtureOptions {
    scenario_path: PathBuf,
    checkpoint_path: PathBuf,
    ticks: Option<usize>,
    compact: bool,
}

fn parse_fixture_args(args: Vec<String>) -> Result<FixtureOptions> {
    let mut paths = Vec::new();
    let mut ticks = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--ticks" => {
                index += 1;
                ticks = Some(
                    args.get(index)
                        .context("--ticks needs a value")?
                        .parse()
                        .context("invalid --ticks")?,
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => {
                bail!("unknown native-runtime-fixture option {flag:?}")
            }
            value => paths.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if paths.len() != 2 || ticks == Some(0) {
        bail!(
            "usage: native-runtime-fixture <scenario.mwsc.gz> <checkpoint.json> [--ticks N] [--json]"
        );
    }
    Ok(FixtureOptions {
        scenario_path: paths.remove(0),
        checkpoint_path: paths.remove(0),
        ticks,
        compact,
    })
}

#[derive(Debug)]
struct BenchOptions {
    scenario_path: PathBuf,
    checkpoint_path: PathBuf,
    repeat: usize,
    warmup: usize,
    ticks: Option<usize>,
    compact: bool,
}

fn parse_bench_args(args: Vec<String>) -> Result<BenchOptions> {
    let mut paths = Vec::new();
    let mut repeat = DEFAULT_BENCH_REPEAT;
    let mut warmup = DEFAULT_BENCH_WARMUP;
    let mut ticks = None;
    let mut compact = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repeat" => {
                index += 1;
                repeat = args
                    .get(index)
                    .context("--repeat needs a value")?
                    .parse()
                    .context("invalid --repeat")?;
            }
            "--warmup" => {
                index += 1;
                warmup = args
                    .get(index)
                    .context("--warmup needs a value")?
                    .parse()
                    .context("invalid --warmup")?;
            }
            "--ticks" => {
                index += 1;
                ticks = Some(
                    args.get(index)
                        .context("--ticks needs a value")?
                        .parse()
                        .context("invalid --ticks")?,
                );
            }
            "--json" => compact = true,
            flag if flag.starts_with('-') => bail!("unknown native-runtime-bench option {flag:?}"),
            value => paths.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if paths.len() != 2 || repeat == 0 || ticks == Some(0) {
        bail!(
            "usage: native-runtime-bench <scenario.mwsc.gz> <checkpoint.json> [--repeat N] [--warmup N] [--ticks N] [--json]"
        );
    }
    Ok(BenchOptions {
        scenario_path: paths.remove(0),
        checkpoint_path: paths.remove(0),
        repeat,
        warmup,
        ticks,
        compact,
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeCheckpointFixture {
    schema: String,
    checkpoint_boundary: CheckpointBoundary,
    scenario: ScenarioExpectation,
    #[serde(default)]
    geography: Option<GeographyFixture>,
    #[serde(default)]
    territory: Option<TerritoryV2Fixture>,
    /// Optional raw battlefield inputs. Absence preserves the legacy frozen-policy mode.
    #[serde(default, deserialize_with = "deserialize_optional_battlefield_fixture")]
    battlefield: Option<BattlefieldFixture>,
    /// History-dependent frontier diffusion work. It is mandatory for v3 and forbidden before v3.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_influence_runtime_fixture"
    )]
    influence_runtime: Option<InfluenceRuntimeFixture>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_side_dynamics_fixture"
    )]
    side_dynamics: Option<SideDynamicsFixture>,
    sides: Vec<SideFixture>,
    active_sides: Vec<u16>,
    hostility_matrix: Vec<u8>,
    tick: u64,
    frame: u64,
    war_grace_end: u64,
    strategic_cycle: u64,
    steps: usize,
    #[serde(default)]
    planner: Option<PlannerFixture>,
    units: Vec<RuntimeUnitFixture>,
    economies: Vec<EconomyFixture>,
    #[serde(default)]
    occupations: Vec<OccupationFixture>,
    #[serde(default)]
    casualties: BTreeMap<u16, f64>,
    #[serde(default)]
    casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    #[serde(default, deserialize_with = "deserialize_optional_operational_ai")]
    operational_ai: Option<OperationalRuntimeState>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_operational_execution"
    )]
    operational_execution: Option<OperationalExecutionState>,
    #[serde(default, deserialize_with = "deserialize_optional_air_power")]
    air_power: Option<AirPowerState>,
    #[serde(default, deserialize_with = "deserialize_optional_naval_planning")]
    naval_planning: Option<NavalPlanningState>,
    #[serde(default)]
    gameplay_rng: Option<GameplayRngFixture>,
    #[serde(default)]
    personnel_reserves: Option<Vec<f64>>,
    #[serde(default)]
    reinforcement: Option<ReinforcementState>,
    #[serde(default)]
    material_logistics: Option<mw_core::MaterialLogisticsState>,
    #[serde(default)]
    strategic_missiles: Option<mw_core::StrategicMissileState>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GameplayRngFixture {
    schema: String,
    algorithm: String,
    state: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideDynamicsFixture {
    schema: String,
    sides: Vec<SideDynamicsSideFixture>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideDynamicsSideFixture {
    side_index: u16,
    initial_personnel: f64,
    personnel: f64,
    momentum_history: Vec<SideDynamicsSampleFixture>,
    war_phase: SideDynamicsWarPhaseFixture,
    posture: SideDynamicsPostureFixture,
    #[serde(deserialize_with = "deserialize_required_nullable_side_dynamics_posture")]
    posture_override: Option<SideDynamicsPostureFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SideDynamicsWarPhaseFixture {
    Advancing,
    Stalemate,
    Retreating,
    Collapsing,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SideDynamicsPostureFixture {
    Offensive,
    Balanced,
    Defensive,
}

fn deserialize_required_nullable_side_dynamics_posture<'de, D>(
    deserializer: D,
) -> Result<Option<SideDynamicsPostureFixture>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<SideDynamicsPostureFixture>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideDynamicsSampleFixture {
    frame: u64,
    controlled: u64,
}

fn deserialize_optional_side_dynamics_fixture<'de, D>(
    deserializer: D,
) -> Result<Option<SideDynamicsFixture>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    SideDynamicsFixture::deserialize(deserializer).map(Some)
}

/// Native v2 continuation-only state. Browser-generated v2 checkpoints may omit
/// this block and intentionally begin with a fresh deterministic front refresh.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlannerFixture {
    objectives: Vec<PlannerObjectiveFixture>,
    #[serde(deserialize_with = "deserialize_unique_u64_map")]
    prior_objective_by_unit: BTreeMap<u64, u64>,
    front_prior_by_unit: Vec<FrontPriorFixture>,
    #[serde(deserialize_with = "deserialize_required_nullable_tick")]
    last_front_refresh_tick: Option<u64>,
}

fn deserialize_required_nullable_tick<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)
}

fn deserialize_unique_u64_map<'de, D>(deserializer: D) -> Result<BTreeMap<u64, u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueU64MapVisitor;

    impl<'de> serde::de::Visitor<'de> for UniqueU64MapVisitor {
        type Value = BTreeMap<u64, u64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with unique unsigned integer keys")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((unit_id, objective_id)) = access.next_entry::<u64, u64>()? {
                if values.insert(unit_id, objective_id).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate planner unit id {unit_id}"
                    )));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueU64MapVisitor)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlannerObjectiveFixture {
    id: u64,
    side_pair: [u16; 2],
    segment_id: u64,
    lat: f64,
    lng: f64,
    capacity: usize,
    priority: i32,
}

impl From<&FrontObjective> for PlannerObjectiveFixture {
    fn from(objective: &FrontObjective) -> Self {
        Self {
            id: objective.id,
            side_pair: objective.side_pair,
            segment_id: objective.segment_id,
            lat: objective.lat,
            lng: objective.lng,
            capacity: objective.capacity,
            priority: objective.priority,
        }
    }
}

impl From<&PlannerObjectiveFixture> for FrontObjective {
    fn from(objective: &PlannerObjectiveFixture) -> Self {
        Self {
            id: objective.id,
            side_pair: objective.side_pair,
            segment_id: objective.segment_id,
            lat: objective.lat,
            lng: objective.lng,
            capacity: objective.capacity,
            priority: objective.priority,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrontPriorFixture {
    unit_id: u64,
    pair_key: String,
    segment_idx: usize,
    objective_id: u64,
}

impl From<&FrontLayoutPrior> for FrontPriorFixture {
    fn from(prior: &FrontLayoutPrior) -> Self {
        Self {
            unit_id: prior.unit_id,
            pair_key: prior.pair_key.clone(),
            segment_idx: prior.segment_idx,
            objective_id: prior.objective_id,
        }
    }
}

impl From<&FrontPriorFixture> for FrontLayoutPrior {
    fn from(prior: &FrontPriorFixture) -> Self {
        Self {
            unit_id: prior.unit_id,
            pair_key: prior.pair_key.clone(),
            segment_idx: prior.segment_idx,
            objective_id: prior.objective_id,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeographyFixture {
    land_runs: Vec<(u64, u8)>,
    world_control_runs: Vec<(u64, u16)>,
    de_jure_runs: Vec<(u64, u16)>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerritoryV2Fixture {
    encoding: String,
    maps: TerritoryV2MapsFixture,
    revisions: TerritoryRevisionFixture,
    committed_census: TerritoryCommittedCensusFixture,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerritoryV2MapsFixture {
    land_runs: Vec<(u64, u8)>,
    world_control_runs: Vec<(u64, u16)>,
    de_jure_runs: Vec<(u64, u16)>,
    primary_occupier_runs: Vec<(u64, u16)>,
    dominant_side_runs: Vec<(u64, i16)>,
    occupation_bits_runs: Vec<(u64, u32)>,
    side_influence_bits_runs: Vec<Vec<(u64, u32)>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerritoryRevisionFixture {
    topology_revision: u64,
    world_revision: u64,
    city_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerritoryCommittedCensusFixture {
    generation: u64,
    commit_sequence: u64,
    mutation_sequence: u64,
    processed_tiles: usize,
    processed_items: usize,
}

/// Canonical pending frontier work. Queue entries may be stale or duplicated because that is
/// observable browser behavior; only the sparse queued-state table is canonicalized.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InfluenceRuntimeFixture {
    schema: String,
    regular_queue: Vec<usize>,
    priority_queue: Vec<usize>,
    queued_cells: Vec<(usize, u8)>,
}

fn deserialize_optional_influence_runtime_fixture<'de, D>(
    deserializer: D,
) -> Result<Option<InfluenceRuntimeFixture>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    InfluenceRuntimeFixture::deserialize(deserializer).map(Some)
}

/// Complete live battlefield inputs. The root field is optional for old checkpoints, but once
/// this object is present every field is required and unknown fields are rejected.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BattlefieldFixture {
    schema: String,
    mountains_enabled: bool,
    terrain_intensity_bits_runs: Vec<(u64, u32)>,
    urban_centers: Vec<BattlefieldUrbanCenterFixture>,
    config: BattlefieldConfigFixture,
    countries: Vec<BattlefieldCountryFixture>,
    units: Vec<BattlefieldUnitStateFixture>,
}

fn deserialize_optional_battlefield_fixture<'de, D>(
    deserializer: D,
) -> Result<Option<BattlefieldFixture>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BattlefieldFixture::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BattlefieldConfigFixture {
    unit_speed: f64,
    unit_naval_speed: f64,
    influence_rate: f64,
    influence_radius: f64,
    encirclement_radius: f64,
    alpen_mountain_speed_multiplier: f64,
    alpen_combat_multiplier: f64,
    native_speed_scale: f64,
    active_combat_exclusion_frames: u64,
    long_war_frame_threshold: u64,
    long_war_defense_multiplier: f64,
    armor_support_radius: f64,
    armor_support_memory_ticks: u64,
}

impl From<BattlefieldConfigFixture> for BattlefieldConfig {
    fn from(value: BattlefieldConfigFixture) -> Self {
        Self {
            unit_speed: value.unit_speed,
            unit_naval_speed: value.unit_naval_speed,
            influence_rate: value.influence_rate,
            influence_radius: value.influence_radius,
            encirclement_radius: value.encirclement_radius,
            armor_support_radius: value.armor_support_radius,
            armor_support_memory_ticks: value.armor_support_memory_ticks,
            alpen_mountain_speed_multiplier: value.alpen_mountain_speed_multiplier,
            alpen_combat_multiplier: value.alpen_combat_multiplier,
            native_speed_scale: value.native_speed_scale,
            active_combat_exclusion_frames: value.active_combat_exclusion_frames,
            long_war_frame_threshold: value.long_war_frame_threshold,
            long_war_defense_multiplier: value.long_war_defense_multiplier,
        }
    }
}

impl From<BattlefieldConfig> for BattlefieldConfigFixture {
    fn from(value: BattlefieldConfig) -> Self {
        Self {
            unit_speed: value.unit_speed,
            unit_naval_speed: value.unit_naval_speed,
            influence_rate: value.influence_rate,
            influence_radius: value.influence_radius,
            encirclement_radius: value.encirclement_radius,
            alpen_mountain_speed_multiplier: value.alpen_mountain_speed_multiplier,
            alpen_combat_multiplier: value.alpen_combat_multiplier,
            native_speed_scale: value.native_speed_scale,
            active_combat_exclusion_frames: value.active_combat_exclusion_frames,
            long_war_frame_threshold: value.long_war_frame_threshold,
            long_war_defense_multiplier: value.long_war_defense_multiplier,
            armor_support_radius: value.armor_support_radius,
            armor_support_memory_ticks: value.armor_support_memory_ticks,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BattlefieldUrbanCenterFixture {
    id: u64,
    country_id: u16,
    cell: usize,
    lat: f64,
    lng: f64,
}

impl From<BattlefieldUrbanCenterFixture> for BattlefieldUrbanCenter {
    fn from(value: BattlefieldUrbanCenterFixture) -> Self {
        Self {
            id: value.id,
            country_id: value.country_id,
            cell: value.cell,
            lat: value.lat,
            lng: value.lng,
        }
    }
}

impl From<BattlefieldUrbanCenter> for BattlefieldUrbanCenterFixture {
    fn from(value: BattlefieldUrbanCenter) -> Self {
        Self {
            id: value.id,
            country_id: value.country_id,
            cell: value.cell,
            lat: value.lat,
            lng: value.lng,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum BattlefieldBuffFixture {
    None,
    Buff,
    Super,
    Godly,
    Weakened,
    Crippled,
}

impl From<BattlefieldBuffFixture> for BattlefieldBuff {
    fn from(value: BattlefieldBuffFixture) -> Self {
        match value {
            BattlefieldBuffFixture::None => Self::None,
            BattlefieldBuffFixture::Buff => Self::Buff,
            BattlefieldBuffFixture::Super => Self::Super,
            BattlefieldBuffFixture::Godly => Self::Godly,
            BattlefieldBuffFixture::Weakened => Self::Weakened,
            BattlefieldBuffFixture::Crippled => Self::Crippled,
        }
    }
}

impl From<BattlefieldBuff> for BattlefieldBuffFixture {
    fn from(value: BattlefieldBuff) -> Self {
        match value {
            BattlefieldBuff::None => Self::None,
            BattlefieldBuff::Buff => Self::Buff,
            BattlefieldBuff::Super => Self::Super,
            BattlefieldBuff::Godly => Self::Godly,
            BattlefieldBuff::Weakened => Self::Weakened,
            BattlefieldBuff::Crippled => Self::Crippled,
        }
    }
}

/// The browser exposes four phase labels, but RETREATING and STALEMATE have identical live
/// battlefield behavior. Core intentionally normalizes both to Stable.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum BattlefieldWarPhaseFixture {
    #[serde(rename = "ADVANCING")]
    Advancing,
    #[serde(rename = "STALEMATE")]
    Stalemate,
    #[serde(rename = "RETREATING")]
    Retreating,
    #[serde(rename = "COLLAPSING")]
    Collapsing,
}

impl From<BattlefieldWarPhaseFixture> for BattlefieldWarPhase {
    fn from(value: BattlefieldWarPhaseFixture) -> Self {
        match value {
            BattlefieldWarPhaseFixture::Advancing => Self::Advancing,
            BattlefieldWarPhaseFixture::Collapsing => Self::Collapsing,
            BattlefieldWarPhaseFixture::Stalemate | BattlefieldWarPhaseFixture::Retreating => {
                Self::Stable
            }
        }
    }
}

impl From<BattlefieldWarPhase> for BattlefieldWarPhaseFixture {
    fn from(value: BattlefieldWarPhase) -> Self {
        match value {
            BattlefieldWarPhase::Advancing => Self::Advancing,
            BattlefieldWarPhase::Collapsing => Self::Collapsing,
            BattlefieldWarPhase::Stable => Self::Stalemate,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BattlefieldCountryFixture {
    country_id: u16,
    combat_buff: BattlefieldBuffFixture,
    influence_buff: BattlefieldBuffFixture,
    attack_buff_percent: f64,
    defense_buff_percent: f64,
    capital_lost: bool,
    war_phase: BattlefieldWarPhaseFixture,
    conquest_mode: bool,
    ai_speed_multiplier: f64,
}

impl BattlefieldCountryFixture {
    fn primitives(self) -> CountryBattlefieldPrimitives {
        CountryBattlefieldPrimitives {
            combat_buff: self.combat_buff.into(),
            influence_buff: self.influence_buff.into(),
            attack_buff_percent: self.attack_buff_percent,
            defense_buff_percent: self.defense_buff_percent,
            capital_lost: self.capital_lost,
            war_phase: self.war_phase.into(),
            conquest_mode: self.conquest_mode,
            ai_speed_multiplier: self.ai_speed_multiplier,
        }
    }

    fn from_primitives(country_id: u16, value: CountryBattlefieldPrimitives) -> Self {
        Self {
            country_id,
            combat_buff: value.combat_buff.into(),
            influence_buff: value.influence_buff.into(),
            attack_buff_percent: value.attack_buff_percent,
            defense_buff_percent: value.defense_buff_percent,
            capital_lost: value.capital_lost,
            war_phase: value.war_phase.into(),
            conquest_mode: value.conquest_mode,
            ai_speed_multiplier: value.ai_speed_multiplier,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BattlefieldUnitStateFixture {
    unit_id: u64,
    is_alpenjager: bool,
    cohesion_seed: f64,
    local_tactics_excluded: bool,
    encircled_ticks: u64,
    #[serde(deserialize_with = "deserialize_required_nullable_tick")]
    armor_support_last_tick: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_required_nullable_tick")]
    supply_collapsed_tick: Option<u64>,
    last_ally_count: f64,
}

impl BattlefieldUnitStateFixture {
    fn state(self) -> BattlefieldUnitState {
        BattlefieldUnitState {
            is_alpenjager: self.is_alpenjager,
            cohesion_seed: self.cohesion_seed,
            local_tactics_excluded: self.local_tactics_excluded,
            encircled_ticks: self.encircled_ticks,
            armor_support_last_tick: self.armor_support_last_tick,
            supply_collapsed_tick: self.supply_collapsed_tick,
            last_ally_count: self.last_ally_count,
        }
    }

    fn from_state(unit_id: u64, value: BattlefieldUnitState) -> Self {
        Self {
            unit_id,
            is_alpenjager: value.is_alpenjager,
            cohesion_seed: value.cohesion_seed,
            local_tactics_excluded: value.local_tactics_excluded,
            encircled_ticks: value.encircled_ticks,
            armor_support_last_tick: value.armor_support_last_tick,
            supply_collapsed_tick: value.supply_collapsed_tick,
            last_ally_count: value.last_ally_count,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum CheckpointBoundary {
    PostStartWar,
    BaselineReplay,
    MidWar,
}

impl CheckpointBoundary {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PostStartWar => "postStartWar",
            Self::BaselineReplay => "baselineReplay",
            Self::MidWar => "midWar",
        }
    }

    const fn resumable(self) -> bool {
        matches!(self, Self::PostStartWar | Self::MidWar)
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PostStartWar => {
                "production-resumable checkpoint captured before the first simulation tick"
            }
            Self::BaselineReplay => {
                "synthetic baseline replay for fixtures and benchmarks; not resumable game state"
            }
            Self::MidWar => {
                "production-resumable quiescent mid-war checkpoint with committed live territory"
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScenarioExpectation {
    sha256: String,
    name: String,
    grid_res: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideFixture {
    side_index: u16,
    country_ids: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UnitKindFixture {
    Army,
    Armor,
}

impl From<UnitKindFixture> for UnitKind {
    fn from(value: UnitKindFixture) -> Self {
        match value {
            UnitKindFixture::Army => Self::Army,
            UnitKindFixture::Armor => Self::Armor,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeUnitFixture {
    id: u64,
    side: u16,
    country_id: u16,
    kind: UnitKindFixture,
    lat: f64,
    lng: f64,
    health: f64,
    max_health: f64,
    personnel: u64,
    personnel_capacity: u64,
    equipment: u64,
    max_equipment: u64,
    quality: f64,
    transport: bool,
    armor_supported: bool,
    landing_penalty_active: bool,
    at_sea: bool,
    last_combat_tick: u64,
    victory_boost_ticks: u64,
    dir_lat: f64,
    dir_lng: f64,
    coast_stuck_ticks: u32,
    armor_landing_penalty_until_tick: u64,
    is_support: bool,
    ally_weight: f64,
    ai_policy: AiPolicyFixture,
    #[serde(default)]
    command_policy: Option<CommandPolicyFixture>,
    influence_policy: Option<InfluencePolicyFixture>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandPolicyFixture {
    band: CommandBand,
    discipline: f64,
    refuses_offense: bool,
    return_home: bool,
    self_defense_only: bool,
    home_target: Option<CommandHomeTargetFixture>,
    transition_cycle: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandHomeTargetFixture {
    cell: usize,
    lat: f64,
    lng: f64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiPolicyFixture {
    base_speed: f64,
    terrain_speed_multiplier: f64,
    speed_multiplier: f64,
    plan_speed_multiplier: f64,
    neutral_penalty: f64,
    push_readiness: f64,
    dealt_multiplier: f64,
    taken_multiplier: f64,
    defense_bonus: f64,
    long_war_defense: f64,
    mountain: bool,
    urban: bool,
    is_reserve: bool,
    reinforcement_eligible: bool,
    encircled: bool,
    deploy_until_tick: u64,
    garrison_excluded: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InfluencePolicyFixture {
    radius: f64,
    delta: f64,
    concentration_bonus: f64,
    beneficiary_country_id: Option<u16>,
    protected_owner_ids: Vec<u16>,
    rebel_de_jure: Option<u16>,
    credit_de_jure: Option<u16>,
    credit_de_jure_by_country: BTreeMap<u16, u16>,
    refuses_offense: bool,
    #[serde(default)]
    temporal_seed: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OccupationFixture {
    victim_id: u16,
    annexer_id: u16,
    base_income: f64,
    core_cells: u32,
    expected_army_units: f64,
    resistance: f64,
    occupation_coverage: f64,
    garrison_coverage: f64,
    garrison_assigned: f64,
    required_garrison: u32,
    held_ratio: f64,
    active_rebellion: bool,
    queued_at_cycle: u64,
    cooldown_until_cycle: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EconomyFixture {
    country_id: u16,
    economic_strength: f64,
    base_income: f64,
    treasury: f64,
    income: f64,
    occupation_yield: f64,
    payroll_due: f64,
    occupation_due: f64,
    payroll_coverage: f64,
    occupation_coverage: f64,
    arrears_cycles: f64,
    command_band: CommandBand,
    mutiny_recovery_cycles: u32,
    initial_core_cells: u32,
    initial_city_population: f64,
    core_control_ratio: f64,
    city_control_ratio: f64,
    capital_held: bool,
    last_event_band: CommandBand,
    capitulated: bool,
}

impl From<&EconomyFixture> for EconomyState {
    fn from(value: &EconomyFixture) -> Self {
        Self {
            country_id: value.country_id,
            economic_strength: value.economic_strength,
            base_income: value.base_income,
            treasury: value.treasury,
            income: value.income,
            occupation_yield: value.occupation_yield,
            payroll_due: value.payroll_due,
            occupation_due: value.occupation_due,
            payroll_coverage: value.payroll_coverage,
            occupation_coverage: value.occupation_coverage,
            arrears_cycles: value.arrears_cycles,
            command_band: value.command_band,
            mutiny_recovery_cycles: value.mutiny_recovery_cycles,
            initial_core_cells: value.initial_core_cells,
            initial_city_population: value.initial_city_population,
            core_control_ratio: value.core_control_ratio,
            city_control_ratio: value.city_control_ratio,
            capital_held: value.capital_held,
            last_event_band: value.last_event_band,
            capitulated: value.capitulated,
        }
    }
}

impl From<&OccupationFixture> for OccupationState {
    fn from(value: &OccupationFixture) -> Self {
        Self {
            victim_id: value.victim_id,
            annexer_id: value.annexer_id,
            base_income: value.base_income,
            core_cells: value.core_cells,
            expected_army_units: value.expected_army_units,
            resistance: value.resistance,
            occupation_coverage: value.occupation_coverage,
            garrison_coverage: value.garrison_coverage,
            garrison_assigned: value.garrison_assigned,
            required_garrison: value.required_garrison,
            held_ratio: value.held_ratio,
            active_rebellion: value.active_rebellion,
            queued_at_cycle: value.queued_at_cycle,
            cooldown_until_cycle: value.cooldown_until_cycle,
        }
    }
}

struct PreparedRuntime {
    raw_sha256: String,
    scenario_name: String,
    decoded: DecodedScenario,
    baseline: DecodedScenario,
    production: ScenarioProduction,
    checkpoint: RuntimeCheckpointFixture,
    country_to_side: BTreeMap<u16, usize>,
    coalition_by_side: Vec<BTreeSet<u16>>,
    live_territory_maps: Option<TerritoryMaps>,
    battlefield: Option<BattlefieldRuntimeState>,
    influence_runtime: Option<mw_core::diffusion::InfluenceRuntimeState>,
    side_dynamics: Option<BTreeMap<usize, mw_core::SideDynamics>>,
    operational_ai: Option<OperationalRuntimeState>,
    operational_execution: Option<OperationalExecutionState>,
    air_power: Option<AirPowerState>,
    naval_planning: Option<NavalPlanningState>,
}

/// Fully validated production handoff for native rendering and simulation.
///
/// `decoded` contains the checkpoint's authoritative exact geography. The
/// runtime owns its independent mutable territory state, so the renderer can
/// move these decoded maps into its immutable/GPU caches without decoding the
/// scenario a second time.
pub struct LoadedRuntime {
    pub decoded: DecodedScenario,
    pub baseline: DecodedScenario,
    pub runtime: NativeRuntime,
    pub checkpoint_boundary: &'static str,
    pub resumable: bool,
    pub exact_geography_supplied: bool,
    pub unit_count: usize,
}

/// Read, strictly validate, and build one browser-to-native runtime handoff.
///
/// This is the same path used by `native-runtime-fixture` and
/// `native-runtime-bench`; the viewer does not have a weaker fallback parser.
pub fn load_runtime_checkpoint(
    scenario_path: &PathBuf,
    checkpoint_path: &PathBuf,
) -> Result<LoadedRuntime> {
    let prepared = prepare_runtime(scenario_path, checkpoint_path)?;
    let runtime = build_runtime(&prepared)?;
    let checkpoint_boundary = prepared.checkpoint.checkpoint_boundary.as_str();
    let resumable = prepared.checkpoint.checkpoint_boundary.resumable();
    let exact_geography_supplied =
        prepared.checkpoint.geography.is_some() || prepared.checkpoint.territory.is_some();
    let unit_count = prepared.checkpoint.units.len();
    Ok(LoadedRuntime {
        decoded: prepared.decoded,
        baseline: prepared.baseline,
        runtime,
        checkpoint_boundary,
        resumable,
        exact_geography_supplied,
        unit_count,
    })
}

type RuntimeTopology = (BTreeMap<u16, usize>, Vec<BTreeSet<u16>>);

struct DecodedGeography {
    land: Vec<u8>,
    world_control: Vec<u16>,
    de_jure: Vec<u16>,
}

fn prepare_runtime(scenario_path: &PathBuf, checkpoint_path: &PathBuf) -> Result<PreparedRuntime> {
    let checkpoint_bytes = read_file(checkpoint_path)?;
    let checkpoint_value: Value = serde_json::from_slice(&checkpoint_bytes)
        .with_context(|| format!("failed to parse {}", checkpoint_path.display()))?;
    if matches!(
        checkpoint_value.get("schema").and_then(Value::as_str),
        Some(
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
                | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        )
    ) {
        validate_checkpoint_v5_required_nullable_fields(&checkpoint_value).with_context(|| {
            format!(
                "failed to validate required v5-v12 operational fields in {}",
                checkpoint_path.display()
            )
        })?;
    }
    validate_checkpoint_v8_supply_collapse_fields(&checkpoint_value).with_context(|| {
        format!(
            "failed to validate checkpoint supply-collapse history fields in {}",
            checkpoint_path.display()
        )
    })?;
    let checkpoint: RuntimeCheckpointFixture = serde_json::from_value(checkpoint_value)
        .with_context(|| format!("failed to parse {}", checkpoint_path.display()))?;
    validate_checkpoint_shape(&checkpoint)?;

    let raw = read_file(scenario_path)?;
    let raw_sha256 = sha256_hex(&raw);
    if raw_sha256 != checkpoint.scenario.sha256.to_ascii_lowercase() {
        bail!(
            "scenario SHA-256 mismatch: checkpoint expects {}, loaded {raw_sha256}",
            checkpoint.scenario.sha256
        );
    }
    let target = GridSpec::world(checkpoint.scenario.grid_res)?;
    let mut decoded = decode_mwsc_gzip(&raw, Some(target))?;
    let scenario_name = scenario_name(&decoded);
    if scenario_name != checkpoint.scenario.name {
        bail!(
            "scenario name mismatch: checkpoint expects {:?}, loaded {:?}",
            checkpoint.scenario.name,
            scenario_name
        );
    }
    if let Some(geography) = checkpoint.geography.as_ref() {
        // Decode all three streams before replacing any scenario map. A bad
        // stream therefore cannot leave a partially applied checkpoint.
        let exact = decode_geography(geography, decoded.target.cell_count()?)?;
        decoded.land = exact.land;
        decoded.world_control = exact.world_control;
        decoded.de_jure = exact.de_jure;
    }
    // Immutable geography is the checkpoint-supplied scenario baseline, not
    // necessarily the raw MWSC projection. Capture it before applying v2 live
    // conquest/control maps so a later native save can round-trip the exact
    // browser/native starting geography without baking current territory into
    // production baselines.
    let baseline = decoded.clone();
    let production = derive_scenario_production(&decoded, &ProductionConfig::default())?;
    if checkpoint.geography.is_some() {
        validate_exact_geography_owners(&decoded, &production)?;
    }
    let (country_to_side, coalition_by_side) = topology(&checkpoint)?;
    let live_territory_maps = checkpoint
        .territory
        .as_ref()
        .map(|territory| {
            decode_territory_v2(
                territory,
                decoded.target.cell_count()?,
                checkpoint.sides.len(),
            )
        })
        .transpose()?;
    if let Some(maps) = live_territory_maps.as_ref() {
        validate_live_territory_owners(maps, &production)?;
        // The viewer must receive the same live geography that seeds the
        // runtime rather than the immutable scenario baseline.
        decoded.land.clone_from(&maps.land);
        decoded.world_control.clone_from(&maps.world_control);
        decoded.de_jure.clone_from(&maps.de_jure);
    }
    validate_checkpoint_against_scenario(&checkpoint, &production, &country_to_side)?;
    let live_unit_ids = checkpoint
        .units
        .iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();
    let battlefield = checkpoint
        .battlefield
        .as_ref()
        .map(|battlefield| {
            decode_battlefield(
                battlefield,
                decoded.target,
                &decoded.land,
                checkpoint.sides.len(),
                &country_to_side,
                &live_unit_ids,
                checkpoint.tick,
            )
        })
        .transpose()?;
    let influence_runtime = checkpoint
        .influence_runtime
        .as_ref()
        .map(|influence_runtime| {
            decode_influence_runtime(influence_runtime, decoded.target.cell_count()?)
        })
        .transpose()?;
    let side_dynamics = checkpoint
        .side_dynamics
        .as_ref()
        .map(decode_side_dynamics)
        .transpose()?;
    let operational_ai = checkpoint.operational_ai.clone();
    if let Some(operations) = operational_ai.as_ref() {
        let live = checkpoint
            .units
            .iter()
            .map(|unit| (unit.id, usize::from(unit.side)))
            .collect::<BTreeMap<_, _>>();
        let countries = production
            .countries
            .iter()
            .map(|country| country.country_id)
            .collect();
        validate_operational_ai(
            operations,
            checkpoint.sides.len(),
            &live,
            &countries,
            checkpoint.tick,
            &checkpoint.hostility_matrix,
        )?;
    }
    let operational_execution = checkpoint.operational_execution.clone();
    if let Some(execution) = operational_execution.as_ref() {
        execution
            .validate_shape(checkpoint.tick, checkpoint.sides.len())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    let air_power = checkpoint.air_power.clone();
    if let Some(air_power) = air_power.as_ref() {
        air_power
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    let naval_planning = checkpoint.naval_planning.clone();
    if let Some(naval_planning) = naval_planning.as_ref() {
        let execution = operational_execution
            .as_ref()
            .context("naval planning checkpoint requires operational execution state")?;
        naval_planning
            .validate_with_execution(checkpoint.sides.len(), execution)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    if let Some(reinforcement) = checkpoint.reinforcement.as_ref() {
        let air_power = air_power
            .as_ref()
            .context("reinforcement checkpoint requires air power state")?;
        reinforcement
            .validate(air_power, &country_to_side, checkpoint.sides.len())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }

    Ok(PreparedRuntime {
        raw_sha256,
        scenario_name,
        decoded,
        baseline,
        production,
        checkpoint,
        country_to_side,
        coalition_by_side,
        live_territory_maps,
        battlefield,
        influence_runtime,
        side_dynamics,
        operational_ai,
        operational_execution,
        air_power,
        naval_planning,
    })
}

fn decode_territory_v2(
    territory: &TerritoryV2Fixture,
    cell_count: usize,
    side_count: usize,
) -> Result<TerritoryMaps> {
    if territory.encoding != "rle-bits-v1" {
        bail!(
            "unsupported territory encoding {:?}; expected \"rle-bits-v1\"",
            territory.encoding
        );
    }
    if territory.maps.side_influence_bits_runs.len() != side_count {
        bail!("territory side influence rows must exactly match the checkpoint side count");
    }
    let land = decode_runs(
        &territory.maps.land_runs,
        cell_count,
        "territory.maps.landRuns",
        |value| (*value <= 2).then_some(*value),
    )?;
    let world_control = decode_runs(
        &territory.maps.world_control_runs,
        cell_count,
        "territory.maps.worldControlRuns",
        |value| Some(*value),
    )?;
    let de_jure = decode_runs(
        &territory.maps.de_jure_runs,
        cell_count,
        "territory.maps.deJureRuns",
        |value| Some(*value),
    )?;
    let primary_occupier = decode_runs(
        &territory.maps.primary_occupier_runs,
        cell_count,
        "territory.maps.primaryOccupierRuns",
        |value| Some(*value),
    )?;
    let dominant_side = decode_runs(
        &territory.maps.dominant_side_runs,
        cell_count,
        "territory.maps.dominantSideRuns",
        |value| match *value {
            -1 => Some(-1),
            side if usize::try_from(side).is_ok_and(|side| side < side_count) => Some(side),
            _ => None,
        },
    )?;
    let occupation = decode_runs(
        &territory.maps.occupation_bits_runs,
        cell_count,
        "territory.maps.occupationBitsRuns",
        |bits| {
            let value = f32::from_bits(*bits);
            value.is_finite().then_some(value)
        },
    )?;
    let side_influence = territory
        .maps
        .side_influence_bits_runs
        .iter()
        .enumerate()
        .map(|(side, runs)| {
            decode_runs(
                runs,
                cell_count,
                &format!("territory.maps.sideInfluenceBitsRuns[{side}]"),
                |bits| {
                    let value = f32::from_bits(*bits);
                    (value.is_finite() && value >= 0.0).then_some(value)
                },
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TerritoryMaps {
        land,
        world_control,
        de_jure,
        primary_occupier,
        dominant_side,
        occupation,
        side_influence,
    })
}

fn decode_influence_runtime(
    influence_runtime: &InfluenceRuntimeFixture,
    cell_count: usize,
) -> Result<mw_core::diffusion::InfluenceRuntimeState> {
    if influence_runtime.schema != NATIVE_INFLUENCE_RUNTIME_SCHEMA {
        bail!(
            "unsupported influence runtime schema {:?}; expected {:?}",
            influence_runtime.schema,
            NATIVE_INFLUENCE_RUNTIME_SCHEMA
        );
    }
    if influence_runtime.regular_queue.len() > INFLUENCE_REGULAR_QUEUE_LIMIT {
        bail!(
            "influenceRuntime.regularQueue exceeds the pending-entry limit of {INFLUENCE_REGULAR_QUEUE_LIMIT}"
        );
    }
    if influence_runtime.priority_queue.len() > INFLUENCE_PRIORITY_QUEUE_LIMIT {
        bail!(
            "influenceRuntime.priorityQueue exceeds the pending-entry limit of {INFLUENCE_PRIORITY_QUEUE_LIMIT}"
        );
    }
    for (label, queue) in [
        (
            "influenceRuntime.regularQueue",
            influence_runtime.regular_queue.as_slice(),
        ),
        (
            "influenceRuntime.priorityQueue",
            influence_runtime.priority_queue.as_slice(),
        ),
    ] {
        if let Some(cell) = queue.iter().copied().find(|cell| *cell >= cell_count) {
            bail!("{label} cell {cell} is outside the territory grid");
        }
    }

    let regular_cells = influence_runtime
        .regular_queue
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let priority_cells = influence_runtime
        .priority_queue
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut previous_cell = None;
    for &(cell, state) in &influence_runtime.queued_cells {
        if cell >= cell_count {
            bail!("influenceRuntime.queuedCells cell {cell} is outside the territory grid");
        }
        if previous_cell.is_some_and(|previous| previous >= cell) {
            bail!("influenceRuntime.queuedCells must be strictly sorted with unique cells");
        }
        let matching_entry = match state {
            1 => regular_cells.contains(&cell),
            2 => priority_cells.contains(&cell),
            _ => bail!("influenceRuntime.queuedCells state must be 1 or 2"),
        };
        if !matching_entry {
            bail!(
                "influenceRuntime.queuedCells cell {cell} state {state} has no matching pending queue entry"
            );
        }
        previous_cell = Some(cell);
    }

    Ok(mw_core::diffusion::InfluenceRuntimeState {
        regular_queue: influence_runtime.regular_queue.clone(),
        priority_queue: influence_runtime.priority_queue.clone(),
        queued_cells: influence_runtime.queued_cells.clone(),
    })
}

fn influence_runtime_fixture(
    influence_runtime: &mw_core::diffusion::InfluenceRuntimeState,
    cell_count: usize,
) -> Result<InfluenceRuntimeFixture> {
    let fixture = InfluenceRuntimeFixture {
        schema: NATIVE_INFLUENCE_RUNTIME_SCHEMA.to_owned(),
        regular_queue: influence_runtime.regular_queue.clone(),
        priority_queue: influence_runtime.priority_queue.clone(),
        queued_cells: influence_runtime.queued_cells.clone(),
    };
    // Core checkpoints should already be canonical. Reuse loader validation so a corrupted
    // in-memory state cannot be published as a checkpoint which the strict loader rejects.
    decode_influence_runtime(&fixture, cell_count)?;
    Ok(fixture)
}

fn decode_battlefield(
    battlefield: &BattlefieldFixture,
    grid: GridSpec,
    land: &[u8],
    max_sides: usize,
    country_to_side: &BTreeMap<u16, usize>,
    live_unit_ids: &[u64],
    checkpoint_tick: u64,
) -> Result<BattlefieldRuntimeState> {
    if battlefield.schema != BATTLEFIELD_SCHEMA_VERSION {
        bail!(
            "unsupported battlefield schema {:?}; expected {:?}",
            battlefield.schema,
            BATTLEFIELD_SCHEMA_VERSION
        );
    }
    if battlefield
        .terrain_intensity_bits_runs
        .windows(2)
        .any(|runs| runs[0].1 == runs[1].1)
    {
        bail!("battlefield terrain intensity runs must be maximal");
    }
    let terrain_intensity = decode_runs(
        &battlefield.terrain_intensity_bits_runs,
        grid.cell_count()?,
        "battlefield.terrainIntensityBitsRuns",
        |bits| {
            let value = f32::from_bits(*bits);
            (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value)
        },
    )?;

    if battlefield.urban_centers.iter().any(|city| city.id == 0) {
        bail!("battlefield urban center ids must be positive");
    }

    let mut countries = BTreeMap::new();
    for country in battlefield.countries.iter().copied() {
        if countries
            .insert(country.country_id, country.primitives())
            .is_some()
        {
            bail!(
                "battlefield countries contain duplicate country id {}",
                country.country_id
            );
        }
    }

    let mut units = BTreeMap::new();
    for unit in battlefield.units.iter().copied() {
        if unit
            .armor_support_last_tick
            .is_some_and(|support_tick| support_tick > checkpoint_tick)
        {
            bail!(
                "battlefield unit {} armorSupportLastTick cannot be newer than checkpoint tick",
                unit.unit_id
            );
        }
        if unit
            .supply_collapsed_tick
            .is_some_and(|collapse_tick| collapse_tick > checkpoint_tick)
        {
            bail!(
                "battlefield unit {} supplyCollapsedTick cannot be newer than checkpoint tick",
                unit.unit_id
            );
        }
        if units.insert(unit.unit_id, unit.state()).is_some() {
            bail!(
                "battlefield units contain duplicate unit id {}",
                unit.unit_id
            );
        }
    }

    let state = BattlefieldRuntimeState {
        config: battlefield.config.into(),
        mountains_enabled: battlefield.mountains_enabled,
        terrain_intensity,
        urban_centers: battlefield
            .urban_centers
            .iter()
            .copied()
            .map(BattlefieldUrbanCenter::from)
            .collect(),
        countries,
        units,
    };
    let world = WorldGridView::new(grid.grid_res, grid.width, grid.height, land)?;
    state.validate(world, max_sides, country_to_side, live_unit_ids)?;
    Ok(state)
}

fn validate_live_territory_owners(
    maps: &TerritoryMaps,
    production: &ScenarioProduction,
) -> Result<()> {
    let known = production
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    for (label, owners) in [
        (
            "territory.maps.worldControlRuns",
            maps.world_control.as_slice(),
        ),
        ("territory.maps.deJureRuns", maps.de_jure.as_slice()),
        (
            "territory.maps.primaryOccupierRuns",
            maps.primary_occupier.as_slice(),
        ),
    ] {
        if let Some(owner) = owners
            .iter()
            .copied()
            .find(|owner| *owner != 0 && !known.contains(owner))
        {
            bail!("{label} references country {owner}, which is absent from scenario metadata");
        }
    }
    Ok(())
}

fn decode_geography(geography: &GeographyFixture, cell_count: usize) -> Result<DecodedGeography> {
    let land = decode_runs(
        &geography.land_runs,
        cell_count,
        "geography.landRuns",
        |value| (*value <= 1).then_some(*value),
    )?;
    let world_control = decode_runs(
        &geography.world_control_runs,
        cell_count,
        "geography.worldControlRuns",
        |value| Some(*value),
    )?;
    let de_jure = decode_runs(
        &geography.de_jure_runs,
        cell_count,
        "geography.deJureRuns",
        |value| Some(*value),
    )?;
    Ok(DecodedGeography {
        land,
        world_control,
        de_jure,
    })
}

fn decode_runs<T, U>(
    runs: &[(u64, T)],
    cell_count: usize,
    label: &str,
    validate_value: impl Fn(&T) -> Option<U>,
) -> Result<Vec<U>>
where
    U: Copy,
{
    let mut decoded = Vec::with_capacity(cell_count);
    for (index, (raw_length, raw_value)) in runs.iter().enumerate() {
        if *raw_length == 0 {
            bail!("{label}[{index}] has a zero run length");
        }
        let length = usize::try_from(*raw_length)
            .with_context(|| format!("{label}[{index}] run length exceeds this platform"))?;
        let end = decoded
            .len()
            .checked_add(length)
            .with_context(|| format!("{label}[{index}] run coverage overflow"))?;
        if end > cell_count {
            bail!("{label}[{index}] exceeds the target grid coverage of {cell_count} cells");
        }
        let value = validate_value(raw_value)
            .with_context(|| format!("{label}[{index}] contains an invalid value"))?;
        decoded.resize(end, value);
    }
    if decoded.len() != cell_count {
        bail!(
            "{label} covers {} cells; expected exactly {cell_count}",
            decoded.len()
        );
    }
    Ok(decoded)
}

fn validate_exact_geography_owners(
    decoded: &DecodedScenario,
    production: &ScenarioProduction,
) -> Result<()> {
    let known = production
        .countries
        .iter()
        .map(|country| country.country_id)
        .collect::<BTreeSet<_>>();
    validate_exact_geography_owner_ids(&decoded.world_control, &decoded.de_jure, &known)
}

fn validate_exact_geography_owner_ids(
    world_control: &[u16],
    de_jure: &[u16],
    known: &BTreeSet<u16>,
) -> Result<()> {
    for (label, owners) in [
        ("geography.worldControlRuns", world_control),
        ("geography.deJureRuns", de_jure),
    ] {
        if let Some(owner) = owners
            .iter()
            .copied()
            .find(|owner| *owner != 0 && !known.contains(owner))
        {
            bail!("{label} references country {owner}, which is absent from scenario metadata");
        }
    }
    Ok(())
}

fn validate_checkpoint_shape(checkpoint: &RuntimeCheckpointFixture) -> Result<()> {
    if checkpoint.schema != NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        && checkpoint.strategic_missiles.is_some()
    {
        bail!("checkpoint-v1 through v11 cannot contain strategicMissiles");
    }
    if !matches!(
        checkpoint.schema.as_str(),
        NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) && checkpoint.material_logistics.is_some()
    {
        bail!("checkpoint-v1 through v10 cannot contain materialLogistics");
    }
    if !matches!(
        checkpoint.schema.as_str(),
        NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
            | NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
    ) && (checkpoint.gameplay_rng.is_some() || checkpoint.personnel_reserves.is_some())
    {
        bail!("checkpoint-v1 through v8 cannot contain checkpoint-v9 replay state");
    }
    if checkpoint.schema != NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA
        && checkpoint.schema != NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA
        && checkpoint.schema != NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        && checkpoint.reinforcement.is_some()
    {
        bail!("checkpoint-v1 through v9 cannot contain checkpoint-v10 reinforcement state");
    }
    match checkpoint.schema.as_str() {
        NATIVE_RUNTIME_CHECKPOINT_SCHEMA => {
            if checkpoint.checkpoint_boundary == CheckpointBoundary::MidWar
                || checkpoint.territory.is_some()
                || checkpoint.battlefield.is_some()
                || checkpoint.influence_runtime.is_some()
                || !checkpoint.casualties_by_victim.is_empty()
                || checkpoint.planner.is_some()
                || checkpoint.side_dynamics.is_some()
                || checkpoint.operational_ai.is_some()
                || checkpoint.operational_execution.is_some()
                || checkpoint.air_power.is_some()
                || checkpoint.naval_planning.is_some()
            {
                bail!("checkpoint-v1 cannot contain checkpoint-v2 live state");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA => {
            if checkpoint.influence_runtime.is_some()
                || checkpoint.side_dynamics.is_some()
                || checkpoint.operational_ai.is_some()
                || checkpoint.operational_execution.is_some()
                || checkpoint.air_power.is_some()
                || checkpoint.naval_planning.is_some()
            {
                bail!("checkpoint-v2 cannot contain checkpoint-v3/v4/v5/v6/v7 runtime state");
            }
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
            {
                bail!(
                    "checkpoint-v2 requires boundary midWar, immutable baseline geography, and live territory"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v2 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_some()
                || checkpoint.operational_ai.is_some()
                || checkpoint.operational_execution.is_some()
                || checkpoint.air_power.is_some()
                || checkpoint.naval_planning.is_some()
            {
                bail!(
                    "checkpoint-v3 requires boundary midWar, immutable baseline geography, live territory, and influence runtime state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v3 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_some()
                || checkpoint.operational_execution.is_some()
                || checkpoint.air_power.is_some()
                || checkpoint.naval_planning.is_some()
            {
                bail!(
                    "checkpoint-v4 requires its complete live state and forbids v5/v6/v7 operational state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v4 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_some()
                || checkpoint.air_power.is_some()
                || checkpoint.naval_planning.is_some()
            {
                bail!(
                    "checkpoint-v5 requires all live runtime state and operationalAi, and forbids v6/v7 execution state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v5 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_some()
            {
                bail!(
                    "checkpoint-v6 requires all live runtime, operational execution, and air power state, and forbids v7 naval planning state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v6 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
            {
                bail!(
                    "checkpoint-v7 requires all live runtime, execution, air power, and naval planning state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v7 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
            {
                bail!(
                    "checkpoint-v8 requires all live runtime, execution, air power, naval planning, and operational-feedback state"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v8 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
            {
                bail!(
                    "checkpoint-v9 requires all live runtime, operational-feedback, and replay state"
                );
            }
            let gameplay_rng = checkpoint
                .gameplay_rng
                .as_ref()
                .context("checkpoint-v9 requires gameplayRng")?;
            if gameplay_rng.schema != GAMEPLAY_RNG_SCHEMA_VERSION
                || gameplay_rng.algorithm != GAMEPLAY_RNG_ALGORITHM
            {
                bail!("checkpoint-v9 gameplay RNG schema or algorithm is unsupported");
            }
            let reserves = checkpoint
                .personnel_reserves
                .as_ref()
                .context("checkpoint-v9 requires personnelReserves")?;
            if reserves.len() != checkpoint.sides.len()
                || reserves
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0)
            {
                bail!(
                    "checkpoint-v9 personnelReserves must exactly cover sides with finite non-negative values"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v9 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
            {
                bail!("checkpoint-v10 requires all live runtime, replay, and reinforcement state");
            }
            let gameplay_rng = checkpoint
                .gameplay_rng
                .as_ref()
                .context("checkpoint-v10 requires gameplayRng")?;
            if gameplay_rng.schema != GAMEPLAY_RNG_SCHEMA_VERSION
                || gameplay_rng.algorithm != GAMEPLAY_RNG_ALGORITHM
            {
                bail!("checkpoint-v10 gameplay RNG schema or algorithm is unsupported");
            }
            let reserves = checkpoint
                .personnel_reserves
                .as_ref()
                .context("checkpoint-v10 requires personnelReserves")?;
            if reserves.len() != checkpoint.sides.len()
                || reserves
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0)
            {
                bail!(
                    "checkpoint-v10 personnelReserves must exactly cover sides with finite non-negative values"
                );
            }
            checkpoint
                .reinforcement
                .as_ref()
                .context("checkpoint-v10 requires reinforcement")?;
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v10 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
                || checkpoint.gameplay_rng.is_none()
                || checkpoint.personnel_reserves.is_none()
                || checkpoint.reinforcement.is_none()
                || checkpoint.material_logistics.is_none()
            {
                bail!("checkpoint-v11 requires complete v10 state plus materialLogistics");
            }
            let gameplay_rng = checkpoint
                .gameplay_rng
                .as_ref()
                .expect("presence was checked above");
            if gameplay_rng.schema != GAMEPLAY_RNG_SCHEMA_VERSION
                || gameplay_rng.algorithm != GAMEPLAY_RNG_ALGORITHM
            {
                bail!("checkpoint-v11 gameplay RNG schema or algorithm is unsupported");
            }
            let reserves = checkpoint
                .personnel_reserves
                .as_ref()
                .expect("presence was checked above");
            if reserves.len() != checkpoint.sides.len()
                || reserves
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0)
            {
                bail!(
                    "checkpoint-v11 personnelReserves must exactly cover sides with finite non-negative values"
                );
            }
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v11 strategicCycle must leave room for a later cycle");
            }
        }
        NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA => {
            if checkpoint.checkpoint_boundary != CheckpointBoundary::MidWar
                || checkpoint.territory.is_none()
                || checkpoint.geography.is_none()
                || checkpoint.influence_runtime.is_none()
                || checkpoint.side_dynamics.is_none()
                || checkpoint.battlefield.is_none()
                || checkpoint.operational_ai.is_none()
                || checkpoint.operational_execution.is_none()
                || checkpoint.air_power.is_none()
                || checkpoint.naval_planning.is_none()
                || checkpoint.gameplay_rng.is_none()
                || checkpoint.personnel_reserves.is_none()
                || checkpoint.reinforcement.is_none()
                || checkpoint.material_logistics.is_none()
                || checkpoint.strategic_missiles.is_none()
            {
                bail!("checkpoint-v12 requires complete v11 state plus strategicMissiles");
            }
            let gameplay_rng = checkpoint
                .gameplay_rng
                .as_ref()
                .expect("presence checked above");
            if gameplay_rng.schema != GAMEPLAY_RNG_SCHEMA_VERSION
                || gameplay_rng.algorithm != GAMEPLAY_RNG_ALGORITHM
            {
                bail!("checkpoint-v12 gameplay RNG schema or algorithm is unsupported");
            }
            let reserves = checkpoint
                .personnel_reserves
                .as_ref()
                .expect("presence checked above");
            if reserves.len() != checkpoint.sides.len()
                || reserves
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0)
            {
                bail!(
                    "checkpoint-v12 personnelReserves must exactly cover sides with finite non-negative values"
                );
            }
            checkpoint
                .strategic_missiles
                .as_ref()
                .expect("presence checked above")
                .validate(checkpoint.sides.len())
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if checkpoint.strategic_cycle == u64::MAX {
                bail!("checkpoint-v12 strategicCycle must leave room for a later cycle");
            }
        }
        _ => bail!(
            "unsupported native runtime checkpoint schema {:?}; expected {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, or {:?}",
            checkpoint.schema,
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA
        ),
    }
    if checkpoint.steps == 0 {
        bail!("native runtime checkpoint must request at least one step");
    }
    if checkpoint.sides.is_empty() || checkpoint.sides.len() > u16::MAX as usize + 1 {
        bail!("native runtime checkpoint must contain a bounded, non-empty side list");
    }
    if checkpoint.scenario.sha256.len() != 64
        || !checkpoint
            .scenario
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("scenario.sha256 must be a 64-character hexadecimal SHA-256 digest");
    }
    if !checkpoint.scenario.grid_res.is_finite() || checkpoint.scenario.grid_res <= 0.0 {
        bail!("scenario.gridRes must be finite and positive");
    }
    let side_count = checkpoint.sides.len();
    if checkpoint.hostility_matrix.len() != side_count.saturating_mul(side_count) {
        bail!("hostilityMatrix must be a directed sideCount by sideCount matrix");
    }
    for left in 0..side_count {
        for right in 0..side_count {
            let value = checkpoint.hostility_matrix[left * side_count + right];
            if value > 1 || (left == right && value != 0) {
                bail!("hostilityMatrix must be binary with a zero diagonal");
            }
        }
    }
    let mut active = BTreeSet::new();
    for &side in &checkpoint.active_sides {
        if usize::from(side) >= side_count || !active.insert(side) {
            bail!("activeSides must be unique and reference declared sides");
        }
    }
    if checkpoint.active_sides.is_empty() {
        bail!("activeSides must explicitly name at least one active side");
    }
    validate_geography_boundary(
        checkpoint.checkpoint_boundary,
        checkpoint.geography.as_ref(),
    )?;
    if let Some(territory) = checkpoint.territory.as_ref() {
        validate_territory_markers(territory)?;
    }
    if let Some(dynamics) = checkpoint.side_dynamics.as_ref() {
        let territory = checkpoint
            .territory
            .as_ref()
            .context("sideDynamics requires live territory")?;
        let controlled_cell_limit =
            territory
                .maps
                .land_runs
                .iter()
                .try_fold(0_u64, |total, (length, _)| {
                    total
                        .checked_add(*length)
                        .context("territory land run lengths overflow u64")
                })?;
        if controlled_cell_limit == 0 {
            bail!("sideDynamics requires a non-empty territory grid");
        }
        validate_side_dynamics(
            dynamics,
            side_count,
            checkpoint.frame,
            controlled_cell_limit,
        )?;
    }
    if let Some(planner) = checkpoint.planner.as_ref() {
        let unit_ids = checkpoint
            .units
            .iter()
            .map(|unit| unit.id)
            .collect::<BTreeSet<_>>();
        validate_planner_fixture(planner, checkpoint.tick, side_count, &unit_ids)?;
    }
    if let Some(material_logistics) = checkpoint.material_logistics.as_ref() {
        let (country_to_side, _) = topology(checkpoint)?;
        let units = checkpoint
            .units
            .iter()
            .map(RuntimeUnitFixture::simulation_unit)
            .collect::<Vec<_>>();
        material_logistics
            .validate(&units, &country_to_side, side_count)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    Ok(())
}

fn validate_side_dynamics(
    dynamics: &SideDynamicsFixture,
    side_count: usize,
    checkpoint_frame: u64,
    controlled_cell_limit: u64,
) -> Result<()> {
    if dynamics.schema != NATIVE_SIDE_DYNAMICS_SCHEMA {
        bail!("unsupported side dynamics schema {:?}", dynamics.schema);
    }
    if dynamics.sides.len() != side_count {
        bail!("sideDynamics.sides must exactly cover declared sides");
    }
    for (expected, side) in dynamics.sides.iter().enumerate() {
        if usize::from(side.side_index) != expected {
            bail!("sideDynamics sides must be contiguous and ordered by sideIndex");
        }
        for value in [side.initial_personnel, side.personnel] {
            if !value.is_finite() || value < 0.0 {
                bail!("sideDynamics personnel must be finite and non-negative");
            }
        }
        if side.posture_override == Some(SideDynamicsPostureFixture::Balanced) {
            bail!("sideDynamics postureOverride must be OFFENSIVE, DEFENSIVE, or null");
        }
        if side.momentum_history.len() > 10 {
            bail!("sideDynamics momentumHistory must contain at most 10 samples");
        }
        let mut previous = None;
        for sample in &side.momentum_history {
            if sample.frame > checkpoint_frame || previous.is_some_and(|frame| sample.frame < frame)
            {
                bail!(
                    "sideDynamics momentumHistory frames must be nondecreasing and within checkpoint frame"
                );
            }
            if sample.controlled > controlled_cell_limit {
                bail!("sideDynamics controlled cells exceed the territory grid");
            }
            previous = Some(sample.frame);
        }
    }
    Ok(())
}

fn decode_side_dynamics(
    dynamics: &SideDynamicsFixture,
) -> Result<BTreeMap<usize, mw_core::SideDynamics>> {
    let mut result = BTreeMap::new();
    for side in &dynamics.sides {
        let phase = match side.war_phase {
            SideDynamicsWarPhaseFixture::Advancing => mw_core::WarPhase::Advancing,
            SideDynamicsWarPhaseFixture::Stalemate => mw_core::WarPhase::Stalemate,
            SideDynamicsWarPhaseFixture::Retreating => mw_core::WarPhase::Retreating,
            SideDynamicsWarPhaseFixture::Collapsing => mw_core::WarPhase::Collapsing,
        };
        let posture = match side.posture {
            SideDynamicsPostureFixture::Offensive => mw_core::WarPosture::Offensive,
            SideDynamicsPostureFixture::Balanced => mw_core::WarPosture::Balanced,
            SideDynamicsPostureFixture::Defensive => mw_core::WarPosture::Defensive,
        };
        let mut state =
            mw_core::SideDynamics::bootstrap(usize::from(side.side_index), side.initial_personnel);
        state.current_personnel = side.personnel;
        state.phase = phase;
        state.posture = posture;
        state.posture_override = side.posture_override.map(|posture| match posture {
            SideDynamicsPostureFixture::Offensive => mw_core::WarPosture::Offensive,
            SideDynamicsPostureFixture::Balanced => mw_core::WarPosture::Balanced,
            SideDynamicsPostureFixture::Defensive => mw_core::WarPosture::Defensive,
        });
        for sample in &side.momentum_history {
            state
                .momentum_samples
                .push_back(mw_core::dynamics::MomentumSample {
                    frame: sample.frame,
                    controlled: sample.controlled,
                });
        }
        result.insert(usize::from(side.side_index), state);
    }
    Ok(result)
}

fn validate_territory_markers(territory: &TerritoryV2Fixture) -> Result<()> {
    let census = territory.committed_census;
    if census.generation == 0
        || census.generation == u64::MAX
        || census.commit_sequence == 0
        || census.processed_tiles == 0
        || census.processed_items == 0
    {
        bail!("midWar territory must contain a completed, advancing census commit");
    }
    Ok(())
}

fn validate_geography_boundary(
    boundary: CheckpointBoundary,
    geography: Option<&GeographyFixture>,
) -> Result<()> {
    if boundary == CheckpointBoundary::PostStartWar && geography.is_none() {
        bail!("postStartWar checkpoint boundary requires exact geography");
    }
    if boundary == CheckpointBoundary::MidWar && geography.is_none() {
        bail!("midWar checkpoint boundary requires immutable baseline geography");
    }
    Ok(())
}

fn topology(checkpoint: &RuntimeCheckpointFixture) -> Result<RuntimeTopology> {
    let mut country_to_side = BTreeMap::new();
    let mut coalitions = vec![BTreeSet::new(); checkpoint.sides.len()];
    for (expected, side) in checkpoint.sides.iter().enumerate() {
        if usize::from(side.side_index) != expected || side.country_ids.is_empty() {
            bail!("sides must be contiguous, ordered by sideIndex, and non-empty");
        }
        for &country_id in &side.country_ids {
            if country_id == 0 || country_to_side.insert(country_id, expected).is_some() {
                bail!("country id {country_id} is zero or appears in multiple sides");
            }
            coalitions[expected].insert(country_id);
        }
    }
    Ok((country_to_side, coalitions))
}

fn validate_checkpoint_against_scenario(
    checkpoint: &RuntimeCheckpointFixture,
    production: &ScenarioProduction,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    let countries = production
        .countries
        .iter()
        .map(|country| (country.country_id, country))
        .collect::<BTreeMap<_, _>>();
    for &country_id in country_to_side.keys() {
        let country = countries
            .get(&country_id)
            .with_context(|| format!("checkpoint country {country_id} is absent from scenario"))?;
        if country.initial_core_cells == 0 {
            bail!("checkpoint country {country_id} has no active scenario core cells");
        }
    }

    let mut economy_ids = BTreeSet::new();
    for economy in &checkpoint.economies {
        if !economy_ids.insert(economy.country_id)
            || !country_to_side.contains_key(&economy.country_id)
        {
            bail!("economies must contain one unique state per declared country");
        }
        let scenario_country = countries[&economy.country_id];
        if economy.initial_core_cells != scenario_country.initial_core_cells
            || economy.initial_city_population.to_bits()
                != scenario_country.initial_city_population.to_bits()
        {
            bail!(
                "economy {} immutable scenario baselines do not match the loaded scenario",
                economy.country_id
            );
        }
        validate_economy_fixture(economy)?;
    }
    if economy_ids != country_to_side.keys().copied().collect() {
        bail!("economies must exactly cover every declared country");
    }

    let mut unit_ids = BTreeSet::new();
    for unit in &checkpoint.units {
        if unit.id == 0 || !unit_ids.insert(unit.id) {
            bail!("unit ids must be positive and unique");
        }
        let expected_side = country_to_side
            .get(&unit.country_id)
            .with_context(|| format!("unit {} has an undeclared country", unit.id))?;
        if *expected_side != usize::from(unit.side) {
            bail!("unit {} side and sovereign coalition disagree", unit.id);
        }
        if usize::from(unit.side) >= checkpoint.sides.len() {
            bail!("unit {} references an out-of-range side", unit.id);
        }
        validate_unit_fixture(unit)?;
        if let Some(policy) = &unit.influence_policy {
            validate_influence_references(unit.id, policy, country_to_side)?;
        }
    }

    let mut occupation_victims = BTreeSet::new();
    for occupation in &checkpoint.occupations {
        if !occupation_victims.insert(occupation.victim_id)
            || occupation.victim_id == occupation.annexer_id
            || !country_to_side.contains_key(&occupation.victim_id)
            || !country_to_side.contains_key(&occupation.annexer_id)
            || occupation.queued_at_cycle > checkpoint.strategic_cycle
        {
            bail!(
                "occupation records must be unique, stay inside declared coalitions, and not be queued in a future strategic cycle"
            );
        }
    }
    for (&country_id, casualties) in &checkpoint.casualties {
        if !country_to_side.contains_key(&country_id)
            || !casualties.is_finite()
            || *casualties < 0.0
        {
            bail!("casualty ledgers must be finite, non-negative, and use declared countries");
        }
    }
    validate_checkpoint_boundary(checkpoint, production, country_to_side)?;
    Ok(())
}

fn validate_checkpoint_boundary(
    checkpoint: &RuntimeCheckpointFixture,
    production: &ScenarioProduction,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    if checkpoint.checkpoint_boundary == CheckpointBoundary::BaselineReplay {
        return Ok(());
    }
    if checkpoint.checkpoint_boundary == CheckpointBoundary::MidWar {
        let declared = country_to_side.keys().copied().collect::<BTreeSet<_>>();
        if checkpoint
            .casualties
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            != declared
        {
            bail!("midWar casualties must exactly cover every declared country");
        }
        if checkpoint
            .casualties_by_victim
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            != declared
        {
            bail!("midWar casualtiesByVictim must exactly cover every declared country");
        }
        for (&victim, attackers) in &checkpoint.casualties_by_victim {
            if !declared.contains(&victim) {
                bail!("casualtiesByVictim references undeclared victim {victim}");
            }
            for (&attacker, value) in attackers {
                if attacker == victim
                    || !declared.contains(&attacker)
                    || !value.is_finite()
                    || *value < 0.0
                {
                    bail!(
                        "casualtiesByVictim entries must use distinct declared countries and finite non-negative values"
                    );
                }
            }
        }
        let active_from_economy = checkpoint
            .economies
            .iter()
            .filter(|economy| !economy.capitulated)
            .filter_map(|economy| country_to_side.get(&economy.country_id).copied())
            .map(|side| side as u16)
            .collect::<BTreeSet<_>>();
        if checkpoint
            .active_sides
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != active_from_economy
        {
            bail!("midWar activeSides must exactly match non-capitulated country economies");
        }
        return Ok(());
    }
    if checkpoint.tick != 0 || checkpoint.frame != 0 || checkpoint.strategic_cycle != 0 {
        bail!("postStartWar checkpoint boundary requires tick=0, frame=0, and strategicCycle=0");
    }
    if !checkpoint.occupations.is_empty() {
        bail!("postStartWar checkpoint boundary cannot contain occupations");
    }
    let declared = country_to_side.keys().copied().collect::<BTreeSet<_>>();
    if checkpoint
        .casualties
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != declared
        || checkpoint.casualties.values().any(|value| *value != 0.0)
    {
        bail!(
            "postStartWar checkpoint boundary requires an explicit zero casualty total for every declared country"
        );
    }

    let derived = production
        .economy_states
        .iter()
        .map(|state| (state.country_id, state))
        .collect::<BTreeMap<_, _>>();
    for economy in &checkpoint.economies {
        let expected = derived.get(&economy.country_id).with_context(|| {
            format!(
                "country {} has no derived starting economy",
                economy.country_id
            )
        })?;
        let mandatory_payroll = checkpoint
            .units
            .iter()
            .filter(|unit| unit.country_id == economy.country_id && unit.health > 0.0)
            .map(|unit| match unit.kind {
                UnitKindFixture::Armor => unit.equipment as f64 / 100.0 * ARMOR_PAYROLL_PER_100,
                UnitKindFixture::Army => {
                    unit.personnel_capacity as f64 / PERSONNEL_PER_FORMATION * PAYROLL_PER_UNIT
                }
            })
            .sum::<f64>();
        let expected_base_income = expected
            .base_income
            .max(mandatory_payroll / TARGET_STARTING_PAYROLL_SHARE);
        let expected_treasury = expected_base_income * STARTING_RESERVE_CYCLES;
        let starting_numeric_matches =
            approximately_equal(economy.economic_strength, expected.economic_strength)
                && approximately_equal(economy.base_income, expected_base_income)
                && approximately_equal(economy.income, expected_base_income)
                && approximately_equal(economy.treasury, expected_treasury)
                && economy.occupation_yield == 0.0
                && economy.payroll_due == 0.0
                && economy.occupation_due == 0.0
                && economy.payroll_coverage == 1.0
                && economy.occupation_coverage == 1.0
                && economy.arrears_cycles == 0.0
                && economy.core_control_ratio == 1.0
                && economy.city_control_ratio == 1.0;
        let starting_state_matches = economy.command_band == CommandBand::Paid
            && economy.mutiny_recovery_cycles == 0
            && economy.capital_held
            && economy.last_event_band == CommandBand::Paid
            && !economy.capitulated;
        if !starting_numeric_matches || !starting_state_matches {
            bail!(
                "postStartWar checkpoint boundary economy {} is not a pristine deployment-adjusted starting state",
                economy.country_id
            );
        }
    }
    Ok(())
}

fn approximately_equal(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= expected.abs().max(1.0) * 1e-12
}

fn validate_economy_fixture(economy: &EconomyFixture) -> Result<()> {
    let values = [
        economy.economic_strength,
        economy.base_income,
        economy.treasury,
        economy.income,
        economy.occupation_yield,
        economy.payroll_due,
        economy.occupation_due,
        economy.payroll_coverage,
        economy.occupation_coverage,
        economy.arrears_cycles,
        economy.initial_city_population,
        economy.core_control_ratio,
        economy.city_control_ratio,
    ];
    if economy.country_id == 0
        || economy.initial_core_cells == 0
        || values.iter().any(|value| !value.is_finite())
        || economy.economic_strength < 0.0
        || economy.base_income < 0.0
        || economy.treasury < 0.0
        || economy.payroll_due < 0.0
        || economy.occupation_due < 0.0
        || economy.arrears_cycles < 0.0
        || !(0.0..=1.0).contains(&economy.payroll_coverage)
        || !(0.0..=1.0).contains(&economy.occupation_coverage)
        || !(0.0..=1.0).contains(&economy.core_control_ratio)
        || !(0.0..=1.0).contains(&economy.city_control_ratio)
    {
        bail!(
            "economy {} contains invalid live numeric state",
            economy.country_id
        );
    }
    Ok(())
}

fn validate_unit_fixture(unit: &RuntimeUnitFixture) -> Result<()> {
    let ai = unit.ai_policy;
    let numeric = [
        unit.lat,
        unit.lng,
        unit.health,
        unit.max_health,
        unit.quality,
        unit.dir_lat,
        unit.dir_lng,
        unit.ally_weight,
        ai.base_speed,
        ai.terrain_speed_multiplier,
        ai.speed_multiplier,
        ai.plan_speed_multiplier,
        ai.neutral_penalty,
        ai.push_readiness,
        ai.dealt_multiplier,
        ai.taken_multiplier,
        ai.defense_bonus,
        ai.long_war_defense,
    ];
    if numeric.iter().any(|value| !value.is_finite())
        || unit.health < 0.0
        || unit.max_health <= 0.0
        || unit.health > unit.max_health
        || unit.personnel > unit.personnel_capacity
        || unit.equipment > unit.max_equipment
        || ai.base_speed < 0.0
        || ai.terrain_speed_multiplier < 0.0
        || ai.speed_multiplier < 0.0
        || ai.plan_speed_multiplier < 0.0
        || ai.neutral_penalty < 0.0
        || ai.push_readiness < 0.0
        || ai.dealt_multiplier < 0.0
        || ai.taken_multiplier < 0.0
        || ai.defense_bonus < 0.0
        || ai.long_war_defense < 0.0
    {
        bail!("unit {} contains invalid live or AI numeric state", unit.id);
    }
    if let Some(policy) = &unit.influence_policy
        && (![policy.radius, policy.delta, policy.concentration_bonus]
            .into_iter()
            .all(f64::is_finite)
            || policy.temporal_seed.is_some_and(|value| !value.is_finite())
            || policy.radius <= 0.0
            || policy.delta < 0.0
            || policy.concentration_bonus < 0.0)
    {
        bail!("unit {} contains an invalid influence policy", unit.id);
    }
    Ok(())
}

fn validate_influence_references(
    unit_id: u64,
    policy: &InfluencePolicyFixture,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<()> {
    for country in policy
        .beneficiary_country_id
        .iter()
        .chain(policy.protected_owner_ids.iter())
        .chain(policy.rebel_de_jure.iter())
        .chain(policy.credit_de_jure.iter())
        .chain(policy.credit_de_jure_by_country.keys())
        .chain(policy.credit_de_jure_by_country.values())
    {
        if *country == 0 || !country_to_side.contains_key(country) {
            bail!("unit {unit_id} influence policy references undeclared country {country}");
        }
    }
    Ok(())
}

impl RuntimeUnitFixture {
    fn simulation_unit(&self) -> SimulationUnit {
        SimulationUnit {
            combat: CombatUnit {
                id: self.id,
                side: u64::from(self.side),
                sovereign: u64::from(self.country_id),
                kind: self.kind.into(),
                lat: self.lat,
                lng: self.lng,
                health: self.health,
                max_health: self.max_health,
                personnel: self.personnel,
                personnel_capacity: self.personnel_capacity,
                equipment: self.equipment,
                max_equipment: self.max_equipment,
                quality: self.quality,
                transport: self.transport,
                armor_supported: self.armor_supported,
                landing_penalty_active: self.landing_penalty_active,
                at_sea: self.at_sea,
                last_combat_tick: self.last_combat_tick,
                victory_boost_ticks: self.victory_boost_ticks,
            },
            dir_lat: self.dir_lat,
            dir_lng: self.dir_lng,
            coast_stuck_ticks: self.coast_stuck_ticks,
            armor_landing_penalty_until_tick: self.armor_landing_penalty_until_tick,
            is_support: self.is_support,
            ally_weight: self.ally_weight,
        }
    }

    fn runtime_policy(
        &self,
        coalition: &BTreeSet<u16>,
        economy_band: CommandBand,
        strategic_cycle: u64,
    ) -> RuntimeUnitPolicy {
        let ai = self.ai_policy;
        let synthesized_command = self.command_policy.is_none();
        let command = self.command_policy.map_or_else(
            || {
                let share = command_refusal_share(economy_band);
                let seed = self
                    .influence_policy
                    .as_ref()
                    .and_then(|policy| policy.temporal_seed)
                    .unwrap_or(self.id as f64);
                let discipline = browser_discipline(seed, self.country_id);
                let refuses_offense = discipline < share;
                UnitCommandPolicy {
                    band: economy_band,
                    discipline,
                    refuses_offense,
                    return_home: matches!(
                        economy_band,
                        CommandBand::Breakdown | CommandBand::Mutiny
                    ),
                    self_defense_only: economy_band == CommandBand::Mutiny,
                    home_target: None,
                    transition_cycle: strategic_cycle,
                }
            },
            |command| UnitCommandPolicy {
                band: command.band,
                discipline: command.discipline,
                refuses_offense: command.refuses_offense,
                return_home: command.return_home,
                self_defense_only: command.self_defense_only,
                home_target: command.home_target.map(|target| CommandHomeTarget {
                    cell: target.cell,
                    lat: target.lat,
                    lng: target.lng,
                }),
                transition_cycle: command.transition_cycle,
            },
        );
        RuntimeUnitPolicy {
            unit_id: self.id,
            ai: UnitAiPolicy {
                base_speed: ai.base_speed,
                movement: ResolvedMovementModifiers {
                    terrain_speed_multiplier: ai.terrain_speed_multiplier,
                    speed_multiplier: ai.speed_multiplier,
                    plan_speed_multiplier: ai.plan_speed_multiplier,
                    neutral_penalty: ai.neutral_penalty,
                    push_readiness: ai.push_readiness,
                },
                combat: ResolvedCombatModifiers {
                    dealt_multiplier: ai.dealt_multiplier,
                    taken_multiplier: ai.taken_multiplier,
                    defense_bonus: ai.defense_bonus,
                    long_war_defense: ai.long_war_defense,
                    mountain: ai.mountain,
                    urban: ai.urban,
                    current_cell_mountain: None,
                    current_cell_urban: None,
                },
                is_reserve: ai.is_reserve,
                reinforcement_eligible: ai.reinforcement_eligible,
                encircled: ai.encircled,
                deploy_until_tick: ai.deploy_until_tick,
                garrison_excluded: ai.garrison_excluded,
            },
            command,
            influence: self.influence_policy.as_ref().map(|policy| {
                let mut influence = UnitInfluencePolicy {
                    radius: policy.radius,
                    delta: policy.delta,
                    concentration_bonus: policy.concentration_bonus,
                    beneficiary: policy.beneficiary_country_id,
                    // Coalition membership is a topology invariant rather than
                    // a duplicated, unit-local checkpoint assertion.
                    owner_ally_country_ids: coalition.clone(),
                    protected_owner_ids: policy.protected_owner_ids.iter().copied().collect(),
                    rebel_de_jure: policy.rebel_de_jure,
                    credit_de_jure: policy.credit_de_jure,
                    credit_de_jure_by_country: policy.credit_de_jure_by_country.clone(),
                    refuses_offense: policy.refuses_offense,
                    browser_temporal_seed: policy.temporal_seed,
                };
                if synthesized_command {
                    influence.refuses_offense = command.refuses_offense;
                }
                influence
            }),
        }
    }
}

fn build_runtime(prepared: &PreparedRuntime) -> Result<NativeRuntime> {
    let checkpoint = &prepared.checkpoint;
    let objectives = checkpoint
        .planner
        .as_ref()
        .map(|planner| {
            planner
                .objectives
                .iter()
                .map(FrontObjective::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let prior_objective_by_unit = checkpoint
        .planner
        .as_ref()
        .map(|planner| planner.prior_objective_by_unit.clone())
        .unwrap_or_default();
    let front_prior_by_unit = checkpoint
        .planner
        .as_ref()
        .map(|planner| {
            planner
                .front_prior_by_unit
                .iter()
                .map(|prior| (prior.unit_id, FrontLayoutPrior::from(prior)))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let last_front_refresh_tick = checkpoint
        .planner
        .as_ref()
        .and_then(|planner| planner.last_front_refresh_tick);
    let units = checkpoint
        .units
        .iter()
        .map(RuntimeUnitFixture::simulation_unit)
        .collect::<Vec<_>>();
    let simulation = Simulation::new(
        SimulationConfig {
            tactical_cell_size: SimulationConfig::default().tactical_cell_size,
            combat: CombatConfig::default(),
        },
        units,
    )?;
    let economy_bands = checkpoint
        .economies
        .iter()
        .map(|economy| (economy.country_id, economy.command_band))
        .collect::<BTreeMap<_, _>>();
    let unit_policies = checkpoint
        .units
        .iter()
        .map(|unit| {
            unit.runtime_policy(
                prepared
                    .coalition_by_side
                    .get(usize::from(unit.side))
                    .expect("checkpoint side validated"),
                economy_bands
                    .get(&unit.country_id)
                    .copied()
                    .unwrap_or(CommandBand::Paid),
                checkpoint.strategic_cycle,
            )
        })
        .collect::<Vec<_>>();

    let territory = build_territory(prepared)?;
    let economies = checkpoint
        .economies
        .iter()
        .map(EconomyState::from)
        .collect::<Vec<_>>();
    if economies.len() != prepared.country_to_side.len() {
        bail!("scenario did not derive an economy for every checkpoint country");
    }
    let occupations = checkpoint
        .occupations
        .iter()
        .map(OccupationState::from)
        .collect::<Vec<_>>();
    let strategic =
        StrategicSimulation::restore(checkpoint.strategic_cycle, economies, occupations)?;
    let runtime_checkpoint = RuntimeCheckpoint {
        tick: checkpoint.tick,
        frame: checkpoint.frame,
        war_grace_end: checkpoint.war_grace_end,
        simulation,
        territory,
        strategic,
        scenario: prepared.production.clone(),
        diplomacy: RuntimeDiplomacy {
            hostility: checkpoint.hostility_matrix.clone(),
            active_sides: checkpoint.active_sides.clone(),
        },
        unit_policies,
        battlefield: prepared.battlefield.clone(),
        objectives,
        prior_objective_by_unit,
        front_prior_by_unit,
        last_front_refresh_tick,
        casualties: checkpoint.casualties.clone(),
        casualties_by_victim: checkpoint.casualties_by_victim.clone(),
        gameplay_rng: checkpoint.gameplay_rng.as_ref().map_or(
            GameplayRngState {
                state: DEFAULT_GAMEPLAY_RNG_SEED,
            },
            |rng| GameplayRngState { state: rng.state },
        ),
        personnel_reserves: checkpoint.personnel_reserves.as_ref().map_or_else(
            || {
                (0..checkpoint.sides.len())
                    .map(|side| (side, 0.0))
                    .collect()
            },
            |reserves| reserves.iter().copied().enumerate().collect(),
        ),
        side_dynamics: prepared.side_dynamics.clone(),
        operations: prepared.operational_ai.clone(),
        operational_execution: prepared.operational_execution.clone(),
        air_power: prepared.air_power.clone(),
        naval_planning: prepared.naval_planning.clone(),
        reinforcement: checkpoint.reinforcement.clone(),
        material_logistics: checkpoint.material_logistics.clone(),
        strategic_missiles: checkpoint.strategic_missiles.clone(),
    };
    Ok(NativeRuntime::new(
        RuntimeConfig::default(),
        runtime_checkpoint,
    )?)
}

fn build_territory(prepared: &PreparedRuntime) -> Result<TerritoryControl> {
    let decoded = &prepared.decoded;
    let cell_count = decoded.target.cell_count()?;
    let side_count = prepared.checkpoint.sides.len();
    let (maps, revisions, committed) = if let Some(maps) = prepared.live_territory_maps.as_ref() {
        let territory = prepared
            .checkpoint
            .territory
            .as_ref()
            .context("live territory maps are missing their checkpoint markers")?;
        (
            maps.clone(),
            territory.revisions,
            Some(TerritoryCommittedState {
                generation: territory.committed_census.generation,
                commit_sequence: territory.committed_census.commit_sequence,
                mutation_sequence: territory.committed_census.mutation_sequence,
                processed_tiles: territory.committed_census.processed_tiles,
                processed_items: territory.committed_census.processed_items,
            }),
        )
    } else {
        let mut land = vec![0_u8; cell_count];
        let mut primary_occupier = vec![0_u16; cell_count];
        let mut dominant_side = vec![-1_i16; cell_count];
        let mut occupation = vec![0.0_f32; cell_count];
        let mut side_influence = (0..side_count)
            .map(|_| vec![0.0_f32; cell_count])
            .collect::<Vec<_>>();

        for cell in 0..cell_count {
            if decoded.land[cell] == 0 {
                continue;
            }
            let owner = decoded.world_control[cell];
            let Some(&side) = prepared.country_to_side.get(&owner) else {
                // Non-theater land remains traversable, but is excluded from the
                // active census and cannot fabricate a conflict or controller.
                land[cell] = 1;
                continue;
            };
            land[cell] = 2;
            primary_occupier[cell] = owner;
            dominant_side[cell] = i16::try_from(side).context("side exceeds territory width")?;
            side_influence[side][cell] = 1.0;
            occupation[cell] = if side.is_multiple_of(2) { 1.0 } else { -1.0 };
        }
        (
            TerritoryMaps {
                land,
                world_control: decoded.world_control.clone(),
                de_jure: decoded.de_jure.clone(),
                primary_occupier,
                dominant_side,
                occupation,
                side_influence,
            },
            TerritoryRevisionFixture {
                topology_revision: 1,
                world_revision: 1,
                city_revision: 1,
            },
            None,
        )
    };

    let cities = prepared
        .production
        .cities
        .iter()
        .map(|city| TerritoryCity {
            id: city.city_id,
            cell: city.cell,
            owner: city.owner_id,
            population: city.population,
            capital: city.capital,
        })
        .collect::<Vec<_>>();
    let protected_owner_ids = prepared.country_to_side.keys().copied().collect();
    let config = TerritoryConfig {
        width: decoded.target.width,
        height: decoded.target.height,
        grid_resolution: decoded.target.grid_res,
        max_sides: side_count,
        tile_size: TERRITORY_TILE_SIZE,
        maps,
        country_to_side: prepared.country_to_side.clone(),
        hostility_matrix: prepared.checkpoint.hostility_matrix.clone(),
        cities,
        protected_owner_ids,
        topology_revision: revisions.topology_revision,
        world_revision: revisions.world_revision,
        city_revision: revisions.city_revision,
    };
    let mut territory = if let Some(state) = committed {
        TerritoryControl::restore(config, state)?
    } else {
        TerritoryControl::new(config)?
    };
    if let Some(state) = prepared.influence_runtime.clone() {
        territory.restore_influence_runtime(state)?;
    }
    Ok(territory)
}

struct FixtureExecution {
    initial: RuntimeSnapshotReport,
    steps: Vec<RuntimeSnapshotReport>,
    render_updates_drained: usize,
}

fn execute_fixture(prepared: &PreparedRuntime, steps: usize) -> Result<FixtureExecution> {
    let mut runtime = build_runtime(prepared)?;
    let initial_snapshot = runtime.latest_snapshot();
    let initial = snapshot_report(&initial_snapshot, &prepared.country_to_side)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let mut reports = Vec::with_capacity(steps);
    for _ in 0..steps {
        if !matches!(runtime.state(), RuntimeState::Running) {
            break;
        }
        let snapshot = runtime.step()?;
        reports.push(snapshot_report(&snapshot, &prepared.country_to_side)?);
        render_updates_drained += drain_render_updates(&mut runtime);
    }
    Ok(FixtureExecution {
        initial,
        steps: reports,
        render_updates_drained,
    })
}

fn drain_render_updates(runtime: &mut NativeRuntime) -> usize {
    let mut drained = 0;
    while runtime.pop_render_update().is_some() {
        drained += 1;
    }
    drained
}

struct TimedRuntimeSample {
    elapsed_ms: f64,
    completed_steps: usize,
    render_updates_drained: usize,
    gated: bool,
    checksum: String,
}

fn execute_benchmark_fresh(prepared: &PreparedRuntime, ticks: usize) -> Result<TimedRuntimeSample> {
    let mut runtime = build_runtime(prepared)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let started = Instant::now();
    let (completed_steps, gated, drained) = execute_timed_steps(&mut runtime, ticks)?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    render_updates_drained += drained;
    let checksum = snapshot_checksum(&runtime.latest_snapshot())?;
    Ok(TimedRuntimeSample {
        elapsed_ms,
        completed_steps,
        render_updates_drained,
        gated,
        checksum,
    })
}

fn execute_timed_steps(runtime: &mut NativeRuntime, ticks: usize) -> Result<(usize, bool, usize)> {
    let mut completed = 0;
    let mut drained = 0;
    while completed < ticks {
        if !matches!(runtime.state(), RuntimeState::Running) {
            break;
        }
        black_box(runtime.step()?);
        completed += 1;
        drained += drain_render_updates(runtime);
    }
    Ok((
        completed,
        !matches!(runtime.state(), RuntimeState::Running),
        drained,
    ))
}

fn execute_persistent_benchmark(
    prepared: &PreparedRuntime,
    options: &BenchOptions,
    ticks: usize,
) -> Result<PersistentModeReport> {
    let mut runtime = build_runtime(prepared)?;
    let mut render_updates_drained = drain_render_updates(&mut runtime);
    let mut samples = Vec::with_capacity(options.repeat);
    let mut completed_samples = 0;
    let mut completed_ticks = 0;
    let mut gated = false;
    for _ in 0..options.repeat {
        let started = Instant::now();
        let (completed, sample_gated, drained) = execute_timed_steps(&mut runtime, ticks)?;
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        render_updates_drained += drained;
        completed_ticks += completed;
        if completed == ticks {
            samples.push(elapsed);
            completed_samples += 1;
        }
        if sample_gated || completed != ticks {
            gated = true;
            break;
        }
    }
    let checksum = snapshot_checksum(&runtime.latest_snapshot())?;
    let metrics = (!samples.is_empty()).then(|| {
        benchmark_mode_report(
            &samples,
            completed_samples,
            ticks,
            render_updates_drained,
            checksum.clone(),
        )
    });
    Ok(PersistentModeReport {
        available: !gated && completed_samples == options.repeat,
        gated,
        completed_samples,
        completed_ticks,
        metrics,
        final_checksum: checksum,
    })
}

fn benchmark_mode_report(
    samples: &[f64],
    repeat: usize,
    ticks: usize,
    render_updates_drained: usize,
    checksum: String,
) -> BenchmarkModeReport {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    BenchmarkModeReport {
        repeat,
        ticks_per_sample: ticks,
        median_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        median_ms_per_tick: percentile(&sorted, 0.50) / ticks as f64,
        render_updates_drained,
        checksum,
    }
}

fn percentile(samples: &[f64], percentile: f64) -> f64 {
    let index = ((samples.len() as f64 * percentile).floor() as usize).min(samples.len() - 1);
    samples[index]
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionInspectReport {
    schema: &'static str,
    scenario: ScenarioIdentityReport,
    counters: ProductionCountersReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_country: Option<SelectedCountryReport>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GridReport {
    grid_res: f64,
    width: usize,
    height: usize,
    cells: usize,
}

impl From<GridSpec> for GridReport {
    fn from(value: GridSpec) -> Self {
        Self {
            grid_res: value.grid_res,
            width: value.width,
            height: value.height,
            cells: value.width.saturating_mul(value.height),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioIdentityReport {
    sha256: String,
    name: String,
    source_grid: GridReport,
    target_grid: GridReport,
    entry_count: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionCountersReport {
    countries: usize,
    cities: usize,
    unresolved_city_owners: usize,
    economy_seeds: usize,
    land_cells: usize,
}

impl From<&ScenarioProduction> for ProductionCountersReport {
    fn from(value: &ScenarioProduction) -> Self {
        Self {
            countries: value.counters.countries,
            cities: value.counters.cities,
            unresolved_city_owners: value.counters.unresolved_city_owners,
            economy_seeds: value.counters.economy_seeds,
            land_cells: value.counters.land_cells,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectedCountryReport {
    country_id: u16,
    name: String,
    gdp: f64,
    population: f64,
    is_rebel: bool,
    initial_core_cells: u32,
    initial_owned_land_cells: u32,
    initial_city_population: f64,
    capital_cell: Option<usize>,
    expected_army_units: f64,
    economic_strength: f64,
    base_income: f64,
    starting_treasury: f64,
}

fn selected_country_report(
    selector: &str,
    production: &ScenarioProduction,
) -> Result<SelectedCountryReport> {
    let numeric = selector.parse::<u16>().ok();
    let normalized = selector.trim().to_lowercase();
    let matches = production
        .countries
        .iter()
        .filter(|country| {
            numeric == Some(country.country_id) || country.name.to_lowercase() == normalized
        })
        .collect::<Vec<_>>();
    let [country] = matches.as_slice() else {
        if matches.is_empty() {
            bail!("country selector {selector:?} did not match this scenario");
        }
        bail!("country selector {selector:?} is ambiguous");
    };
    let economy = production
        .economy_states
        .iter()
        .find(|state| state.country_id == country.country_id)
        .context("selected country has no derived economy")?;
    Ok(SelectedCountryReport {
        country_id: country.country_id,
        name: country.name.clone(),
        gdp: country.gdp,
        population: country.population,
        is_rebel: country.is_rebel,
        initial_core_cells: country.initial_core_cells,
        initial_owned_land_cells: country.initial_owned_land_cells,
        initial_city_population: country.initial_city_population,
        capital_cell: country.capital_cell,
        expected_army_units: country.expected_army_units,
        economic_strength: economy.economic_strength,
        base_income: economy.base_income,
        starting_treasury: economy.treasury,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFixtureReport {
    #[serde(flatten)]
    body: RuntimeFixtureReportBody,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFixtureReportBody {
    schema: &'static str,
    runtime_schema: &'static str,
    checkpoint_boundary: CheckpointBoundaryReport,
    scenario: ScenarioIdentityReport,
    checkpoint: CheckpointReport,
    production: RuntimeProductionReport,
    requested_steps: usize,
    completed_steps: usize,
    render_updates_drained: usize,
    initial: RuntimeSnapshotReport,
    steps: Vec<RuntimeSnapshotReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBenchReport {
    #[serde(flatten)]
    body: RuntimeBenchReportBody,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBenchReportBody {
    schema: &'static str,
    runtime_schema: &'static str,
    mode: &'static str,
    checkpoint_boundary: CheckpointBoundaryReport,
    geography: GeographyReport,
    scenario: ScenarioIdentityReport,
    units: usize,
    sides: usize,
    countries: usize,
    repeat: usize,
    warmup: usize,
    ticks_per_sample: usize,
    fresh: BenchmarkModeReport,
    persistent: PersistentModeReport,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkModeReport {
    repeat: usize,
    ticks_per_sample: usize,
    median_ms: f64,
    p95_ms: f64,
    median_ms_per_tick: f64,
    render_updates_drained: usize,
    checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistentModeReport {
    available: bool,
    gated: bool,
    completed_samples: usize,
    completed_ticks: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<BenchmarkModeReport>,
    final_checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointReport {
    checkpoint_boundary: CheckpointBoundary,
    geography: GeographyReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    territory: Option<TerritoryCheckpointReport>,
    tick: u64,
    frame: u64,
    war_grace_end: u64,
    strategic_cycle: u64,
    sides: Vec<SideFixture>,
    active_sides: Vec<u16>,
    hostility_matrix: Vec<u8>,
    units: usize,
    economies: Vec<EconomyFixture>,
    occupations: usize,
    casualties: BTreeMap<u16, f64>,
    casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gameplay_rng: Option<GameplayRngFixture>,
    #[serde(skip_serializing_if = "Option::is_none")]
    personnel_reserves: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reinforcement: Option<ReinforcementState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategic_missiles: Option<StrategicMissileState>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeographyReport {
    exact_geography_supplied: bool,
    land_runs: usize,
    world_control_runs: usize,
    de_jure_runs: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerritoryCheckpointReport {
    encoding: String,
    land_runs: usize,
    world_control_runs: usize,
    de_jure_runs: usize,
    primary_occupier_runs: usize,
    dominant_side_runs: usize,
    occupation_runs: usize,
    side_influence_rows: usize,
    side_influence_runs: usize,
    revisions: TerritoryRevisionFixture,
    committed_census: TerritoryCommittedCensusFixture,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointBoundaryReport {
    kind: CheckpointBoundary,
    resumable: bool,
    description: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeProductionReport {
    counters: ProductionCountersReport,
    theater_countries: Vec<SelectedCountryReport>,
}

impl PreparedRuntime {
    fn schema(&self) -> &'static str {
        match self.checkpoint.schema.as_str() {
            NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA => NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            _ => NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
        }
    }

    fn boundary_report(&self) -> CheckpointBoundaryReport {
        CheckpointBoundaryReport {
            kind: self.checkpoint.checkpoint_boundary,
            resumable: self.checkpoint.checkpoint_boundary.resumable(),
            description: self.checkpoint.checkpoint_boundary.description(),
        }
    }

    fn identity_report(&self) -> ScenarioIdentityReport {
        ScenarioIdentityReport {
            sha256: self.raw_sha256.clone(),
            name: self.scenario_name.clone(),
            source_grid: GridReport::from(self.decoded.source),
            target_grid: GridReport::from(self.decoded.target),
            entry_count: self.decoded.entry_count,
        }
    }

    fn geography_report(&self) -> GeographyReport {
        if let Some(geography) = self.checkpoint.geography.as_ref() {
            GeographyReport {
                exact_geography_supplied: true,
                land_runs: geography.land_runs.len(),
                world_control_runs: geography.world_control_runs.len(),
                de_jure_runs: geography.de_jure_runs.len(),
            }
        } else if let Some(territory) = self.checkpoint.territory.as_ref() {
            GeographyReport {
                exact_geography_supplied: true,
                land_runs: territory.maps.land_runs.len(),
                world_control_runs: territory.maps.world_control_runs.len(),
                de_jure_runs: territory.maps.de_jure_runs.len(),
            }
        } else {
            GeographyReport {
                exact_geography_supplied: false,
                land_runs: 0,
                world_control_runs: 0,
                de_jure_runs: 0,
            }
        }
    }

    fn checkpoint_report(&self) -> CheckpointReport {
        CheckpointReport {
            checkpoint_boundary: self.checkpoint.checkpoint_boundary,
            geography: self.geography_report(),
            territory: self.checkpoint.territory.as_ref().map(|territory| {
                TerritoryCheckpointReport {
                    encoding: territory.encoding.clone(),
                    land_runs: territory.maps.land_runs.len(),
                    world_control_runs: territory.maps.world_control_runs.len(),
                    de_jure_runs: territory.maps.de_jure_runs.len(),
                    primary_occupier_runs: territory.maps.primary_occupier_runs.len(),
                    dominant_side_runs: territory.maps.dominant_side_runs.len(),
                    occupation_runs: territory.maps.occupation_bits_runs.len(),
                    side_influence_rows: territory.maps.side_influence_bits_runs.len(),
                    side_influence_runs: territory
                        .maps
                        .side_influence_bits_runs
                        .iter()
                        .map(Vec::len)
                        .sum(),
                    revisions: territory.revisions,
                    committed_census: territory.committed_census,
                }
            }),
            tick: self.checkpoint.tick,
            frame: self.checkpoint.frame,
            war_grace_end: self.checkpoint.war_grace_end,
            strategic_cycle: self.checkpoint.strategic_cycle,
            sides: self.checkpoint.sides.clone(),
            active_sides: self.checkpoint.active_sides.clone(),
            hostility_matrix: self.checkpoint.hostility_matrix.clone(),
            units: self.checkpoint.units.len(),
            economies: self.checkpoint.economies.clone(),
            occupations: self.checkpoint.occupations.len(),
            casualties: self.checkpoint.casualties.clone(),
            casualties_by_victim: self.checkpoint.casualties_by_victim.clone(),
            gameplay_rng: self.checkpoint.gameplay_rng.clone(),
            personnel_reserves: self.checkpoint.personnel_reserves.clone(),
            reinforcement: self.checkpoint.reinforcement.clone(),
            strategic_missiles: self.checkpoint.strategic_missiles.clone(),
        }
    }

    fn production_report(&self) -> RuntimeProductionReport {
        let theater_countries = self
            .country_to_side
            .keys()
            .filter_map(|country_id| {
                selected_country_report(&country_id.to_string(), &self.production).ok()
            })
            .collect();
        RuntimeProductionReport {
            counters: ProductionCountersReport::from(&self.production),
            theater_countries,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSnapshotReport {
    tick: u64,
    frame: u64,
    state: RuntimeStateReport,
    units: UnitTotalsReport,
    combat_events: usize,
    removed_units: usize,
    abandoned_orders: usize,
    territory: TerritorySnapshotReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategic: Option<StrategicSnapshotReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operational: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operational_execution: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    air_power: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reinforcement: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategic_missiles: Option<Value>,
    counters: Value,
    casualty_totals: BTreeMap<u16, f64>,
    casualties_by_victim: BTreeMap<u16, BTreeMap<u16, f64>>,
    gameplay_rng_state: u32,
    personnel_reserves: BTreeMap<usize, f64>,
    pending_render_updates: usize,
    state_checksum: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStateReport {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycle: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tick: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    desertion_commands: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surrender_commands: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_resolution: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ConflictResolutionPlan>,
}

impl From<RuntimeState> for RuntimeStateReport {
    fn from(value: RuntimeState) -> Self {
        match value {
            RuntimeState::Running => Self {
                kind: "running",
                cycle: None,
                tick: None,
                desertion_commands: None,
                surrender_commands: None,
                conflict_resolution: None,
                resolution: None,
            },
            RuntimeState::AwaitingStrategicEffects {
                cycle,
                tick,
                desertion_commands,
                surrender_commands,
                conflict_resolution,
            } => Self {
                kind: "awaitingStrategicEffects",
                cycle: Some(cycle),
                tick: Some(tick),
                desertion_commands: Some(desertion_commands),
                surrender_commands: Some(surrender_commands),
                conflict_resolution: Some(conflict_resolution),
                resolution: None,
            },
            RuntimeState::ConflictResolved {
                cycle,
                tick,
                resolution,
            } => Self {
                kind: "conflictResolved",
                cycle: Some(cycle),
                tick: Some(tick),
                desertion_commands: None,
                surrender_commands: None,
                conflict_resolution: Some(true),
                resolution: Some(resolution),
            },
            RuntimeState::Poisoned => Self {
                kind: "poisoned",
                cycle: None,
                tick: None,
                desertion_commands: None,
                surrender_commands: None,
                conflict_resolution: None,
                resolution: None,
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnitTotalsReport {
    total: usize,
    by_side: Vec<SideUnitTotalsReport>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SideUnitTotalsReport {
    side: u16,
    units: usize,
    armies: usize,
    armor: usize,
    health: f64,
    personnel: u64,
    equipment: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerritorySnapshotReport {
    generation: u64,
    commit_sequence: u64,
    land_cells: u64,
    positive_occupation_cells: u64,
    negative_occupation_cells: u64,
    countries: Vec<CountryTerritoryReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CountryTerritoryReport {
    country_id: u16,
    side_index: i16,
    owned: u64,
    controlled: u64,
    core_controlled: u64,
    core_control_ratio: f64,
    city_population_controlled: f64,
    capital_held: bool,
    frontline: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StrategicSnapshotReport {
    cycle: u64,
    tick: u64,
    countries: usize,
    occupations: usize,
    occupation_assessments: usize,
    desertion_commands: usize,
    surrender_commands: usize,
    events: usize,
    conflict_resolved: bool,
    economies: Vec<LiveEconomyReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveEconomyReport {
    country_id: u16,
    treasury: f64,
    income: f64,
    payroll_due: f64,
    occupation_due: f64,
    payroll_coverage: f64,
    occupation_coverage: f64,
    arrears_cycles: f64,
    command_band: CommandBand,
    capitulated: bool,
}

fn snapshot_report(
    snapshot: &Arc<RuntimeSnapshot>,
    country_to_side: &BTreeMap<u16, usize>,
) -> Result<RuntimeSnapshotReport> {
    let mut by_side = BTreeMap::<u16, SideUnitTotalsReport>::new();
    for unit in snapshot.frame_snapshot.units.iter() {
        let side = unit.side;
        let totals = by_side.entry(side).or_insert_with(|| SideUnitTotalsReport {
            side,
            ..Default::default()
        });
        totals.units += 1;
        match unit.kind {
            UnitKind::Army => totals.armies += 1,
            UnitKind::Armor => totals.armor += 1,
        }
        totals.health += unit.health;
        totals.personnel = totals.personnel.saturating_add(unit.personnel);
        totals.equipment = totals.equipment.saturating_add(unit.equipment);
    }
    let countries = snapshot
        .territory_snapshot
        .countries
        .iter()
        .filter(|country| country_to_side.contains_key(&country.country_id))
        .map(|country| CountryTerritoryReport {
            country_id: country.country_id,
            side_index: country.side_index,
            owned: country.owned,
            controlled: country.controlled,
            core_controlled: country.core_controlled,
            core_control_ratio: country.core_control_ratio,
            city_population_controlled: country.city_population_controlled,
            capital_held: country.capital_held,
            frontline: country.frontline,
        })
        .collect();
    let strategic = snapshot
        .strategic_snapshot
        .as_ref()
        .map(|strategic| StrategicSnapshotReport {
            cycle: strategic.cycle,
            tick: strategic.tick,
            countries: strategic.countries.len(),
            occupations: strategic.occupations.len(),
            occupation_assessments: strategic.occupation_assessments.len(),
            desertion_commands: strategic.desertions.len(),
            surrender_commands: strategic.surrenders.len(),
            events: strategic.events.len(),
            conflict_resolved: strategic.conflict_resolution.is_some(),
            economies: strategic
                .countries
                .iter()
                .map(|country| LiveEconomyReport {
                    country_id: country.country_id,
                    treasury: country.economy.treasury,
                    income: country.economy.income,
                    payroll_due: country.economy.payroll_due,
                    occupation_due: country.economy.occupation_due,
                    payroll_coverage: country.economy.payroll_coverage,
                    occupation_coverage: country.economy.occupation_coverage,
                    arrears_cycles: country.economy.arrears_cycles,
                    command_band: country.economy.command_band,
                    capitulated: country.economy.capitulated,
                })
                .collect(),
        });
    Ok(RuntimeSnapshotReport {
        tick: snapshot.tick,
        frame: snapshot.frame,
        state: snapshot.state.into(),
        units: UnitTotalsReport {
            total: snapshot.frame_snapshot.units.len(),
            by_side: by_side.into_values().collect(),
        },
        combat_events: snapshot.frame_snapshot.events.len(),
        removed_units: snapshot.frame_snapshot.removed_ids.len(),
        abandoned_orders: snapshot.frame_snapshot.abandoned_ids.len(),
        territory: TerritorySnapshotReport {
            generation: snapshot.territory_snapshot.generation,
            commit_sequence: snapshot.territory_snapshot.commit_sequence,
            land_cells: snapshot.territory_snapshot.land_cells,
            positive_occupation_cells: snapshot.territory_snapshot.positive_occupation_cells,
            negative_occupation_cells: snapshot.territory_snapshot.negative_occupation_cells,
            countries,
        },
        strategic,
        operational: snapshot
            .operational_snapshot
            .as_ref()
            .map(|operational| serde_json::to_value(operational.as_ref()))
            .transpose()?,
        operational_execution: snapshot
            .operational_execution_snapshot
            .as_ref()
            .map(|execution| serde_json::to_value(execution.as_ref()))
            .transpose()?,
        air_power: snapshot
            .air_power_snapshot
            .as_ref()
            .map(|air_power| serde_json::to_value(air_power.as_ref()))
            .transpose()?,
        reinforcement: snapshot
            .reinforcement_snapshot
            .as_ref()
            .map(|reinforcement| serde_json::to_value(reinforcement.as_ref()))
            .transpose()?,
        strategic_missiles: snapshot
            .strategic_missile_snapshot
            .as_ref()
            .map(|missiles| serde_json::to_value(missiles.as_ref()))
            .transpose()?,
        counters: counters_json(snapshot),
        casualty_totals: snapshot.casualty_totals.as_ref().clone(),
        casualties_by_victim: snapshot.casualties_by_victim.as_ref().clone(),
        gameplay_rng_state: snapshot.gameplay_rng_state.state,
        personnel_reserves: snapshot.personnel_reserves.as_ref().clone(),
        pending_render_updates: snapshot.pending_render_updates,
        state_checksum: snapshot_checksum(snapshot)?,
    })
}

fn counters_json(snapshot: &RuntimeSnapshot) -> Value {
    let counters = snapshot.counters;
    json!({
        "front": {
            "refreshed": counters.front_refreshed,
            "segments": counters.front_segments,
            "objectives": counters.front_objectives,
        },
        "ai": {
            "inputUnits": counters.ai.input_units,
            "contactOrders": counters.ai.contact_orders,
            "retreatOrders": counters.ai.retreat_orders,
            "stickyAssignments": counters.ai.sticky_assignments,
            "frontAssignments": counters.ai.front_assignments,
            "reinforcementAssignments": counters.ai.reinforcement_assignments,
            "fieldOrders": counters.ai.field_orders,
            "holdOrders": counters.ai.hold_orders,
        },
        "simulation": {
            "inputUnits": counters.simulation.input_units,
            "tacticalCells": counters.simulation.tactical_cells,
            "candidateContacts": counters.simulation.candidate_contacts,
            "acceptedContacts": counters.simulation.accepted_contacts,
            "proximityEvents": counters.simulation.proximity_events,
            "directEvents": counters.simulation.direct_events,
            "movedUnits": counters.simulation.moved_units,
            "heldUnits": counters.simulation.held_units,
            "removedUnits": counters.simulation.removed_units,
            "abandonedOrders": counters.simulation.abandoned_orders,
        },
        "attrition": {
            "damagedUnits": counters.attrition.damaged_units,
            "removedUnits": counters.attrition.removed_units,
            "personnelLoss": counters.attrition.personnel_loss,
            "equipmentLoss": counters.attrition.equipment_loss,
            "supplyCollapses": counters.attrition.supply_collapses,
            "exiledUnits": counters.attrition.exiled_units,
            "recoveredPersonnel": counters.attrition.recovered_personnel,
        },
        "missiles": {
            "launches": counters.missiles.launches,
            "impacts": counters.missiles.impacts,
            "damagedUnits": counters.missiles.damaged_units,
            "removedUnits": counters.missiles.removed_units,
            "personnelLoss": counters.missiles.personnel_loss,
            "equipmentLoss": counters.missiles.equipment_loss,
        },
        "reinforcement": {
            "recruitedUnits": counters.reinforcement.recruited_units,
            "recruitedPersonnel": counters.reinforcement.recruited_personnel,
            "aircraftPurchased": counters.reinforcement.aircraft_purchased,
            "aircraftReinforced": counters.reinforcement.aircraft_reinforced,
            "airWingsCreated": counters.reinforcement.air_wings_created,
        },
        "influence": {
            "sources": counters.influence.sources,
            "cohort": counters.influence.cohort,
            "applicationBudget": counters.influence.application_budget,
            "diffusionBudget": counters.influence.diffusion_budget,
            "diffusionProcessedItems": counters.influence.diffusion_processed_items,
            "diffusionStaleEntries": counters.influence.diffusion_stale_entries,
            "processedSourceCells": counters.influence.processed_source_cells,
            "touchedInfluenceCells": counters.influence.touched_influence_cells,
            "changedControllerCells": counters.influence.changed_controller_cells,
            "changedCreditCells": counters.influence.changed_credit_cells,
        },
        "census": {
            "processedItems": counters.census.processed_items,
            "committed": counters.census.committed,
            "flushedForStrategicCycle": counters.census.flushed_for_strategic_cycle,
            "territoryGeneration": counters.census.territory_generation,
            "territoryCommitSequence": counters.census.territory_commit_sequence,
        },
        "strategic": counters.strategic.map(|value| json!({
            "countriesProcessed": value.countries_processed,
            "occupationsProcessed": value.occupations_processed,
            "capitulations": value.capitulations,
            "desertionCommands": value.desertion_commands,
            "events": value.events,
        })),
        "strategicDerivation": counters.strategic_derivation.map(|value| json!({
            "countries": value.countries,
            "units": value.units,
            "payrollUnits": value.payroll_units,
            "garrisonUnits": value.garrison_units,
            "occupations": value.occupations,
            "activeSides": value.active_sides,
            "hostilePairs": value.hostile_pairs,
        })),
        "renderUpdatesEnqueued": counters.render_updates_enqueued,
    })
}

fn snapshot_checksum(snapshot: &RuntimeSnapshot) -> Result<String> {
    let mut checksum = Fnv64::new();
    checksum.write_u64(snapshot.tick);
    checksum.write_u64(snapshot.frame);
    match snapshot.state {
        RuntimeState::Running => checksum.write_u64(0),
        RuntimeState::AwaitingStrategicEffects {
            cycle,
            tick,
            desertion_commands,
            surrender_commands,
            conflict_resolution,
        } => {
            checksum.write_u64(1);
            checksum.write_u64(cycle);
            checksum.write_u64(tick);
            checksum.write_usize(desertion_commands);
            checksum.write_usize(surrender_commands);
            checksum.write_bool(conflict_resolution);
        }
        RuntimeState::Poisoned => checksum.write_u64(2),
        RuntimeState::ConflictResolved {
            cycle,
            tick,
            resolution,
        } => {
            checksum.write_u64(3);
            checksum.write_u64(cycle);
            checksum.write_u64(tick);
            checksum.write_bytes(&serde_json::to_vec(&resolution)?);
        }
    }
    checksum.write_usize(snapshot.frame_snapshot.units.len());
    for unit in snapshot.frame_snapshot.units.iter() {
        checksum.write_u64(unit.id);
        checksum.write_u16(unit.side);
        checksum.write_u64(unit.sovereign);
        checksum.write_u64(match unit.kind {
            UnitKind::Army => 0,
            UnitKind::Armor => 1,
        });
        for value in [
            unit.lat,
            unit.lng,
            unit.health,
            unit.max_health,
            f64::from(unit.health_fraction),
            unit.dir_lat,
            unit.dir_lng,
        ] {
            checksum.write_f64(value);
        }
        for value in [
            unit.personnel,
            unit.personnel_capacity,
            unit.equipment,
            unit.max_equipment,
            unit.last_combat_tick,
            unit.victory_boost_ticks,
        ] {
            checksum.write_u64(value);
        }
        checksum.write_u64(u64::from(unit.coast_stuck_ticks));
        for value in [
            unit.landing_penalty_active,
            unit.transport,
            unit.at_sea,
            unit.armor_supported,
            unit.is_alpenjager,
        ] {
            checksum.write_bool(value);
        }
        checksum.write_u64(unit.encircled_ticks);
        checksum.write_f64(f64::from(unit.mountain_intensity));
    }
    checksum.write_usize(snapshot.frame_snapshot.events.len());
    for event in snapshot.frame_snapshot.events.iter() {
        checksum_combat_event(&mut checksum, event);
    }
    for ids in [
        snapshot.frame_snapshot.removed_ids.as_ref(),
        snapshot.frame_snapshot.abandoned_ids.as_ref(),
    ] {
        checksum.write_usize(ids.len());
        for &id in ids {
            checksum.write_u64(id);
        }
    }
    checksum.write_bytes(&serde_json::to_vec(snapshot.territory_snapshot.as_ref())?);
    if let Some(strategic) = &snapshot.strategic_snapshot {
        checksum.write_bool(true);
        checksum.write_u64(strategic.cycle);
        checksum.write_u64(strategic.tick);
        checksum.write_u64(strategic.territory_generation);
        checksum.write_u64(strategic.territory_commit_sequence);
        checksum.write_bytes(&serde_json::to_vec(strategic.countries.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.occupations.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(
            strategic.occupation_assessments.as_ref(),
        )?);
        checksum.write_bytes(&serde_json::to_vec(strategic.desertions.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.surrenders.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(strategic.events.as_ref())?);
        checksum.write_bytes(&serde_json::to_vec(&strategic.conflict_resolution)?);
    } else {
        checksum.write_bool(false);
    }
    if let Some(operational) = &snapshot.operational_snapshot {
        checksum.write_bool(true);
        checksum.write_bytes(&serde_json::to_vec(operational.as_ref())?);
    } else {
        checksum.write_bool(false);
    }
    if let Some(execution) = &snapshot.operational_execution_snapshot {
        checksum.write_bool(true);
        checksum.write_bytes(&serde_json::to_vec(execution.as_ref())?);
    } else {
        checksum.write_bool(false);
    }
    if let Some(air_power) = &snapshot.air_power_snapshot {
        checksum.write_bool(true);
        checksum.write_bytes(&serde_json::to_vec(air_power.as_ref())?);
    } else {
        checksum.write_bool(false);
    }
    checksum.write_bytes(&serde_json::to_vec(snapshot.casualty_totals.as_ref())?);
    checksum.write_bytes(&serde_json::to_vec(snapshot.casualties_by_victim.as_ref())?);
    checksum.write_u64(u64::from(snapshot.gameplay_rng_state.state));
    checksum.write_bytes(&serde_json::to_vec(snapshot.personnel_reserves.as_ref())?);
    checksum.write_bytes(&serde_json::to_vec(
        &snapshot.reinforcement_snapshot.as_deref(),
    )?);
    checksum.write_bytes(&serde_json::to_vec(
        &snapshot.material_logistics_snapshot.as_deref(),
    )?);
    checksum.write_bytes(&serde_json::to_vec(
        &snapshot.strategic_missile_snapshot.as_deref(),
    )?);
    checksum.write_bytes(&serde_json::to_vec(&counters_json(snapshot))?);
    checksum.write_usize(snapshot.pending_render_updates);
    Ok(checksum.finish())
}

fn checksum_combat_event(checksum: &mut Fnv64, event: &CombatEvent) {
    checksum.write_u64(match event.layer {
        CombatLayer::Proximity => 0,
        CombatLayer::Direct => 1,
    });
    checksum.write_u64(event.attacker_id);
    checksum.write_u64(event.target_id);
    for value in [
        event.target_damage,
        event.attacker_damage,
        event.transport_self_damage,
        event.target_resulting_health,
        event.attacker_resulting_health,
    ] {
        checksum.write_f64(value);
    }
    for value in [
        event.target_personnel_loss,
        event.attacker_personnel_loss,
        event.target_equipment_loss,
        event.attacker_equipment_loss,
    ] {
        checksum.write_u64(value);
    }
    checksum.write_bool(event.target_knockback_blocked);
    checksum.write_bool(event.attacker_knockback_blocked);
}

fn checksum_serializable(value: &impl Serialize) -> Result<String> {
    let mut checksum = Fnv64::new();
    checksum.write_bytes(&serde_json::to_vec(value)?);
    Ok(checksum.finish())
}

struct Fnv64(u64);

impl Fnv64 {
    const fn new() -> Self {
        Self(FNV64_OFFSET)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV64_PRIME);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn write_f64(&mut self, value: f64) {
        self.write_u64(value.to_bits());
    }

    fn write_bool(&mut self, value: bool) {
        self.write_bytes(&[u8::from(value)]);
    }

    fn finish(&self) -> String {
        format!("{:016x}", self.0)
    }
}

fn scenario_name(decoded: &DecodedScenario) -> String {
    decoded
        .metadata
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Unnamed scenario")
        .to_owned()
}

fn read_file(path: &PathBuf) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn print_json(value: &impl Serialize, compact: bool) -> Result<()> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((input.len() + 72).next_multiple_of(64));
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    let mut words = [0_u32; 64];
    for block in padded.chunks_exact(64) {
        for (index, bytes) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for index in 16..64 {
            let left = words[index - 15];
            let right = words[index - 2];
            let sigma0 = left.rotate_right(7) ^ left.rotate_right(18) ^ (left >> 3);
            let sigma1 = right.rotate_right(17) ^ right.rotate_right(19) ^ (right >> 10);
            words[index] = words[index - 16]
                .wrapping_add(sigma0)
                .wrapping_add(words[index - 7])
                .wrapping_add(sigma1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    state.iter().map(|value| format!("{value:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_v2_territory() -> TerritoryV2Fixture {
        TerritoryV2Fixture {
            encoding: "rle-bits-v1".to_owned(),
            maps: TerritoryV2MapsFixture {
                land_runs: vec![(2, 1), (2, 2)],
                world_control_runs: vec![(2, 1), (2, 2)],
                de_jure_runs: vec![(1, 1), (3, 2)],
                primary_occupier_runs: vec![(2, 1), (2, 2)],
                dominant_side_runs: vec![(1, -1), (1, 0), (2, 1)],
                occupation_bits_runs: vec![(2, 0.25_f32.to_bits()), (2, (-0.5_f32).to_bits())],
                side_influence_bits_runs: vec![
                    vec![(2, 1.0_f32.to_bits()), (2, 0.0_f32.to_bits())],
                    vec![(2, 0.0_f32.to_bits()), (2, 1.0_f32.to_bits())],
                ],
            },
            revisions: TerritoryRevisionFixture {
                topology_revision: 7,
                world_revision: 11,
                city_revision: 13,
            },
            committed_census: TerritoryCommittedCensusFixture {
                generation: 5,
                commit_sequence: 4,
                mutation_sequence: 9,
                processed_tiles: 1,
                processed_items: 4,
            },
        }
    }

    fn valid_battlefield() -> BattlefieldFixture {
        BattlefieldFixture {
            schema: BATTLEFIELD_SCHEMA_VERSION.to_owned(),
            mountains_enabled: true,
            terrain_intensity_bits_runs: vec![
                (1, 0.0_f32.to_bits()),
                (2, f32::from_bits(0x3eaa_aaab).to_bits()),
                (1, 0.0_f32.to_bits()),
            ],
            urban_centers: vec![BattlefieldUrbanCenterFixture {
                id: 11,
                country_id: 1,
                cell: 0,
                lat: -89.5,
                lng: -179.5,
            }],
            config: BattlefieldConfig::default().into(),
            countries: vec![BattlefieldCountryFixture {
                country_id: 1,
                combat_buff: BattlefieldBuffFixture::Super,
                influence_buff: BattlefieldBuffFixture::Weakened,
                attack_buff_percent: 12.5,
                defense_buff_percent: 7.5,
                capital_lost: true,
                war_phase: BattlefieldWarPhaseFixture::Retreating,
                conquest_mode: false,
                ai_speed_multiplier: 1.25,
            }],
            units: vec![BattlefieldUnitStateFixture {
                unit_id: 7,
                is_alpenjager: true,
                cohesion_seed: 0.375,
                local_tactics_excluded: false,
                encircled_ticks: 61,
                armor_support_last_tick: Some(9),
                supply_collapsed_tick: Some(8),
                last_ally_count: 4.0,
            }],
        }
    }

    fn valid_influence_runtime() -> InfluenceRuntimeFixture {
        InfluenceRuntimeFixture {
            schema: NATIVE_INFLUENCE_RUNTIME_SCHEMA.to_owned(),
            // Repeated and stale entries are intentional and must remain accepted.
            regular_queue: vec![0, 1, 1, 3],
            priority_queue: vec![2, 0],
            queued_cells: vec![(1, 1), (2, 2)],
        }
    }

    fn minimal_checkpoint(
        schema: &str,
        boundary: CheckpointBoundary,
        territory: Option<TerritoryV2Fixture>,
    ) -> RuntimeCheckpointFixture {
        RuntimeCheckpointFixture {
            schema: schema.to_owned(),
            checkpoint_boundary: boundary,
            scenario: ScenarioExpectation {
                sha256: "0".repeat(64),
                name: "test".to_owned(),
                grid_res: 1.0,
            },
            geography: Some(GeographyFixture {
                land_runs: vec![(4, 1)],
                world_control_runs: vec![(4, 1)],
                de_jure_runs: vec![(4, 1)],
            }),
            territory,
            battlefield: None,
            influence_runtime: None,
            side_dynamics: None,
            sides: vec![SideFixture {
                side_index: 0,
                country_ids: vec![1],
            }],
            active_sides: vec![0],
            hostility_matrix: vec![0],
            tick: 0,
            frame: 0,
            war_grace_end: 0,
            strategic_cycle: 0,
            steps: 1,
            planner: None,
            units: Vec::new(),
            economies: Vec::new(),
            occupations: Vec::new(),
            casualties: BTreeMap::new(),
            casualties_by_victim: BTreeMap::new(),
            operational_ai: None,
            operational_execution: None,
            air_power: None,
            naval_planning: None,
            gameplay_rng: None,
            personnel_reserves: None,
            reinforcement: None,
            material_logistics: None,
            strategic_missiles: None,
        }
    }

    fn valid_v6_checkpoint() -> RuntimeCheckpointFixture {
        let mut checkpoint = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
            CheckpointBoundary::MidWar,
            Some(valid_v2_territory()),
        );
        checkpoint.battlefield = Some(valid_battlefield());
        checkpoint.influence_runtime = Some(valid_influence_runtime());
        checkpoint.side_dynamics = Some(SideDynamicsFixture {
            schema: NATIVE_SIDE_DYNAMICS_SCHEMA.to_owned(),
            sides: vec![SideDynamicsSideFixture {
                side_index: 0,
                initial_personnel: 100.0,
                personnel: 100.0,
                momentum_history: Vec::new(),
                war_phase: SideDynamicsWarPhaseFixture::Stalemate,
                posture: SideDynamicsPostureFixture::Balanced,
                posture_override: None,
            }],
        });
        checkpoint.operational_ai = Some(OperationalRuntimeState::bootstrap(1, &[0], &[0.0]));
        checkpoint.operational_execution = Some(OperationalExecutionState::default());
        checkpoint.air_power = Some(AirPowerState::new(Vec::new(), Vec::new()).unwrap());
        checkpoint
    }

    fn valid_v7_checkpoint() -> RuntimeCheckpointFixture {
        let mut checkpoint = valid_v6_checkpoint();
        checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA.to_owned();
        checkpoint.naval_planning = Some(
            serde_json::from_value(json!({
                "schema": "native-naval-planning-v1",
                "sideStates": [{"side": 0, "nextReassessTick": 0}],
                "nextOperationSequence": 1
            }))
            .unwrap(),
        );
        checkpoint
    }

    fn valid_v8_checkpoint() -> RuntimeCheckpointFixture {
        let mut checkpoint = valid_v7_checkpoint();
        checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA.to_owned();
        checkpoint
    }

    fn valid_v9_checkpoint() -> RuntimeCheckpointFixture {
        let mut checkpoint = valid_v8_checkpoint();
        checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA.to_owned();
        checkpoint.gameplay_rng = Some(GameplayRngFixture {
            schema: GAMEPLAY_RNG_SCHEMA_VERSION.to_owned(),
            algorithm: GAMEPLAY_RNG_ALGORITHM.to_owned(),
            state: 0x4d57_5031,
        });
        checkpoint.personnel_reserves = Some(vec![125.0]);
        checkpoint
    }

    fn valid_v10_checkpoint() -> RuntimeCheckpointFixture {
        let mut checkpoint = valid_v9_checkpoint();
        checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA.to_owned();
        checkpoint
            .air_power
            .as_mut()
            .unwrap()
            .country_coverage
            .push(mw_core::AirCountryCoverage {
                country_id: 1,
                operations_coverage: 1.0,
            });
        checkpoint.reinforcement = Some(
            ReinforcementState::bootstrap(
                checkpoint.air_power.as_ref().unwrap(),
                1,
                1,
                &BTreeMap::from([(1, 0)]),
                1,
            )
            .unwrap(),
        );
        checkpoint
    }

    fn valid_legacy_checkpoint(schema: &str) -> RuntimeCheckpointFixture {
        match schema {
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA => minimal_checkpoint(
                NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
                CheckpointBoundary::PostStartWar,
                None,
            ),
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA => minimal_checkpoint(
                NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
                CheckpointBoundary::MidWar,
                Some(valid_v2_territory()),
            ),
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA => {
                let mut checkpoint = minimal_checkpoint(
                    NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
                    CheckpointBoundary::MidWar,
                    Some(valid_v2_territory()),
                );
                checkpoint.influence_runtime = Some(valid_influence_runtime());
                checkpoint
            }
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA => {
                let mut checkpoint = valid_v6_checkpoint();
                checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA.to_owned();
                checkpoint.operational_ai = None;
                checkpoint.operational_execution = None;
                checkpoint.air_power = None;
                checkpoint
            }
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA => {
                let mut checkpoint = valid_v6_checkpoint();
                checkpoint.schema = NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA.to_owned();
                checkpoint.operational_execution = None;
                checkpoint.air_power = None;
                checkpoint
            }
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA => valid_v6_checkpoint(),
            _ => panic!("unsupported legacy checkpoint schema {schema}"),
        }
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fnv_checksum_is_stable() {
        let mut checksum = Fnv64::new();
        checksum.write_bytes(b"native-runtime");
        assert_eq!(checksum.finish(), "3627c1abde06b44b");
    }

    #[test]
    fn checkpoint_boundaries_have_explicit_resume_semantics() {
        let production: CheckpointBoundary = serde_json::from_str("\"postStartWar\"").unwrap();
        let replay: CheckpointBoundary = serde_json::from_str("\"baselineReplay\"").unwrap();
        let mid_war: CheckpointBoundary = serde_json::from_str("\"midWar\"").unwrap();
        assert!(production.resumable());
        assert!(!replay.resumable());
        assert!(mid_war.resumable());
        assert!(replay.description().contains("not resumable"));
        assert_eq!(mid_war.as_str(), "midWar");
    }

    #[test]
    fn legacy_checkpoint_recovers_stable_discipline_from_browser_seed() {
        let checkpoint: RuntimeCheckpointFixture = serde_json::from_str(include_str!(
            "../../../fixtures/native-runtime-checkpoint-v1.json"
        ))
        .unwrap();
        let unit = &checkpoint.units[0];
        assert!(unit.command_policy.is_none());
        let seed = unit
            .influence_policy
            .as_ref()
            .and_then(|policy| policy.temporal_seed)
            .unwrap_or(unit.id as f64);
        let coalition = checkpoint.sides[usize::from(unit.side)]
            .country_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        let unpaid = unit.runtime_policy(&coalition, CommandBand::Unpaid, 2);
        assert_eq!(
            unpaid.command.discipline,
            browser_discipline(seed, unit.country_id)
        );
        assert_eq!(
            unpaid.command.refuses_offense,
            unpaid.command.discipline < command_refusal_share(CommandBand::Unpaid)
        );

        let breakdown = unit.runtime_policy(&coalition, CommandBand::Breakdown, 3);
        assert!(breakdown.command.refuses_offense);
        assert!(breakdown.influence.as_ref().unwrap().refuses_offense);
    }

    #[test]
    fn resolved_runtime_state_report_preserves_terminal_result() {
        let resolution = ConflictResolutionPlan {
            kind: mw_core::ConflictResolutionKind::FullCapitulation,
            winner_side: Some(1),
            stop_simulation: true,
        };
        let report = RuntimeStateReport::from(RuntimeState::ConflictResolved {
            cycle: 8,
            tick: 4_800,
            resolution,
        });
        assert_eq!(report.kind, "conflictResolved");
        assert_eq!(report.cycle, Some(8));
        assert_eq!(report.tick, Some(4_800));
        assert_eq!(report.conflict_resolution, Some(true));
        assert_eq!(report.resolution, Some(resolution));
    }

    #[test]
    fn checkpoint_v2_requires_its_mid_war_live_state_contract() {
        let mut checkpoint = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            CheckpointBoundary::MidWar,
            Some(valid_v2_territory()),
        );
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        checkpoint.planner = Some(PlannerFixture {
            objectives: Vec::new(),
            prior_objective_by_unit: BTreeMap::new(),
            front_prior_by_unit: Vec::new(),
            last_front_refresh_tick: None,
        });
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut independently_advanced_cycle = checkpoint.clone();
        independently_advanced_cycle.tick = 1;
        independently_advanced_cycle.strategic_cycle = 7;
        assert!(validate_checkpoint_shape(&independently_advanced_cycle).is_ok());

        let mut exhausted_cycle = checkpoint.clone();
        exhausted_cycle.strategic_cycle = u64::MAX;
        assert!(validate_checkpoint_shape(&exhausted_cycle).is_err());

        let mut wrong_boundary = checkpoint.clone();
        wrong_boundary.checkpoint_boundary = CheckpointBoundary::PostStartWar;
        assert!(validate_checkpoint_shape(&wrong_boundary).is_err());

        let mut missing_geography = checkpoint.clone();
        missing_geography.geography = None;
        assert!(validate_checkpoint_shape(&missing_geography).is_err());

        let mut missing_territory = checkpoint.clone();
        missing_territory.territory = None;
        assert!(validate_checkpoint_shape(&missing_territory).is_err());

        let legacy_with_v2_state = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            CheckpointBoundary::PostStartWar,
            Some(valid_v2_territory()),
        );
        assert!(validate_checkpoint_shape(&legacy_with_v2_state).is_err());

        let mut legacy_with_planner = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            CheckpointBoundary::PostStartWar,
            None,
        );
        legacy_with_planner.planner = checkpoint.planner;
        assert!(validate_checkpoint_shape(&legacy_with_planner).is_err());
    }

    #[test]
    fn checkpoint_v3_requires_influence_runtime_and_older_schemas_reject_it() {
        let mut checkpoint = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            CheckpointBoundary::MidWar,
            Some(valid_v2_territory()),
        );
        checkpoint.influence_runtime = Some(valid_influence_runtime());
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing = checkpoint.clone();
        missing.influence_runtime = None;
        assert!(validate_checkpoint_shape(&missing).is_err());

        let mut v2 = checkpoint.clone();
        v2.schema = NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&v2).is_err());

        let mut v1 = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            CheckpointBoundary::PostStartWar,
            None,
        );
        v1.influence_runtime = Some(valid_influence_runtime());
        assert!(validate_checkpoint_shape(&v1).is_err());
    }

    #[test]
    fn checkpoint_v4_side_dynamics_wire_is_strict() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OptionalSideDynamics {
            #[serde(
                default,
                deserialize_with = "deserialize_optional_side_dynamics_fixture"
            )]
            side_dynamics: Option<SideDynamicsFixture>,
        }

        let absent: OptionalSideDynamics = serde_json::from_value(json!({})).unwrap();
        assert!(absent.side_dynamics.is_none());
        assert!(
            serde_json::from_value::<OptionalSideDynamics>(json!({"sideDynamics": null})).is_err()
        );

        let valid = SideDynamicsFixture {
            schema: NATIVE_SIDE_DYNAMICS_SCHEMA.to_owned(),
            sides: vec![SideDynamicsSideFixture {
                side_index: 0,
                initial_personnel: 100.0,
                personnel: 90.0,
                momentum_history: vec![SideDynamicsSampleFixture {
                    frame: 1,
                    controlled: 2,
                }],
                war_phase: SideDynamicsWarPhaseFixture::Advancing,
                posture: SideDynamicsPostureFixture::Offensive,
                posture_override: None,
            }],
        };
        validate_side_dynamics(&valid, 1, 1, 4).unwrap();
        let encoded = serde_json::to_value(&valid).unwrap();
        assert_eq!(encoded["sides"][0]["warPhase"], "ADVANCING");
        assert_eq!(encoded["sides"][0]["posture"], "OFFENSIVE");
        assert!(encoded["sides"][0]["postureOverride"].is_null());
        let mut missing_override = encoded.clone();
        missing_override["sides"][0]
            .as_object_mut()
            .unwrap()
            .remove("postureOverride");
        assert!(serde_json::from_value::<SideDynamicsFixture>(missing_override).is_err());
        assert!(
            serde_json::from_value::<SideDynamicsFixture>(json!({
                "schema": NATIVE_SIDE_DYNAMICS_SCHEMA,
                "sides": [{
                    "sideIndex": 0,
                    "initialPersonnel": 100.0,
                    "personnel": 90.0,
                    "momentumHistory": [],
                    "warPhase": "advancing",
                    "posture": "OFFENSIVE",
                    "postureOverride": null
                }]
            }))
            .is_err()
        );

        let mut checkpoint = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            CheckpointBoundary::MidWar,
            Some(valid_v2_territory()),
        );
        checkpoint.frame = 1;
        checkpoint.influence_runtime = Some(valid_influence_runtime());
        checkpoint.battlefield = Some(valid_battlefield());
        checkpoint.side_dynamics = Some(valid.clone());
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_dynamics = checkpoint.clone();
        missing_dynamics.side_dynamics = None;
        assert!(validate_checkpoint_shape(&missing_dynamics).is_err());
        let mut missing_battlefield = checkpoint.clone();
        missing_battlefield.battlefield = None;
        assert!(validate_checkpoint_shape(&missing_battlefield).is_err());
        let mut legacy_v3 = checkpoint.clone();
        legacy_v3.schema = NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&legacy_v3).is_err());

        let mut too_many = valid.clone();
        too_many.sides[0].momentum_history = (0..11)
            .map(|frame| SideDynamicsSampleFixture {
                frame,
                controlled: 1,
            })
            .collect();
        assert!(validate_side_dynamics(&too_many, 1, 10, 4).is_err());
        let mut outside_grid = valid.clone();
        outside_grid.sides[0].momentum_history[0].controlled = 5;
        assert!(validate_side_dynamics(&outside_grid, 1, 1, 4).is_err());
        let mut balanced_override = valid.clone();
        balanced_override.sides[0].posture_override = Some(SideDynamicsPostureFixture::Balanced);
        assert!(validate_side_dynamics(&balanced_override, 1, 1, 4).is_err());
        let mut unknown_schema = valid;
        unknown_schema.schema = "native-side-dynamics-v0".to_owned();
        assert!(validate_side_dynamics(&unknown_schema, 1, 1, 4).is_err());
    }

    #[test]
    fn checkpoint_v5_requires_operational_ai_and_v4_forbids_it() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OptionalOperationalAi {
            #[serde(default, deserialize_with = "deserialize_optional_operational_ai")]
            operational_ai: Option<OperationalRuntimeState>,
        }

        let absent: OptionalOperationalAi = serde_json::from_value(json!({})).unwrap();
        assert!(absent.operational_ai.is_none());
        assert!(
            serde_json::from_value::<OptionalOperationalAi>(json!({"operationalAi": null}))
                .is_err()
        );
        let operations = OperationalRuntimeState::bootstrap(1, &[0], &[0.0]);
        let present: OptionalOperationalAi = serde_json::from_value(json!({
            "operationalAi": serde_json::to_value(&operations).unwrap()
        }))
        .unwrap();
        assert_eq!(present.operational_ai, Some(operations.clone()));

        let dynamics = SideDynamicsFixture {
            schema: NATIVE_SIDE_DYNAMICS_SCHEMA.to_owned(),
            sides: vec![SideDynamicsSideFixture {
                side_index: 0,
                initial_personnel: 100.0,
                personnel: 90.0,
                momentum_history: Vec::new(),
                war_phase: SideDynamicsWarPhaseFixture::Stalemate,
                posture: SideDynamicsPostureFixture::Balanced,
                posture_override: None,
            }],
        };
        let mut checkpoint = minimal_checkpoint(
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            CheckpointBoundary::MidWar,
            Some(valid_v2_territory()),
        );
        checkpoint.battlefield = Some(valid_battlefield());
        checkpoint.influence_runtime = Some(valid_influence_runtime());
        checkpoint.side_dynamics = Some(dynamics);
        checkpoint.operational_ai = Some(operations);
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing = checkpoint.clone();
        missing.operational_ai = None;
        assert!(validate_checkpoint_shape(&missing).is_err());
        let mut v4_with_operations = checkpoint.clone();
        v4_with_operations.schema = NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&v4_with_operations).is_err());
        let mut overflowing_cycle = checkpoint;
        overflowing_cycle.strategic_cycle = u64::MAX;
        assert!(validate_checkpoint_shape(&overflowing_cycle).is_err());
    }

    #[test]
    fn checkpoint_v5_requires_every_nullable_operational_key() {
        let valid = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            "operationalAi": {
                "sides": [{
                    "override": {"expiresTick": null},
                    "intel": {"contacts": [{"countryId": null}]}
                }],
                "taskForces": [{
                    "theaterId": null,
                    "target": null,
                    "stagingAnchor": null,
                    "withdrawalAnchor": null,
                    "completionReason": null,
                    "outcome": null,
                    "parentTaskForceId": null,
                    "supplyInvalidatedTick": null
                }],
                "countryDesperation": [{
                    "initialCities": null,
                    "initialManpower": null,
                    "previousControlled": null
                }],
                "overrideEvents": [{"posture": null, "expiresTick": null}]
            }
        });
        validate_checkpoint_v5_required_nullable_fields(&valid).unwrap();

        for (parent, key) in [
            ("/operationalAi/sides/0", "override"),
            ("/operationalAi/sides/0/override", "expiresTick"),
            ("/operationalAi/sides/0/intel/contacts/0", "countryId"),
            ("/operationalAi/taskForces/0", "theaterId"),
            ("/operationalAi/taskForces/0", "target"),
            ("/operationalAi/taskForces/0", "stagingAnchor"),
            ("/operationalAi/taskForces/0", "withdrawalAnchor"),
            ("/operationalAi/taskForces/0", "completionReason"),
            ("/operationalAi/taskForces/0", "outcome"),
            ("/operationalAi/taskForces/0", "parentTaskForceId"),
            ("/operationalAi/taskForces/0", "supplyInvalidatedTick"),
            ("/operationalAi/countryDesperation/0", "initialCities"),
            ("/operationalAi/countryDesperation/0", "initialManpower"),
            ("/operationalAi/countryDesperation/0", "previousControlled"),
            ("/operationalAi/overrideEvents/0", "posture"),
            ("/operationalAi/overrideEvents/0", "expiresTick"),
        ] {
            let mut missing = valid.clone();
            missing
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(
                validate_checkpoint_v5_required_nullable_fields(&missing).is_err(),
                "missing {parent}/{key} was accepted"
            );
        }
    }

    #[test]
    fn checkpoint_v5_operational_wire_rejects_recursive_unknown_fields() {
        let operations = OperationalRuntimeState::bootstrap(1, &[0], &[0.0]);
        let encoded = serde_json::to_value(operations).unwrap();

        let mut unknown_root = encoded.clone();
        unknown_root["unknown"] = json!(true);
        assert!(serde_json::from_value::<OperationalRuntimeState>(unknown_root).is_err());

        let mut unknown_side = encoded;
        unknown_side["sides"][0]["unknown"] = json!(true);
        assert!(serde_json::from_value::<OperationalRuntimeState>(unknown_side).is_err());
    }

    #[test]
    fn checkpoint_v5_operational_hostility_is_exact_and_directed() {
        let operations = OperationalRuntimeState::bootstrap(2, &[0, 1, 1, 0], &[1.0, 1.0]);
        validate_operational_ai(
            &operations,
            2,
            &BTreeMap::new(),
            &BTreeSet::new(),
            0,
            &[0, 1, 1, 0],
        )
        .unwrap();
        assert!(
            validate_operational_ai(
                &operations,
                2,
                &BTreeMap::new(),
                &BTreeSet::new(),
                0,
                &[0, 1, 0, 0],
            )
            .is_err()
        );
        assert!(
            validate_operational_ai(
                &operations,
                2,
                &BTreeMap::new(),
                &BTreeSet::new(),
                0,
                &[1, 1, 1, 0],
            )
            .is_err()
        );
    }

    #[test]
    fn checkpoint_v6_requires_both_execution_states_and_v1_through_v5_forbid_them() {
        let checkpoint = valid_v6_checkpoint();
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_execution = checkpoint.clone();
        missing_execution.operational_execution = None;
        assert!(validate_checkpoint_shape(&missing_execution).is_err());
        let mut missing_air = checkpoint.clone();
        missing_air.air_power = None;
        assert!(validate_checkpoint_shape(&missing_air).is_err());
        let mut missing_operations = checkpoint.clone();
        missing_operations.operational_ai = None;
        assert!(validate_checkpoint_shape(&missing_operations).is_err());

        for legacy_schema in [
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
        ] {
            let mut legacy = checkpoint.clone();
            legacy.schema = legacy_schema.to_owned();
            assert!(
                validate_checkpoint_shape(&legacy).is_err(),
                "{legacy_schema} accepted v6 execution state"
            );
        }
    }

    #[test]
    fn checkpoint_v7_requires_naval_planning_and_v1_through_v6_forbid_it() {
        let checkpoint = valid_v7_checkpoint();
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_planning = checkpoint.clone();
        missing_planning.naval_planning = None;
        assert!(validate_checkpoint_shape(&missing_planning).is_err());

        for legacy_schema in [
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
        ] {
            let mut legacy = valid_legacy_checkpoint(legacy_schema);
            assert!(
                validate_checkpoint_shape(&legacy).is_ok(),
                "{legacy_schema} regression fixture is not valid"
            );
            legacy.naval_planning = checkpoint.naval_planning.clone();
            assert!(
                validate_checkpoint_shape(&legacy).is_err(),
                "{legacy_schema} accepted v7 naval planning state"
            );
        }
    }

    #[test]
    fn checkpoint_v8_requires_complete_v7_state_and_supply_history_wire() {
        let checkpoint = valid_v8_checkpoint();
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_planning = checkpoint.clone();
        missing_planning.naval_planning = None;
        assert!(validate_checkpoint_shape(&missing_planning).is_err());

        let valid_wire = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
            "battlefield": {"units": [{"supplyCollapsedTick": null}]}
        });
        assert!(validate_checkpoint_v8_supply_collapse_fields(&valid_wire).is_ok());

        let missing_wire = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
            "battlefield": {"units": [{}]}
        });
        assert!(validate_checkpoint_v8_supply_collapse_fields(&missing_wire).is_err());

        let legacy_wire = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA,
            "battlefield": {"units": [{"supplyCollapsedTick": null}]}
        });
        assert!(validate_checkpoint_v8_supply_collapse_fields(&legacy_wire).is_err());
    }

    #[test]
    fn checkpoint_v9_requires_strict_rng_and_complete_personnel_reserves() {
        let checkpoint = valid_v9_checkpoint();
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_rng = checkpoint.clone();
        missing_rng.gameplay_rng = None;
        assert!(validate_checkpoint_shape(&missing_rng).is_err());
        let mut wrong_algorithm = checkpoint.clone();
        wrong_algorithm.gameplay_rng.as_mut().unwrap().algorithm = "splitmix64".to_owned();
        assert!(validate_checkpoint_shape(&wrong_algorithm).is_err());
        let mut missing_reserves = checkpoint.clone();
        missing_reserves.personnel_reserves = None;
        assert!(validate_checkpoint_shape(&missing_reserves).is_err());
        let mut invalid_reserves = checkpoint.clone();
        invalid_reserves.personnel_reserves = Some(vec![f64::NAN]);
        assert!(validate_checkpoint_shape(&invalid_reserves).is_err());

        let mut v8_with_replay_state = checkpoint;
        v8_with_replay_state.schema = NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&v8_with_replay_state).is_err());

        let v9_wire = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA,
            "battlefield": {"units": [{"supplyCollapsedTick": null}]}
        });
        assert!(validate_checkpoint_v8_supply_collapse_fields(&v9_wire).is_ok());
    }

    #[test]
    fn checkpoint_v10_requires_reinforcement_and_v1_through_v9_forbid_it() {
        let checkpoint = valid_v10_checkpoint();
        assert!(validate_checkpoint_shape(&checkpoint).is_ok());

        let mut missing_reinforcement = checkpoint.clone();
        missing_reinforcement.reinforcement = None;
        assert!(validate_checkpoint_shape(&missing_reinforcement).is_err());

        for legacy_schema in [
            NATIVE_RUNTIME_CHECKPOINT_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V2_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V3_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V4_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V7_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V8_SCHEMA,
            NATIVE_RUNTIME_CHECKPOINT_V9_SCHEMA,
        ] {
            let mut legacy = checkpoint.clone();
            legacy.schema = legacy_schema.to_owned();
            assert!(
                validate_checkpoint_shape(&legacy).is_err(),
                "{legacy_schema} accepted v10 reinforcement state"
            );
        }

        let v10_wire = json!({
            "schema": NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA,
            "battlefield": {"units": [{"supplyCollapsedTick": null}]}
        });
        assert!(validate_checkpoint_v8_supply_collapse_fields(&v10_wire).is_ok());
    }

    #[test]
    fn checkpoint_v11_requires_material_logistics_and_v10_forbids_it() {
        let mut v11 = valid_v10_checkpoint();
        v11.schema = NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA.to_owned();
        v11.material_logistics = Some(
            serde_json::from_value(json!({
                "schema": "native-material-logistics-v1",
                "countries": [{"countryId": 1, "armorCapacity": 100, "reserveArmor": 0,
                    "armorQuality": 50.0, "armorReplacementSpent": 0.0, "airfieldRepairSpent": 0.0}]
            }))
            .unwrap(),
        );
        assert!(validate_checkpoint_shape(&v11).is_ok());
        let mut missing = v11.clone();
        missing.material_logistics = None;
        assert!(validate_checkpoint_shape(&missing).is_err());
        let mut invalid = v11.clone();
        invalid.material_logistics.as_mut().unwrap().schema = "invalid-material-schema".to_owned();
        assert!(validate_checkpoint_shape(&invalid).is_err());
        let mut forbidden = v11;
        forbidden.schema = NATIVE_RUNTIME_CHECKPOINT_V10_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&forbidden).is_err());
    }

    #[test]
    fn checkpoint_v12_requires_strategic_missiles_and_v11_forbids_them() {
        let mut v12 = valid_v10_checkpoint();
        v12.schema = NATIVE_RUNTIME_CHECKPOINT_V12_SCHEMA.to_owned();
        v12.material_logistics = Some(
            serde_json::from_value(json!({
                "schema": "native-material-logistics-v1",
                "countries": [{"countryId": 1, "armorCapacity": 100, "reserveArmor": 0,
                    "armorQuality": 50.0, "armorReplacementSpent": 0.0, "airfieldRepairSpent": 0.0}]
            }))
            .unwrap(),
        );
        v12.strategic_missiles = Some(mw_core::StrategicMissileState {
            schema: mw_core::STRATEGIC_MISSILE_SCHEMA_VERSION.to_owned(),
            enabled: true,
            technology_allowed: true,
            bases: vec![mw_core::MissileBase {
                lat: 0.0,
                lng: 0.0,
                side_index: 0,
            }],
            missiles: Vec::new(),
            explosions: Vec::new(),
        });
        assert!(validate_checkpoint_shape(&v12).is_ok());

        let mut missing = v12.clone();
        missing.strategic_missiles = None;
        assert!(validate_checkpoint_shape(&missing).is_err());

        let mut invalid = v12.clone();
        invalid.strategic_missiles.as_mut().unwrap().schema = "invalid-missiles".to_owned();
        assert!(validate_checkpoint_shape(&invalid).is_err());

        let mut invalid_side = v12.clone();
        invalid_side.strategic_missiles.as_mut().unwrap().bases[0].side_index = usize::MAX;
        assert!(validate_checkpoint_shape(&invalid_side).is_err());

        let mut strict_wire =
            serde_json::to_value(v12.strategic_missiles.as_ref().unwrap()).unwrap();
        strict_wire["bases"][0]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<mw_core::StrategicMissileState>(strict_wire).is_err());

        let mut forbidden = v12;
        forbidden.schema = NATIVE_RUNTIME_CHECKPOINT_V11_SCHEMA.to_owned();
        assert!(validate_checkpoint_shape(&forbidden).is_err());
    }

    #[test]
    fn checkpoint_v7_naval_planning_wire_is_strict_and_rejects_null() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OptionalNavalPlanning {
            #[serde(default, deserialize_with = "deserialize_optional_naval_planning")]
            naval_planning: Option<NavalPlanningState>,
        }

        let planning_wire = json!({
            "schema": "native-naval-planning-v1",
            "sideStates": [{"side": 0, "nextReassessTick": 0}],
            "nextOperationSequence": 1
        });
        let planning: NavalPlanningState = serde_json::from_value(planning_wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&planning).unwrap(), planning_wire);
        assert!(planning.validate(1).is_ok());

        let mut wrong_schema = planning.clone();
        wrong_schema.schema = "native-naval-planning-v0".to_owned();
        assert!(wrong_schema.validate(1).is_err());
        let mut missing_side = planning.clone();
        missing_side.side_states.clear();
        assert!(missing_side.validate(1).is_err());
        let mut zero_sequence = planning.clone();
        zero_sequence.next_operation_sequence = 0;
        assert!(zero_sequence.validate(1).is_err());

        let mut unknown_planning = planning_wire;
        unknown_planning["unknown"] = json!(true);
        assert!(serde_json::from_value::<NavalPlanningState>(unknown_planning).is_err());

        let absent: OptionalNavalPlanning = serde_json::from_value(json!({})).unwrap();
        assert!(absent.naval_planning.is_none());
        assert!(
            serde_json::from_value::<OptionalNavalPlanning>(json!({"navalPlanning": null}))
                .is_err()
        );
    }

    #[test]
    fn checkpoint_v6_execution_wire_round_trips_strictly_and_rejects_null() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OptionalExecution {
            #[serde(
                default,
                deserialize_with = "deserialize_optional_operational_execution"
            )]
            operational_execution: Option<OperationalExecutionState>,
            #[serde(default, deserialize_with = "deserialize_optional_air_power")]
            air_power: Option<AirPowerState>,
        }

        let execution = OperationalExecutionState::default();
        let execution_wire = serde_json::to_value(&execution).unwrap();
        let decoded_execution: OperationalExecutionState =
            serde_json::from_value(execution_wire.clone()).unwrap();
        assert_eq!(decoded_execution, execution);
        let mut unknown_execution = execution_wire;
        unknown_execution["unknown"] = json!(true);
        assert!(serde_json::from_value::<OperationalExecutionState>(unknown_execution).is_err());

        let air_power = AirPowerState::new(Vec::new(), Vec::new()).unwrap();
        let air_wire = serde_json::to_value(&air_power).unwrap();
        let decoded_air: AirPowerState = serde_json::from_value(air_wire.clone()).unwrap();
        assert_eq!(decoded_air, air_power);
        let mut unknown_air = air_wire;
        unknown_air["unknown"] = json!(true);
        assert!(serde_json::from_value::<AirPowerState>(unknown_air).is_err());

        let absent: OptionalExecution = serde_json::from_value(json!({})).unwrap();
        assert!(absent.operational_execution.is_none());
        assert!(absent.air_power.is_none());
        assert!(
            serde_json::from_value::<OptionalExecution>(json!({"operationalExecution": null}))
                .is_err()
        );
        assert!(serde_json::from_value::<OptionalExecution>(json!({"airPower": null})).is_err());
    }

    #[test]
    fn influence_runtime_block_is_strict_and_rejects_null() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OptionalInfluenceRuntime {
            #[serde(
                default,
                deserialize_with = "deserialize_optional_influence_runtime_fixture"
            )]
            influence_runtime: Option<InfluenceRuntimeFixture>,
        }

        let absent: OptionalInfluenceRuntime = serde_json::from_value(json!({})).unwrap();
        assert!(absent.influence_runtime.is_none());
        assert!(
            serde_json::from_value::<OptionalInfluenceRuntime>(json!({"influenceRuntime": null}))
                .is_err()
        );

        let encoded = serde_json::to_value(valid_influence_runtime()).unwrap();
        let present: OptionalInfluenceRuntime =
            serde_json::from_value(json!({"influenceRuntime": encoded.clone()})).unwrap();
        assert!(present.influence_runtime.is_some());

        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove("queuedCells");
        assert!(serde_json::from_value::<InfluenceRuntimeFixture>(missing).is_err());

        let mut unknown = encoded;
        unknown["cursor"] = json!(0);
        assert!(serde_json::from_value::<InfluenceRuntimeFixture>(unknown).is_err());
    }

    #[test]
    fn influence_runtime_validation_preserves_stale_and_duplicate_queue_entries() {
        let fixture = valid_influence_runtime();
        let decoded = decode_influence_runtime(&fixture, 4).unwrap();
        assert_eq!(decoded.regular_queue, fixture.regular_queue);
        assert_eq!(decoded.priority_queue, fixture.priority_queue);
        assert_eq!(decoded.queued_cells, fixture.queued_cells);
        assert_eq!(influence_runtime_fixture(&decoded, 4).unwrap(), fixture);

        let mut invalid = fixture.clone();
        invalid.schema = "native-influence-runtime-v0".to_owned();
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.regular_queue = vec![0; INFLUENCE_REGULAR_QUEUE_LIMIT + 1];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.priority_queue = vec![0; INFLUENCE_PRIORITY_QUEUE_LIMIT + 1];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.priority_queue.push(4);
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.queued_cells = vec![(2, 2), (1, 1)];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.queued_cells = vec![(1, 1), (1, 2)];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.queued_cells = vec![(1, 0)];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture.clone();
        invalid.queued_cells = vec![(2, 1)];
        assert!(decode_influence_runtime(&invalid, 4).is_err());

        invalid = fixture;
        invalid.queued_cells = vec![(1, 2)];
        invalid.priority_queue.retain(|cell| *cell != 1);
        assert!(decode_influence_runtime(&invalid, 4).is_err());
    }

    #[test]
    fn planner_block_is_strict_and_reference_checked() {
        let mut planner = PlannerFixture {
            objectives: vec![PlannerObjectiveFixture {
                id: 19,
                side_pair: [0, 1],
                segment_id: 23,
                lat: 1.0,
                lng: 2.0,
                capacity: 1,
                priority: 3,
            }],
            prior_objective_by_unit: BTreeMap::from([(7, 19)]),
            front_prior_by_unit: vec![FrontPriorFixture {
                unit_id: 7,
                pair_key: "0|1".to_owned(),
                segment_idx: 0,
                objective_id: 19,
            }],
            last_front_refresh_tick: Some(10),
        };
        let units = BTreeSet::from([7]);
        assert!(validate_planner_fixture(&planner, 10, 2, &units).is_ok());

        let encoded = serde_json::to_value(&planner).unwrap();
        assert_eq!(encoded["objectives"][0]["sidePair"], json!([0, 1]));
        assert!(encoded.get("frontPriorByUnit").is_some());
        let mut missing_field = encoded.clone();
        missing_field
            .as_object_mut()
            .unwrap()
            .remove("lastFrontRefreshTick");
        assert!(serde_json::from_value::<PlannerFixture>(missing_field).is_err());
        let mut unknown = encoded;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<PlannerFixture>(unknown).is_err());
        assert!(
            serde_json::from_str::<PlannerFixture>(
                r#"{
                    "objectives":[{"id":19,"sidePair":[0,1],"segmentId":23,"lat":1.0,"lng":2.0,"capacity":1,"priority":3}],
                    "priorObjectiveByUnit":{"7":19,"07":19},
                    "frontPriorByUnit":[],
                    "lastFrontRefreshTick":10
                }"#
            )
            .is_err()
        );

        let mut duplicate_objective = planner.clone();
        duplicate_objective
            .objectives
            .push(duplicate_objective.objectives[0]);
        assert!(validate_planner_fixture(&duplicate_objective, 10, 2, &units).is_err());

        let mut duplicate_unit = planner.clone();
        duplicate_unit
            .front_prior_by_unit
            .push(duplicate_unit.front_prior_by_unit[0].clone());
        assert!(validate_planner_fixture(&duplicate_unit, 10, 2, &units).is_err());

        planner.prior_objective_by_unit = BTreeMap::from([(8, 19)]);
        assert!(validate_planner_fixture(&planner, 10, 2, &units).is_err());
        planner.prior_objective_by_unit = BTreeMap::from([(7, 20)]);
        assert!(validate_planner_fixture(&planner, 10, 2, &units).is_err());
        planner.prior_objective_by_unit = BTreeMap::from([(7, 19)]);
        planner.last_front_refresh_tick = Some(11);
        assert!(validate_planner_fixture(&planner, 10, 2, &units).is_err());
    }

    #[test]
    fn checkpoint_writer_rejects_zero_continuation_steps() {
        assert!(validate_checkpoint_write_steps(0).is_err());
        assert!(validate_checkpoint_write_steps(1).is_ok());
        assert_eq!(
            checkpoint_output_parent(Path::new("save.json")),
            Path::new(".")
        );
        assert_eq!(
            checkpoint_output_parent(Path::new("saves/save.json")),
            Path::new("saves")
        );
    }

    #[test]
    fn checkpoint_v2_decodes_exact_rle_and_float_bits() {
        let decoded = decode_territory_v2(&valid_v2_territory(), 4, 2).unwrap();
        assert_eq!(decoded.land, [1, 1, 2, 2]);
        assert_eq!(decoded.world_control, [1, 1, 2, 2]);
        assert_eq!(decoded.de_jure, [1, 2, 2, 2]);
        assert_eq!(decoded.primary_occupier, [1, 1, 2, 2]);
        assert_eq!(decoded.dominant_side, [-1, 0, 1, 1]);
        assert_eq!(decoded.occupation, [0.25, 0.25, -0.5, -0.5]);
        assert_eq!(decoded.side_influence[0], [1.0, 1.0, 0.0, 0.0]);
        assert_eq!(decoded.side_influence[1], [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn battlefield_block_is_optional_but_present_object_is_strict_and_complete() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct OptionalBattlefield {
            #[serde(default, deserialize_with = "deserialize_optional_battlefield_fixture")]
            battlefield: Option<BattlefieldFixture>,
        }

        let absent: OptionalBattlefield = serde_json::from_value(json!({})).unwrap();
        assert!(absent.battlefield.is_none());
        assert!(
            serde_json::from_value::<OptionalBattlefield>(json!({"battlefield": null})).is_err()
        );

        let encoded = serde_json::to_value(valid_battlefield()).unwrap();
        let present: OptionalBattlefield =
            serde_json::from_value(json!({"battlefield": encoded.clone()})).unwrap();
        assert!(present.battlefield.is_some());

        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove("config");
        assert!(serde_json::from_value::<BattlefieldFixture>(missing).is_err());

        let mut unknown = encoded.clone();
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BattlefieldFixture>(unknown).is_err());

        let mut missing_nullable = encoded.clone();
        missing_nullable["units"][0]
            .as_object_mut()
            .unwrap()
            .remove("armorSupportLastTick");
        assert!(serde_json::from_value::<BattlefieldFixture>(missing_nullable).is_err());

        let mut legacy_unit = encoded.clone();
        legacy_unit["units"][0]
            .as_object_mut()
            .unwrap()
            .remove("supplyCollapsedTick");
        assert!(serde_json::from_value::<BattlefieldFixture>(legacy_unit).is_ok());

        let mut invalid_phase = encoded;
        invalid_phase["countries"][0]["warPhase"] = json!("STABLE");
        assert!(serde_json::from_value::<BattlefieldFixture>(invalid_phase).is_err());
    }

    #[test]
    fn battlefield_decode_is_exact_reference_checked_and_round_trips_core_state() {
        let grid = GridSpec {
            grid_res: 1.0,
            width: 2,
            height: 2,
        };
        let land = [2, 2, 2, 2];
        let topology = BTreeMap::from([(1, 0_usize)]);
        let state =
            decode_battlefield(&valid_battlefield(), grid, &land, 1, &topology, &[7], 10).unwrap();
        assert_eq!(
            state
                .terrain_intensity
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            [
                0.0_f32.to_bits(),
                0x3eaa_aaab,
                0x3eaa_aaab,
                0.0_f32.to_bits()
            ]
        );
        assert_eq!(state.countries[&1].war_phase, BattlefieldWarPhase::Stable);
        assert_eq!(state.units[&7].encircled_ticks, 61);
        assert_eq!(state.units[&7].armor_support_last_tick, Some(9));
        assert_eq!(state.units[&7].supply_collapsed_tick, Some(8));

        let encoded = battlefield_fixture(&state, 10).unwrap();
        assert_eq!(encoded.schema, BATTLEFIELD_SCHEMA_VERSION);
        assert!(matches!(
            encoded.countries[0].war_phase,
            BattlefieldWarPhaseFixture::Stalemate
        ));
        let restored = decode_battlefield(&encoded, grid, &land, 1, &topology, &[7], 10).unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn battlefield_decode_rejects_corruption_and_incomplete_coverage() {
        let grid = GridSpec {
            grid_res: 1.0,
            width: 2,
            height: 2,
        };
        let land = [2, 2, 2, 2];
        let topology = BTreeMap::from([(1, 0_usize)]);
        let decode = |fixture: &BattlefieldFixture| {
            decode_battlefield(fixture, grid, &land, 1, &topology, &[7], 10)
        };

        let mut invalid = valid_battlefield();
        invalid.schema = "battlefield-v0".to_owned();
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.terrain_intensity_bits_runs = vec![(2, 0), (2, 0)];
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.terrain_intensity_bits_runs = vec![(4, f32::NAN.to_bits())];
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.terrain_intensity_bits_runs = vec![(4, 1.01_f32.to_bits())];
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.countries.clear();
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.countries.push(invalid.countries[0]);
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.units.clear();
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.units[0].armor_support_last_tick = Some(11);
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.units[0].supply_collapsed_tick = Some(11);
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.urban_centers[0].cell = 1;
        assert!(decode(&invalid).is_err());

        invalid = valid_battlefield();
        invalid.urban_centers[0].id = 0;
        assert!(decode(&invalid).is_err());
    }

    #[test]
    fn checkpoint_v2_rejects_corrupt_rle_and_float_maps() {
        let mut invalid = valid_v2_territory();
        invalid.maps.land_runs = vec![(0, 1), (4, 0)];
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());

        invalid = valid_v2_territory();
        invalid.maps.world_control_runs = vec![(3, 1)];
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());

        invalid = valid_v2_territory();
        invalid.maps.dominant_side_runs = vec![(4, 2)];
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());

        invalid = valid_v2_territory();
        invalid.maps.occupation_bits_runs = vec![(4, f32::NAN.to_bits())];
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());

        invalid = valid_v2_territory();
        invalid.maps.side_influence_bits_runs[0] = vec![(4, (-1.0_f32).to_bits())];
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());

        invalid = valid_v2_territory();
        invalid.maps.side_influence_bits_runs.pop();
        assert!(decode_territory_v2(&invalid, 4, 2).is_err());
    }

    #[test]
    fn checkpoint_v2_requires_committed_advancing_census_markers() {
        assert!(validate_territory_markers(&valid_v2_territory()).is_ok());

        for mutate in [
            |territory: &mut TerritoryV2Fixture| territory.committed_census.generation = 0,
            |territory: &mut TerritoryV2Fixture| {
                territory.committed_census.generation = u64::MAX;
            },
            |territory: &mut TerritoryV2Fixture| {
                territory.committed_census.commit_sequence = 0;
            },
            |territory: &mut TerritoryV2Fixture| {
                territory.committed_census.processed_tiles = 0;
            },
            |territory: &mut TerritoryV2Fixture| {
                territory.committed_census.processed_items = 0;
            },
        ] {
            let mut invalid = valid_v2_territory();
            mutate(&mut invalid);
            assert!(validate_territory_markers(&invalid).is_err());
        }
    }

    #[test]
    fn geography_parser_is_strict_and_width_bounded() {
        let geography: GeographyFixture = serde_json::from_str(
            r#"{
                "landRuns": [[2, 1], [2, 0]],
                "worldControlRuns": [[1, 65535], [3, 0]],
                "deJureRuns": [[4, 1]]
            }"#,
        )
        .unwrap();
        assert_eq!(geography.land_runs, [(2, 1), (2, 0)]);
        assert_eq!(geography.world_control_runs, [(1, u16::MAX), (3, 0)]);
        assert_eq!(geography.de_jure_runs, [(4, 1)]);

        assert!(
            serde_json::from_str::<GeographyFixture>(
                r#"{
                    "landRuns": [[1, 0]],
                    "worldControlRuns": [[1, 65536]],
                    "deJureRuns": [[1, 0]]
                }"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<GeographyFixture>(
                r#"{
                    "landRuns": [[1, 0]],
                    "worldControlRuns": [[1, 0]],
                    "deJureRuns": [[1, 0]],
                    "unexpected": true
                }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn geography_decoder_requires_exact_transactional_coverage() {
        let geography = GeographyFixture {
            land_runs: vec![(2, 1), (2, 0)],
            world_control_runs: vec![(1, 1), (3, 2)],
            de_jure_runs: vec![(3, 7), (1, 9)],
        };
        let decoded = decode_geography(&geography, 4).unwrap();
        assert_eq!(decoded.land, [1, 1, 0, 0]);
        assert_eq!(decoded.world_control, [1, 2, 2, 2]);
        assert_eq!(decoded.de_jure, [7, 7, 7, 9]);

        let mut invalid = geography.clone();
        invalid.land_runs = vec![(4, 2)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(0, 1), (4, 0)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(3, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(5, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
        invalid.land_runs = vec![(u64::MAX, 1)];
        assert!(decode_geography(&invalid, 4).is_err());
    }

    #[test]
    fn production_boundary_requires_exact_geography() {
        let geography = GeographyFixture {
            land_runs: vec![(1, 0)],
            world_control_runs: vec![(1, 0)],
            de_jure_runs: vec![(1, 0)],
        };
        assert!(validate_geography_boundary(CheckpointBoundary::PostStartWar, None).is_err());
        assert!(
            validate_geography_boundary(CheckpointBoundary::PostStartWar, Some(&geography)).is_ok()
        );
        assert!(validate_geography_boundary(CheckpointBoundary::BaselineReplay, None).is_ok());
    }

    #[test]
    fn exact_geography_rejects_unknown_metadata_owners_even_at_sea() {
        let known = BTreeSet::from([1, 2]);
        assert!(validate_exact_geography_owner_ids(&[0, 1, 2], &[2, 1, 0], &known).is_ok());
        assert!(validate_exact_geography_owner_ids(&[0, 99], &[0, 1], &known).is_err());
        assert!(validate_exact_geography_owner_ids(&[0, 1], &[99, 0], &known).is_err());
    }

    #[test]
    fn checkpoint_rle_is_stable_and_merges_adjacent_values() {
        assert_eq!(rle(&[1_u8, 1, 2, 2, 2, 1]), vec![(2, 1), (3, 2), (1, 1)]);
        assert_eq!(rle(&[0x8000_0000_u32, 0x8000_0000]), vec![(2, 0x8000_0000)]);
    }

    #[test]
    fn checkpoint_unit_json_contains_policy_contract() {
        let unit = SimulationUnit {
            combat: CombatUnit {
                id: 7,
                side: 1,
                sovereign: 42,
                kind: UnitKind::Armor,
                lat: 1.0,
                lng: 2.0,
                health: 3.0,
                max_health: 4.0,
                personnel: 5,
                personnel_capacity: 6,
                equipment: 7,
                max_equipment: 8,
                quality: 9.0,
                transport: false,
                armor_supported: true,
                landing_penalty_active: false,
                at_sea: false,
                last_combat_tick: 10,
                victory_boost_ticks: 11,
            },
            dir_lat: 0.1,
            dir_lng: 0.2,
            coast_stuck_ticks: 3,
            armor_landing_penalty_until_tick: 12,
            is_support: false,
            ally_weight: 1.0,
        };
        let mut influence = UnitInfluencePolicy {
            browser_temporal_seed: Some(4.5),
            ..UnitInfluencePolicy::default()
        };
        influence.owner_ally_country_ids.insert(42);
        let policy = RuntimeUnitPolicy {
            unit_id: 7,
            ai: UnitAiPolicy {
                is_reserve: true,
                deploy_until_tick: 13,
                ..UnitAiPolicy::default()
            },
            command: UnitCommandPolicy::paid(7.0, 42),
            influence: Some(influence),
        };
        let value = unit_json(&unit, &policy);
        assert_eq!(value["aiPolicy"]["isReserve"], true);
        assert_eq!(value["aiPolicy"]["deployUntilTick"], 13);
        assert_eq!(value["influencePolicy"]["temporalSeed"], 4.5);
        assert!(
            value["influencePolicy"]
                .as_object()
                .is_some_and(|policy| !policy.contains_key("ownerAllyCountryIds"))
        );
        let parsed: RuntimeUnitFixture = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.ai_policy.deploy_until_tick, 13);
        assert_eq!(
            parsed
                .influence_policy
                .and_then(|policy| policy.temporal_seed),
            Some(4.5)
        );
    }

    #[test]
    fn checkpoint_casualty_coverage_omits_invalid_self_pairs() {
        let countries = BTreeMap::from([(7, 0_usize), (11, 1_usize)]);
        let nested = covered_nested_casualties(&BTreeMap::new(), &countries);
        assert_eq!(nested[&7], BTreeMap::from([(11, 0.0)]));
        assert_eq!(nested[&11], BTreeMap::from([(7, 0.0)]));
    }
}
