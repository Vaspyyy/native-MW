//! Read-only projection of immutable runtime publications into browser-style map overlays.

use std::collections::BTreeMap;

use bytemuck::{Pod, Zeroable};
use mw_core::{
    AirPowerState, AirRole, AirWingState, FrameSnapshot, IntelStatus, NavalOperationPhase,
    OperationalContact, OperationalExecutionState, OperationalPoint, OperationalSnapshot, Point,
    RuntimeSnapshot, TaskForcePhase,
};

use crate::unit_renderer::geographic_to_world;

const MARKER_AIRFIELD: u32 = 0;
const MARKER_FIGHTER: u32 = 1;
const MARKER_STRIKE: u32 = 2;
const MARKER_BATTLE: u32 = 3;
const MARKER_CONTACT: u32 = 4;
const MARKER_ANCHOR: u32 = 5;

const FLAG_DISABLED_OR_STALE: u32 = 1;
const FLAG_ANCHOR_TARGET: u32 = 1;
const FLAG_ANCHOR_WITHDRAWAL: u32 = 2;
const FLAG_DASHED: u32 = 1;

const SIDE_COLORS: [[f32; 4]; 8] = [
    [1.0, 50.0 / 255.0, 50.0 / 255.0, 0.95],
    [50.0 / 255.0, 100.0 / 255.0, 1.0, 0.95],
    [1.0, 200.0 / 255.0, 0.0, 0.95],
    [0.0, 200.0 / 255.0, 100.0 / 255.0, 0.95],
    [180.0 / 255.0, 50.0 / 255.0, 220.0 / 255.0, 0.95],
    [1.0, 130.0 / 255.0, 0.0, 0.95],
    [0.0, 210.0 / 255.0, 210.0 / 255.0, 0.95],
    [200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0, 0.95],
];

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct MarkerInstance {
    world: [f32; 2],
    color: [f32; 4],
    size: f32,
    value: f32,
    kind: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct SegmentInstance {
    start: [f32; 2],
    end: [f32; 2],
    color: [f32; 4],
    width: f32,
    flags: u32,
}

#[derive(Default)]
struct OverlayProjection {
    markers: Vec<MarkerInstance>,
    segments: Vec<SegmentInstance>,
    air_markers: [usize; 2],
    battle_markers: [usize; 2],
    operation_markers: [usize; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BattleCluster {
    lat: f64,
    lng: f64,
    participants: u32,
}

pub struct WorldOverlayRenderer {
    marker_pipeline: wgpu::RenderPipeline,
    segment_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    marker_buffer: wgpu::Buffer,
    segment_buffer: wgpu::Buffer,
    marker_capacity: usize,
    segment_capacity: usize,
    marker_count: u32,
    segment_count: u32,
    air_markers: [u32; 2],
    battle_markers: [u32; 2],
    operation_markers: [u32; 2],
    markers: Vec<MarkerInstance>,
    segments: Vec<SegmentInstance>,
}

impl WorldOverlayRenderer {
    pub fn new(
        device: &wgpu::Device,
        view_buffer: &wgpu::Buffer,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("world_overlay.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world overlay bindings"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world overlay bind group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world overlay pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let target = Some(wgpu::ColorTargetState {
            format: surface_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        });
        let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world overlay marker pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_marker"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MarkerInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32,
                        4 => Float32,
                        5 => Uint32,
                        6 => Uint32
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_marker"),
                targets: std::slice::from_ref(&target),
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let segment_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world overlay segment pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_segment"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SegmentInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        1 => Float32x2,
                        2 => Float32x2,
                        3 => Float32x4,
                        4 => Float32,
                        5 => Uint32
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_segment"),
                targets: &[target],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let marker_buffer = empty_buffer::<MarkerInstance>(device, "world overlay markers");
        let segment_buffer = empty_buffer::<SegmentInstance>(device, "world overlay segments");
        Self {
            marker_pipeline,
            segment_pipeline,
            bind_group,
            marker_buffer,
            segment_buffer,
            marker_capacity: 1,
            segment_capacity: 1,
            marker_count: 0,
            segment_count: 0,
            air_markers: [0; 2],
            battle_markers: [0; 2],
            operation_markers: [0; 2],
            markers: Vec::new(),
            segments: Vec::new(),
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &RuntimeSnapshot,
        selected_side: Option<usize>,
    ) {
        let projection = project_world_overlays(snapshot, selected_side);
        self.markers = projection.markers;
        self.segments = projection.segments;

        if self.markers.len() > self.marker_capacity {
            self.marker_capacity = self.markers.len().next_power_of_two();
            self.marker_buffer = sized_buffer::<MarkerInstance>(
                device,
                "world overlay markers",
                self.marker_capacity,
            );
        }
        if self.segments.len() > self.segment_capacity {
            self.segment_capacity = self.segments.len().next_power_of_two();
            self.segment_buffer = sized_buffer::<SegmentInstance>(
                device,
                "world overlay segments",
                self.segment_capacity,
            );
        }
        if !self.markers.is_empty() {
            queue.write_buffer(&self.marker_buffer, 0, bytemuck::cast_slice(&self.markers));
        }
        if !self.segments.is_empty() {
            queue.write_buffer(
                &self.segment_buffer,
                0,
                bytemuck::cast_slice(&self.segments),
            );
        }
        self.marker_count = self.markers.len() as u32;
        self.segment_count = self.segments.len() as u32;
        self.air_markers = projection.air_markers.map(|index| index as u32);
        self.battle_markers = projection.battle_markers.map(|index| index as u32);
        self.operation_markers = projection.operation_markers.map(|index| index as u32);
    }

    pub fn draw_air<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_marker_range(pass, self.air_markers);
    }

    pub fn draw_battles<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_marker_range(pass, self.battle_markers);
    }

    pub fn draw_operations<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.segment_count > 0 {
            pass.set_pipeline(&self.segment_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.segment_buffer.slice(..));
            pass.draw(0..6, 0..self.segment_count);
        }
        self.draw_marker_range(pass, self.operation_markers);
    }

    fn draw_marker_range<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, range: [u32; 2]) {
        if range[0] < range[1] {
            pass.set_pipeline(&self.marker_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.marker_buffer.slice(..));
            pass.draw(0..6, range[0]..range[1]);
        }
    }

    pub fn marker_count(&self) -> u32 {
        self.marker_count
    }

    pub fn segment_count(&self) -> u32 {
        self.segment_count
    }
}

fn empty_buffer<T>(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    sized_buffer::<T>(device, label, 1)
}

fn sized_buffer<T>(device: &wgpu::Device, label: &str, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (capacity.max(1) * std::mem::size_of::<T>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn project_world_overlays(
    snapshot: &RuntimeSnapshot,
    selected_side: Option<usize>,
) -> OverlayProjection {
    let mut projection = OverlayProjection::default();
    append_air_markers(
        snapshot.air_power_snapshot.as_deref(),
        &mut projection.markers,
    );
    projection.air_markers = [0, projection.markers.len()];
    let battle_start = projection.markers.len();
    append_battle_markers(
        &snapshot.frame_snapshot,
        snapshot.frame,
        &mut projection.markers,
    );
    projection.battle_markers = [battle_start, projection.markers.len()];
    let operation_start = projection.markers.len();
    if let (Some(side), Some(operations)) =
        (selected_side, snapshot.operational_snapshot.as_deref())
    {
        append_operations(operations, side, &mut projection);
    }
    if let (Some(side), Some(execution)) = (
        selected_side,
        snapshot.operational_execution_snapshot.as_deref(),
    ) {
        append_execution(execution, side, &mut projection);
    }
    projection.operation_markers = [operation_start, projection.markers.len()];
    projection
}

fn append_air_markers(air: Option<&AirPowerState>, markers: &mut Vec<MarkerInstance>) {
    let Some(air) = air else {
        return;
    };
    markers.extend(air.airfields.iter().map(|field| MarkerInstance {
        world: geographic_to_world(field.lat, field.lng),
        color: side_color(field.side),
        size: 9.0,
        value: (field.health / 100.0).clamp(0.0, 1.0) as f32,
        kind: MARKER_AIRFIELD,
        flags: u32::from(field.disabled || field.health <= 0.0),
    }));
    markers.extend(
        air.wings
            .iter()
            .filter(|wing| air_wing_visible(wing.count, wing.state))
            .map(|wing| MarkerInstance {
                world: geographic_to_world(wing.lat, wing.lng),
                color: side_color(wing.side),
                size: 9.0,
                value: if wing.max_count == 0 {
                    0.0
                } else {
                    wing.count as f32 / wing.max_count as f32
                },
                kind: match wing.role {
                    AirRole::Fighter => MARKER_FIGHTER,
                    AirRole::Strike => MARKER_STRIKE,
                },
                flags: 0,
            }),
    );
}

fn air_wing_visible(count: u32, state: AirWingState) -> bool {
    count > 0
        && !matches!(
            state,
            AirWingState::Grounded | AirWingState::Rearming | AirWingState::Evacuated
        )
}

fn append_battle_markers(snapshot: &FrameSnapshot, frame: u64, markers: &mut Vec<MarkerInstance>) {
    let pulse = 0.9 + ((frame as f64 * 0.2).sin() * 0.1) as f32;
    markers.extend(battle_clusters(snapshot).into_iter().map(|battle| {
        let size_multiplier = (1.0 + battle.participants as f32 / 15.0).min(2.0);
        MarkerInstance {
            world: geographic_to_world(battle.lat, battle.lng),
            color: [1.0, 0.82, 0.28, 0.96],
            size: 13.0 * size_multiplier * pulse,
            value: (battle.participants as f32 / 15.0).clamp(0.0, 1.0),
            kind: MARKER_BATTLE,
            flags: 0,
        }
    }));
}

fn battle_clusters(snapshot: &FrameSnapshot) -> Vec<BattleCluster> {
    let positions = snapshot
        .units
        .iter()
        .map(|unit| (unit.id, (unit.lat, unit.lng)))
        .collect::<BTreeMap<_, _>>();
    let mut clusters = Vec::<BattleCluster>::new();
    for event in snapshot.events.iter() {
        let attacker = positions.get(&event.attacker_id).copied();
        let target = positions.get(&event.target_id).copied();
        let Some((lat, lng)) = combat_midpoint(attacker, target) else {
            continue;
        };
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            let delta_lng = wrapped_longitude_delta(lng, cluster.lng);
            (lat - cluster.lat).powi(2) + delta_lng.powi(2) < 0.16
        }) {
            cluster.participants = cluster.participants.saturating_add(1);
            let weight = f64::from(cluster.participants);
            cluster.lat = (cluster.lat * (weight - 1.0) + lat) / weight;
            let delta = wrapped_longitude_delta(lng, cluster.lng);
            cluster.lng = normalize_longitude(cluster.lng + delta / weight);
        } else {
            clusters.push(BattleCluster {
                lat,
                lng,
                participants: 2,
            });
        }
    }
    clusters
}

fn combat_midpoint(attacker: Option<(f64, f64)>, target: Option<(f64, f64)>) -> Option<(f64, f64)> {
    match (attacker, target) {
        (Some((attacker_lat, attacker_lng)), Some((target_lat, target_lng))) => Some((
            (attacker_lat + target_lat) * 0.5,
            normalize_longitude(
                attacker_lng + wrapped_longitude_delta(target_lng, attacker_lng) * 0.5,
            ),
        )),
        (Some(position), None) | (None, Some(position)) => Some(position),
        (None, None) => None,
    }
}

fn append_operations(
    operations: &OperationalSnapshot,
    selected_side: usize,
    projection: &mut OverlayProjection,
) {
    for task_force in operations
        .task_forces
        .iter()
        .filter(|task_force| {
            task_force_visible(task_force.side_index, task_force.phase, selected_side)
        })
        .take(6)
    {
        let color = side_color(selected_side);
        let mut route = Vec::with_capacity(task_force.route.len() + 2);
        push_distinct_point(&mut route, task_force.staging_anchor);
        for point in &task_force.route {
            push_distinct_point(&mut route, Some(*point));
        }
        push_distinct_point(&mut route, task_force.target);
        let dashed = matches!(
            task_force.phase,
            TaskForcePhase::Assembling | TaskForcePhase::Regrouping
        );
        for points in route.windows(2) {
            append_geographic_segment(
                &mut projection.segments,
                points[0].lat,
                points[0].lng,
                points[1].lat,
                points[1].lng,
                color,
                u32::from(dashed) * FLAG_DASHED,
            );
        }
        append_anchor(projection, task_force.staging_anchor, color, 0);
        append_anchor(projection, task_force.target, color, FLAG_ANCHOR_TARGET);
        append_anchor(
            projection,
            task_force.withdrawal_anchor,
            color,
            FLAG_ANCHOR_WITHDRAWAL,
        );
    }

    let Some(side) = operations
        .sides
        .iter()
        .find(|side| side.side_index == selected_side)
    else {
        return;
    };
    projection.markers.extend(
        prioritized_contacts(&side.intel.contacts)
            .into_iter()
            .take(18)
            .map(|contact| {
                let confidence = contact.confidence.clamp(0.15, 1.0);
                let age_fade = (1.0 / (1.0 + contact.age_ticks as f64 / 180.0)).max(0.18);
                let stale = contact.status != IntelStatus::Fresh;
                let alpha = (confidence * age_fade).min(if stale { 0.34 } else { 0.88 }) as f32;
                let power = estimated_contact_power(contact);
                let radius = (4.0 + (power + 1.0).log10()).clamp(4.0, 9.0);
                let prediction_ticks = contact.age_ticks.min(side.intel.config.stale_ticks) as f64;
                let lat =
                    (contact.lat + contact.velocity_lat * prediction_ticks).clamp(-90.0, 90.0);
                let lng =
                    normalize_longitude(contact.lng + contact.velocity_lng * prediction_ticks);
                MarkerInstance {
                    world: geographic_to_world(lat, lng),
                    color: [1.0, 173.0 / 255.0, 66.0 / 255.0, alpha],
                    size: radius as f32 + 3.0,
                    value: confidence as f32,
                    kind: MARKER_CONTACT,
                    flags: u32::from(stale) * FLAG_DISABLED_OR_STALE,
                }
            }),
    );
}

fn append_execution(
    execution: &OperationalExecutionState,
    selected_side: usize,
    projection: &mut OverlayProjection,
) {
    for operation in execution
        .naval_operations
        .iter()
        .filter(|operation| naval_operation_visible(operation.side, operation.phase, selected_side))
        .take(6)
    {
        let color = side_color(selected_side);
        let mut route = Vec::with_capacity(operation.route.len() + 2);
        push_distinct_execution_point(&mut route, operation.staging);
        for point in &operation.route {
            push_distinct_execution_point(&mut route, *point);
        }
        push_distinct_execution_point(&mut route, operation.target);
        let dashed = matches!(
            operation.phase,
            NavalOperationPhase::Gathering | NavalOperationPhase::Embarkation
        );
        for points in route.windows(2) {
            append_geographic_segment(
                &mut projection.segments,
                points[0].lat,
                points[0].lng,
                points[1].lat,
                points[1].lng,
                color,
                u32::from(dashed) * FLAG_DASHED,
            );
        }
        append_execution_anchor(projection, operation.staging, color, 0);
        append_execution_anchor(projection, operation.target, color, FLAG_ANCHOR_TARGET);
    }

    // Defender reactions do not own a route; their target remains useful as a
    // selected-side warning anchor while the assigned land units converge.
    for reaction in execution
        .defender_reactions
        .iter()
        .filter(|reaction| reaction.side == selected_side)
        .take(6)
    {
        append_execution_anchor(
            projection,
            reaction.target,
            [1.0, 0.68, 0.26, 0.92],
            FLAG_ANCHOR_WITHDRAWAL,
        );
    }
}

fn task_force_visible(side: usize, phase: TaskForcePhase, selected_side: usize) -> bool {
    side == selected_side && phase != TaskForcePhase::Complete
}

fn naval_operation_visible(side: usize, phase: NavalOperationPhase, selected_side: usize) -> bool {
    side == selected_side && phase != NavalOperationPhase::Complete
}

fn prioritized_contacts(contacts: &[OperationalContact]) -> Vec<&OperationalContact> {
    let mut contacts = contacts.iter().collect::<Vec<_>>();
    contacts.sort_by(|left, right| {
        estimated_contact_power(right)
            .total_cmp(&estimated_contact_power(left))
            .then_with(|| left.key.cmp(&right.key))
    });
    contacts
}

fn estimated_contact_power(contact: &OperationalContact) -> f64 {
    (contact.observed_power * (0.6 + contact.confidence.clamp(0.0, 1.0) * 0.4)).max(0.0)
}

fn append_geographic_segment(
    segments: &mut Vec<SegmentInstance>,
    start_lat: f64,
    start_lng: f64,
    end_lat: f64,
    end_lng: f64,
    color: [f32; 4],
    flags: u32,
) {
    let raw_delta = end_lng - start_lng;
    if raw_delta.abs() <= 180.0 {
        push_segment(
            segments, start_lat, start_lng, end_lat, end_lng, color, flags,
        );
        return;
    }

    let wrapped_delta = wrapped_longitude_delta(end_lng, start_lng);
    if wrapped_delta.abs() <= f64::EPSILON {
        return;
    }
    let boundary = if wrapped_delta > 0.0 { 180.0 } else { -180.0 };
    let progress = ((boundary - start_lng) / wrapped_delta).clamp(0.0, 1.0);
    let crossing_lat = start_lat + (end_lat - start_lat) * progress;
    push_segment(
        segments,
        start_lat,
        start_lng,
        crossing_lat,
        boundary,
        color,
        flags,
    );
    push_segment(
        segments,
        crossing_lat,
        -boundary,
        end_lat,
        end_lng,
        color,
        flags,
    );
}

fn push_segment(
    segments: &mut Vec<SegmentInstance>,
    start_lat: f64,
    start_lng: f64,
    end_lat: f64,
    end_lng: f64,
    color: [f32; 4],
    flags: u32,
) {
    let start = geographic_to_world(start_lat, start_lng);
    let end = geographic_to_world(end_lat, end_lng);
    if start == end {
        return;
    }
    segments.push(SegmentInstance {
        start,
        end,
        color,
        width: 2.5,
        flags,
    });
}

fn push_distinct_point(route: &mut Vec<OperationalPoint>, point: Option<OperationalPoint>) {
    let Some(point) = point else {
        return;
    };
    if route.last().is_none_or(|last| {
        last.lat.to_bits() != point.lat.to_bits() || last.lng.to_bits() != point.lng.to_bits()
    }) {
        route.push(point);
    }
}

fn push_distinct_execution_point(route: &mut Vec<Point>, point: Point) {
    if route.last().is_none_or(|last| {
        last.lat.to_bits() != point.lat.to_bits() || last.lng.to_bits() != point.lng.to_bits()
    }) {
        route.push(point);
    }
}

fn append_anchor(
    projection: &mut OverlayProjection,
    point: Option<OperationalPoint>,
    color: [f32; 4],
    flags: u32,
) {
    if let Some(point) = point {
        projection.markers.push(MarkerInstance {
            world: geographic_to_world(point.lat, point.lng),
            color,
            size: 8.0,
            value: 1.0,
            kind: MARKER_ANCHOR,
            flags,
        });
    }
}

fn append_execution_anchor(
    projection: &mut OverlayProjection,
    point: Point,
    color: [f32; 4],
    flags: u32,
) {
    projection.markers.push(MarkerInstance {
        world: geographic_to_world(point.lat, point.lng),
        color,
        size: 8.0,
        value: 1.0,
        kind: MARKER_ANCHOR,
        flags,
    });
}

fn side_color(side: usize) -> [f32; 4] {
    SIDE_COLORS[side % SIDE_COLORS.len()]
}

fn wrapped_longitude_delta(value: f64, origin: f64) -> f64 {
    let mut delta = value - origin;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

fn normalize_longitude(value: f64) -> f64 {
    (value + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mw_core::{CombatEvent, CombatLayer, UnitKind, UnitSnapshot};

    use super::*;

    fn unit(id: u64, lat: f64, lng: f64) -> UnitSnapshot {
        UnitSnapshot {
            id,
            side: (id % 2) as u16,
            sovereign: id,
            kind: UnitKind::Army,
            lat,
            lng,
            health: 100.0,
            max_health: 100.0,
            health_fraction: 1.0,
            personnel: 1_000,
            personnel_capacity: 1_000,
            equipment: 0,
            max_equipment: 0,
            dir_lat: 0.0,
            dir_lng: 0.0,
            coast_stuck_ticks: 0,
            last_combat_tick: 1,
            victory_boost_ticks: 0,
            landing_penalty_active: false,
            transport: false,
            at_sea: false,
        }
    }

    fn event(attacker_id: u64, target_id: u64) -> CombatEvent {
        CombatEvent {
            schema_version: "1",
            layer: CombatLayer::Proximity,
            attacker_id,
            target_id,
            target_damage: 1.0,
            attacker_damage: 1.0,
            transport_self_damage: 0.0,
            target_personnel_loss: 1,
            attacker_personnel_loss: 1,
            target_equipment_loss: 0,
            attacker_equipment_loss: 0,
            target_resulting_health: 99.0,
            attacker_resulting_health: 99.0,
            target_knockback_blocked: false,
            attacker_knockback_blocked: false,
        }
    }

    fn contact(key: &str, observed_power: f64) -> OperationalContact {
        OperationalContact {
            key: key.to_owned(),
            enemy_side_index: 1,
            sector_id: "sector".to_owned(),
            unit_id: 1,
            country_id: None,
            domain: "land".to_owned(),
            kind: "army".to_owned(),
            lat: 0.0,
            lng: 0.0,
            velocity_lat: 0.0,
            velocity_lng: 0.0,
            observed_power,
            base_confidence: 1.0,
            confidence: 1.0,
            observed_tick: 1,
            age_ticks: 0,
            status: IntelStatus::Fresh,
            source: "test".to_owned(),
        }
    }

    fn frame(units: Vec<UnitSnapshot>, events: Vec<CombatEvent>) -> FrameSnapshot {
        FrameSnapshot {
            schema_version: "native-tick-v1",
            tick: 1,
            frame: 1,
            units: Arc::from(units),
            events: Arc::from(events),
            removed_ids: Arc::from([]),
            abandoned_ids: Arc::from([]),
        }
    }

    #[test]
    fn browser_side_palette_is_stable_and_wraps() {
        assert_eq!(side_color(0), [1.0, 50.0 / 255.0, 50.0 / 255.0, 0.95]);
        assert_eq!(side_color(8), side_color(0));
        assert_eq!(side_color(9), side_color(1));
    }

    #[test]
    fn only_live_map_aircraft_are_visible() {
        assert!(air_wing_visible(5, AirWingState::Patrol));
        assert!(air_wing_visible(5, AirWingState::Attacking));
        assert!(!air_wing_visible(0, AirWingState::Patrol));
        assert!(!air_wing_visible(5, AirWingState::Grounded));
        assert!(!air_wing_visible(5, AirWingState::Rearming));
        assert!(!air_wing_visible(5, AirWingState::Evacuated));
    }

    #[test]
    fn nearby_combat_events_form_one_weighted_cluster() {
        let snapshot = frame(
            vec![
                unit(1, 10.0, 20.0),
                unit(2, 10.1, 20.1),
                unit(3, 10.2, 20.2),
            ],
            vec![event(1, 2), event(2, 3)],
        );
        let clusters = battle_clusters(&snapshot);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].participants, 3);
        assert!((10.0..=10.2).contains(&clusters[0].lat));
        assert!((20.0..=20.2).contains(&clusters[0].lng));
    }

    #[test]
    fn battle_midpoint_wraps_across_the_antimeridian() {
        let (_, lng) = combat_midpoint(Some((0.0, 179.0)), Some((0.0, -179.0))).unwrap();
        assert_eq!(lng.abs(), 180.0);
    }

    #[test]
    fn operation_visibility_is_selected_side_only_and_excludes_complete() {
        assert!(task_force_visible(2, TaskForcePhase::Attacking, 2));
        assert!(!task_force_visible(1, TaskForcePhase::Attacking, 2));
        assert!(!task_force_visible(2, TaskForcePhase::Complete, 2));
        assert!(naval_operation_visible(2, NavalOperationPhase::Transit, 2));
        assert!(!naval_operation_visible(1, NavalOperationPhase::Transit, 2));
        assert!(!naval_operation_visible(
            2,
            NavalOperationPhase::Complete,
            2
        ));
    }

    #[test]
    fn duplicate_route_points_are_removed() {
        let point = OperationalPoint { lat: 1.0, lng: 2.0 };
        let mut route = Vec::new();
        push_distinct_point(&mut route, Some(point));
        push_distinct_point(&mut route, Some(point));
        assert_eq!(route, [point]);
    }

    #[test]
    fn strongest_contacts_win_the_browser_marker_cap_order() {
        let mut raw_stronger = contact("raw-stronger", 100.0);
        raw_stronger.confidence = 0.0;
        let contacts = vec![
            contact("low", 1.0),
            raw_stronger,
            contact("estimated-stronger", 70.0),
            contact("z-tie", 10.0),
            contact("a-tie", 10.0),
        ];
        let keys = prioritized_contacts(&contacts)
            .into_iter()
            .map(|contact| contact.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "estimated-stronger",
                "raw-stronger",
                "a-tie",
                "z-tie",
                "low"
            ]
        );
    }

    #[test]
    fn antimeridian_routes_split_at_both_map_edges() {
        let mut segments = Vec::new();
        append_geographic_segment(&mut segments, 10.0, 179.0, 12.0, -179.0, side_color(0), 0);
        assert_eq!(segments.len(), 2);
        assert!(
            segments
                .iter()
                .all(|segment| (segment.end[0] - segment.start[0]).abs() < 0.02)
        );
        assert_eq!(segments[0].end[0], 2.0);
        assert_eq!(segments[1].start[0], 0.0);
    }

    #[test]
    fn generated_marker_data_stays_finite() {
        let snapshot = frame(
            vec![unit(1, 0.0, 0.0), unit(2, 0.1, 0.1)],
            vec![event(1, 2)],
        );
        let mut markers = Vec::new();
        append_battle_markers(&snapshot, 7, &mut markers);
        assert_eq!(markers.len(), 1);
        assert!(markers[0].world.into_iter().all(f32::is_finite));
        assert!(markers[0].color.into_iter().all(f32::is_finite));
        assert!(markers[0].size.is_finite() && markers[0].size > 0.0);
    }
}
