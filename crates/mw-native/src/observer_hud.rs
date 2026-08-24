//! Dependency-free procedural observer HUD rendering.

use bytemuck::{Pod, Zeroable};
use winit::dpi::{PhysicalPosition, PhysicalSize};

const PANEL_WIDTH: f32 = 440.0;
const PANEL_MIN_HEIGHT: f32 = 80.0;
const PANEL_MAX_HEIGHT: f32 = 640.0;
const PANEL_MARGIN: f32 = 12.0;
const PANEL_PADDING: f32 = 10.0;
const TOP_BAR_HEIGHT: f32 = 44.0;
const TOP_BAR_MARGIN: f32 = 16.0;
const CONTROL_HEIGHT: f32 = 30.0;
const CONTROL_GAP: f32 = 4.0;
const CONTROL_RIGHT_MARGIN: f32 = 12.0;
const CONTROL_WIDTHS: [f32; 4] = [32.0, 30.0, 38.0, 30.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackAction {
    TogglePause,
    SpeedDown,
    CycleSpeed,
    SpeedUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudHit {
    Playback(PlaybackAction),
    Panel,
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackPresentation {
    pub paused: bool,
    pub speed: u8,
    pub unit_count: usize,
    pub hovered: Option<PlaybackAction>,
    pub pressed: Option<PlaybackAction>,
}

pub struct ObserverHudUpload<'a> {
    pub lines: &'a [String],
    pub accent: [f32; 4],
    pub playback: Option<PlaybackPresentation>,
    pub show_observer: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HudLayout {
    pub top_bar: PanelBounds,
    pub observer_panel: PanelBounds,
    pub status_text: PanelBounds,
    pub unit_text: PanelBounds,
    pub playback_buttons: [PanelBounds; 4],
    pub playback_active: bool,
}

impl HudLayout {
    pub fn button(self, action: PlaybackAction) -> PanelBounds {
        self.playback_buttons[action_index(action)]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PanelBounds {
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
}

/// Returns the fixed top-right HUD bounds, clamped to the available surface.
pub fn panel_bounds(viewport: PhysicalSize<u32>, line_count: usize) -> PanelBounds {
    let viewport_width = viewport.width as f32;
    let viewport_height = viewport.height as f32;
    if viewport_width == 0.0 || viewport_height == 0.0 {
        return PanelBounds::default();
    }

    let margin = PANEL_MARGIN
        .min(viewport_width * 0.05)
        .min(viewport_height * 0.05);
    let width = PANEL_WIDTH.min((viewport_width - margin * 2.0).max(0.0));
    let desired_height = (PANEL_PADDING * 2.0 + line_count.max(1) as f32 * 18.0)
        .clamp(PANEL_MIN_HEIGHT, PANEL_MAX_HEIGHT);
    let top_margin = TOP_BAR_MARGIN.min(viewport_height * 0.05);
    let top_bar_height = TOP_BAR_HEIGHT.min((viewport_height - top_margin * 2.0).max(0.0));
    let panel_top = (top_margin + top_bar_height + margin).min(viewport_height);
    let height = desired_height.min((viewport_height - panel_top - margin).max(0.0));
    PanelBounds {
        x: (viewport_width - margin - width).max(0.0),
        y: panel_top,
        width,
        height,
    }
}

pub fn hud_layout(
    viewport: PhysicalSize<u32>,
    line_count: usize,
    playback_active: bool,
    show_observer: bool,
) -> HudLayout {
    let width = viewport.width as f32;
    let height = viewport.height as f32;
    if width <= 0.0 || height <= 0.0 {
        return HudLayout::default();
    }
    let horizontal_margin = TOP_BAR_MARGIN.min(width * 0.05);
    let vertical_margin = TOP_BAR_MARGIN.min(height * 0.05);
    let top_bar = PanelBounds {
        x: horizontal_margin,
        y: vertical_margin,
        width: (width - horizontal_margin * 2.0).max(0.0),
        height: TOP_BAR_HEIGHT.min((height - vertical_margin * 2.0).max(0.0)),
    };
    let mut buttons = [PanelBounds::default(); 4];
    if playback_active {
        let total_width = CONTROL_WIDTHS.iter().sum::<f32>() + CONTROL_GAP * 3.0;
        let scale = (top_bar.width / (total_width + CONTROL_RIGHT_MARGIN * 2.0)).min(1.0);
        let gap = CONTROL_GAP * scale;
        let control_height = CONTROL_HEIGHT.min(top_bar.height - 8.0).max(0.0);
        let scaled_total = CONTROL_WIDTHS.iter().sum::<f32>() * scale + gap * 3.0;
        let mut x =
            (top_bar.right() - CONTROL_RIGHT_MARGIN.min(top_bar.width * 0.04) - scaled_total)
                .max(top_bar.x);
        let y = top_bar.y + ((top_bar.height - control_height) * 0.5).max(0.0);
        for (index, target) in buttons.iter_mut().enumerate() {
            let button_width = CONTROL_WIDTHS[index] * scale;
            *target = PanelBounds {
                x,
                y,
                width: button_width,
                height: control_height,
            };
            x += button_width + gap;
        }
    }
    let text_right = if playback_active {
        (buttons[0].x - 12.0).max(0.0)
    } else {
        (top_bar.right() - 12.0).max(top_bar.x)
    };
    let text_x = (top_bar.x + 12.0).min(text_right);
    let text_width = (text_right - text_x).max(0.0);
    let status_width = text_width.min(216.0);
    let remaining = (text_width - status_width - 16.0).max(0.0);
    let unit_width = if remaining >= 72.0 {
        remaining.min(120.0)
    } else {
        0.0
    };
    HudLayout {
        top_bar,
        observer_panel: if show_observer {
            panel_bounds(viewport, line_count)
        } else {
            PanelBounds::default()
        },
        status_text: PanelBounds {
            x: text_x,
            y: top_bar.y + 10.0,
            width: status_width,
            height: 24.0_f32.min(top_bar.height),
        },
        unit_text: PanelBounds {
            x: (text_x + status_width + 16.0).min(text_right),
            y: top_bar.y + 12.0,
            width: unit_width,
            height: 18.0_f32.min(top_bar.height),
        },
        playback_buttons: buttons,
        playback_active,
    }
}

pub fn hud_hit_test(point: PhysicalPosition<f64>, layout: HudLayout) -> HudHit {
    if layout.playback_active {
        for action in PLAYBACK_ACTIONS {
            if contains(layout.button(action), point) {
                return HudHit::Playback(action);
            }
        }
    }
    if contains(layout.top_bar, point) || contains(layout.observer_panel, point) {
        HudHit::Panel
    } else {
        HudHit::Outside
    }
}

const PLAYBACK_ACTIONS: [PlaybackAction; 4] = [
    PlaybackAction::TogglePause,
    PlaybackAction::SpeedDown,
    PlaybackAction::CycleSpeed,
    PlaybackAction::SpeedUp,
];

const fn action_index(action: PlaybackAction) -> usize {
    match action {
        PlaybackAction::TogglePause => 0,
        PlaybackAction::SpeedDown => 1,
        PlaybackAction::CycleSpeed => 2,
        PlaybackAction::SpeedUp => 3,
    }
}

fn contains(bounds: PanelBounds, point: PhysicalPosition<f64>) -> bool {
    bounds.width > 0.0
        && bounds.height > 0.0
        && point.x >= f64::from(bounds.x)
        && point.x <= f64::from(bounds.right())
        && point.y >= f64::from(bounds.y)
        && point.y <= f64::from(bounds.bottom())
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
struct HudVertex {
    screen: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    padding: [f32; 2],
}

pub struct ObserverHudRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    viewport_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: usize,
    vertex_count: u32,
    vertices: Vec<HudVertex>,
}

impl ObserverHudRenderer {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("observer_hud.wgsl"));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("observer HUD bindings"),
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
        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("observer HUD viewport"),
            size: std::mem::size_of::<ViewportUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("observer HUD bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("observer HUD pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("observer HUD pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<HudVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("observer HUD vertices"),
            size: std::mem::size_of::<HudVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group,
            viewport_buffer,
            vertex_buffer,
            vertex_capacity: 1,
            vertex_count: 0,
            vertices: Vec::new(),
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: PhysicalSize<u32>,
        content: ObserverHudUpload<'_>,
    ) {
        self.vertices = build_vertices(
            viewport,
            content.lines,
            content.accent,
            content.playback,
            content.show_observer,
        );
        if self.vertices.len() > self.vertex_capacity {
            self.vertex_capacity = self.vertices.len().next_power_of_two();
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("observer HUD vertices"),
                size: (self.vertex_capacity * std::mem::size_of::<HudVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        let viewport_uniform = ViewportUniform {
            size: [viewport.width as f32, viewport.height as f32],
            padding: [0.0; 2],
        };
        queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::bytes_of(&viewport_uniform),
        );
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
        }
        self.vertex_count = self.vertices.len() as u32;
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.vertex_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }
}

fn build_vertices(
    viewport: PhysicalSize<u32>,
    lines: &[String],
    accent: [f32; 4],
    playback: Option<PlaybackPresentation>,
    show_observer: bool,
) -> Vec<HudVertex> {
    let layout = hud_layout(viewport, lines.len(), playback.is_some(), show_observer);
    let bounds = layout.observer_panel;
    if layout.top_bar.width <= 0.0 || layout.top_bar.height <= 0.0 {
        return Vec::new();
    }

    let accent = accent.map(|channel| channel.clamp(0.0, 1.0));
    let mut vertices = Vec::new();
    push_rect(
        &mut vertices,
        layout.top_bar.x,
        layout.top_bar.y,
        layout.top_bar.width,
        layout.top_bar.height,
        [0.039, 0.039, 0.047, 0.80],
    );
    push_outline(&mut vertices, layout.top_bar, [1.0, 1.0, 1.0, 0.10]);
    if let Some(playback) = playback {
        push_playback_controls(&mut vertices, layout, playback);
    }
    if !show_observer {
        return vertices;
    }
    push_rect(
        &mut vertices,
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
        [0.025, 0.04, 0.065, 0.82],
    );
    let border = 1.0_f32.min(bounds.width).min(bounds.height);
    push_rect(
        &mut vertices,
        bounds.x,
        bounds.y,
        bounds.width,
        border,
        [accent[0], accent[1], accent[2], accent[3] * 0.72],
    );
    push_rect(
        &mut vertices,
        bounds.x,
        bounds.y,
        3.0_f32.min(bounds.width),
        bounds.height,
        accent,
    );

    let padding = PANEL_PADDING
        .min(bounds.width * 0.12)
        .min(bounds.height * 0.12);
    let available_width = (bounds.width - padding * 2.0).max(0.0);
    let available_height = (bounds.height - padding * 2.0).max(0.0);
    let longest_line = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let line_count = lines.len().max(1) as f32;
    let width_scale = available_width / (longest_line * 6.0);
    let height_scale = available_height / (line_count * 9.0);
    let scale = width_scale.min(height_scale).clamp(1.0, 2.0);
    let advance_x = 6.0 * scale;
    let advance_y = 9.0 * scale;
    let max_columns = (available_width / advance_x).floor() as usize;
    let max_lines = (available_height / advance_y).floor() as usize;
    if max_columns == 0 || max_lines == 0 {
        return vertices;
    }

    let text_color = [0.91, 0.95, 0.98, 0.94];
    for (line_index, line) in lines.iter().take(max_lines).enumerate() {
        let y = bounds.y + padding + line_index as f32 * advance_y;
        for (column, character) in line.chars().take(max_columns).enumerate() {
            push_glyph(
                &mut vertices,
                character,
                bounds.x + padding + column as f32 * advance_x,
                y,
                scale,
                text_color,
            );
        }
    }
    vertices
}

fn push_rect(
    vertices: &mut Vec<HudVertex>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: [f32; 4],
) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let top_left = HudVertex {
        screen: [x, y],
        color,
    };
    let top_right = HudVertex {
        screen: [x + width, y],
        color,
    };
    let bottom_right = HudVertex {
        screen: [x + width, y + height],
        color,
    };
    let bottom_left = HudVertex {
        screen: [x, y + height],
        color,
    };
    vertices.extend_from_slice(&[
        top_left,
        top_right,
        bottom_right,
        top_left,
        bottom_right,
        bottom_left,
    ]);
}

fn playback_fill(action: PlaybackAction, state: PlaybackPresentation) -> [f32; 4] {
    let mut color: [f32; 4] = match action {
        PlaybackAction::TogglePause if state.paused => [0.153, 0.682, 0.376, 1.0],
        PlaybackAction::TogglePause => [0.953, 0.612, 0.071, 1.0],
        PlaybackAction::CycleSpeed if state.speed > 1 => [0.149, 0.310, 0.224, 1.0],
        _ => [1.0, 1.0, 1.0, 0.03],
    };
    if state.hovered == Some(action) {
        for channel in &mut color[..3] {
            *channel = *channel * 0.78 + 0.22;
        }
        color[3] = color[3].max(0.18_f32);
    }
    if state.pressed == Some(action) {
        for channel in &mut color[..3] {
            *channel *= 0.72;
        }
    }
    color
}

fn push_playback_controls(
    vertices: &mut Vec<HudVertex>,
    layout: HudLayout,
    state: PlaybackPresentation,
) {
    let status = if state.paused {
        "SIMULATION PAUSED"
    } else {
        "CONNECTION STABLE"
    };
    let status_scale = (layout.status_text.width / (status.chars().count() as f32 * 6.0)).min(2.0);
    if status_scale >= 0.5 {
        push_text(
            vertices,
            status,
            layout.status_text.x,
            layout.status_text.y + 4.0,
            status_scale,
            [0.91, 0.95, 0.98, 0.94],
        );
    }
    if layout.unit_text.width > 0.0 {
        let units = format!("{} UNITS", state.unit_count);
        let unit_scale = (layout.unit_text.width / (units.chars().count() as f32 * 6.0)).min(1.5);
        push_text(
            vertices,
            &units,
            layout.unit_text.x,
            layout.unit_text.y + 3.0,
            unit_scale,
            [0.62, 0.66, 0.70, 0.94],
        );
    }

    for action in PLAYBACK_ACTIONS {
        let bounds = layout.button(action);
        let fill = playback_fill(action, state);
        push_rect(
            vertices,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            fill,
        );
        let border = if action == PlaybackAction::CycleSpeed && state.speed > 1 {
            [0.247, 0.663, 0.416, 1.0]
        } else {
            [1.0, 1.0, 1.0, 0.10]
        };
        push_outline(vertices, bounds, border);
        let cx = bounds.x + bounds.width * 0.5;
        let cy = bounds.y + bounds.height * 0.5;
        match action {
            PlaybackAction::TogglePause if state.paused => push_triangle(
                vertices,
                [cx - 4.0, cy - 6.0],
                [cx - 4.0, cy + 6.0],
                [cx + 6.0, cy],
                [1.0; 4],
            ),
            PlaybackAction::TogglePause => {
                push_rect(vertices, cx - 5.0, cy - 6.0, 3.0, 12.0, [1.0; 4]);
                push_rect(vertices, cx + 2.0, cy - 6.0, 3.0, 12.0, [1.0; 4]);
            }
            PlaybackAction::SpeedDown => {
                push_chevron(vertices, cx, cy, false, [0.88, 0.88, 0.88, 1.0])
            }
            PlaybackAction::SpeedUp => {
                push_chevron(vertices, cx, cy, true, [0.88, 0.88, 0.88, 1.0])
            }
            PlaybackAction::CycleSpeed => {
                let label = format!("{}X", state.speed.clamp(1, 3));
                push_text(
                    vertices,
                    &label,
                    cx - 6.0,
                    cy - 5.0,
                    1.5,
                    [0.91, 0.95, 0.98, 0.94],
                );
            }
        }
    }
}

fn push_outline(vertices: &mut Vec<HudVertex>, bounds: PanelBounds, color: [f32; 4]) {
    push_rect(vertices, bounds.x, bounds.y, bounds.width, 1.0, color);
    push_rect(
        vertices,
        bounds.x,
        bounds.bottom() - 1.0,
        bounds.width,
        1.0,
        color,
    );
    push_rect(vertices, bounds.x, bounds.y, 1.0, bounds.height, color);
    push_rect(
        vertices,
        bounds.right() - 1.0,
        bounds.y,
        1.0,
        bounds.height,
        color,
    );
}

fn push_triangle(
    vertices: &mut Vec<HudVertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    color: [f32; 4],
) {
    vertices.extend([
        HudVertex { screen: a, color },
        HudVertex { screen: b, color },
        HudVertex { screen: c, color },
    ]);
}

fn push_chevron(vertices: &mut Vec<HudVertex>, cx: f32, cy: f32, right: bool, color: [f32; 4]) {
    let direction = if right { 1.0 } else { -1.0 };
    for offset in 0..5 {
        let x = cx + direction * (offset as f32 - 2.0);
        let spread = (offset as f32 - 2.0).abs();
        push_rect(vertices, x, cy - 4.0 + spread, 2.0, 2.0, color);
        push_rect(vertices, x, cy + 2.0 - spread, 2.0, 2.0, color);
    }
}

fn push_text(
    vertices: &mut Vec<HudVertex>,
    text: &str,
    x: f32,
    y: f32,
    scale: f32,
    color: [f32; 4],
) {
    for (column, character) in text.chars().enumerate() {
        push_glyph(
            vertices,
            character,
            x + column as f32 * 6.0 * scale,
            y,
            scale,
            color,
        );
    }
}

fn push_glyph(
    vertices: &mut Vec<HudVertex>,
    character: char,
    x: f32,
    y: f32,
    scale: f32,
    color: [f32; 4],
) {
    for (row, bits) in glyph_rows(character).iter().copied().enumerate() {
        for column in 0..5 {
            if bits & (1 << (4 - column)) != 0 {
                push_rect(
                    vertices,
                    x + column as f32 * scale,
                    y + row as f32 * scale,
                    scale,
                    scale,
                    color,
                );
            }
        }
    }
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '=' => [0, 31, 0, 31, 0, 0, 0],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 0, 12, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        ';' => [0, 12, 12, 0, 12, 8, 0],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '\\' => [16, 8, 8, 4, 2, 2, 1],
        '%' => [17, 2, 4, 4, 8, 16, 17],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '#' => [10, 10, 31, 10, 31, 10, 10],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '\'' => [4, 4, 2, 0, 0, 0, 0],
        '"' => [10, 10, 10, 0, 0, 0, 0],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_compact_and_top_right() {
        let bounds = panel_bounds(PhysicalSize::new(1920, 1080), 33);
        assert_eq!(
            bounds,
            PanelBounds {
                x: 1468.0,
                y: 72.0,
                width: 440.0,
                height: 614.0
            }
        );
        assert!(contains(bounds, PhysicalPosition::new(1900.0, 80.0)));
        assert!(!contains(bounds, PhysicalPosition::new(1200.0, 20.0)));
    }

    #[test]
    fn tiny_and_zero_viewports_are_clamped() {
        let tiny = panel_bounds(PhysicalSize::new(20, 10), 33);
        assert!(tiny.x >= 0.0 && tiny.y >= 0.0);
        assert!(tiny.right() <= 20.0 && tiny.bottom() <= 10.0);
        assert_eq!(
            panel_bounds(PhysicalSize::new(0, 10), 33),
            PanelBounds::default()
        );
        assert!(!contains(
            panel_bounds(PhysicalSize::new(0, 0), 33),
            PhysicalPosition::new(0.0, 0.0)
        ));
    }

    #[test]
    fn glyph_generation_is_stable_and_case_insensitive() {
        assert_eq!(glyph_rows('a'), glyph_rows('A'));
        assert_eq!(glyph_rows('A'), [14, 17, 17, 31, 17, 17, 17]);
        let mut first = Vec::new();
        let mut second = Vec::new();
        push_glyph(&mut first, 'A', 3.0, 4.0, 2.0, [1.0; 4]);
        push_glyph(&mut second, 'a', 3.0, 4.0, 2.0, [1.0; 4]);
        assert_eq!(first, second);
        assert_eq!(first.len(), 18 * 6);
    }

    #[test]
    fn empty_text_keeps_only_panel_geometry() {
        let vertices = build_vertices(
            PhysicalSize::new(800, 600),
            &[],
            [0.2, 0.7, 1.0, 1.0],
            None,
            true,
        );
        assert_eq!(vertices.len(), 48);
        assert!(build_vertices(PhysicalSize::new(0, 0), &[], [1.0; 4], None, true).is_empty());
    }

    #[test]
    fn full_observer_readout_fits_common_viewports() {
        let lines = vec!["I".to_owned(); 33];
        let expected_vertices = 48 + 33 * 11 * 6;
        assert_eq!(
            build_vertices(PhysicalSize::new(1920, 1080), &lines, [1.0; 4], None, true).len(),
            expected_vertices
        );
        assert_eq!(
            build_vertices(PhysicalSize::new(800, 600), &lines, [1.0; 4], None, true).len(),
            expected_vertices
        );
    }

    fn playback(paused: bool, speed: u8) -> PlaybackPresentation {
        PlaybackPresentation {
            paused,
            speed,
            unit_count: 42,
            hovered: None,
            pressed: None,
        }
    }

    #[test]
    fn playback_layout_matches_browser_order_and_gaps() {
        let layout = hud_layout(PhysicalSize::new(1280, 720), 20, true, true);
        assert_eq!(layout.top_bar.x, 16.0);
        assert_eq!(layout.top_bar.y, 16.0);
        assert_eq!(layout.top_bar.height, 44.0);
        assert!(layout.observer_panel.y >= layout.top_bar.bottom());
        for index in 0..3 {
            assert_eq!(
                layout.playback_buttons[index + 1].x - layout.playback_buttons[index].right(),
                4.0
            );
        }
        assert_eq!(
            layout.playback_buttons.map(|bounds| bounds.width),
            [32.0, 30.0, 38.0, 30.0]
        );
        let compact = hud_layout(PhysicalSize::new(800, 600), 20, true, true);
        assert!(compact.observer_panel.y >= compact.top_bar.bottom());
        assert!(compact.observer_panel.bottom() <= 588.0);
        assert!(compact.status_text.right() <= compact.button(PlaybackAction::TogglePause).x);
        assert!(
            compact.unit_text.width == 0.0
                || compact.unit_text.right() <= compact.button(PlaybackAction::TogglePause).x
        );
        let tiny = hud_layout(PhysicalSize::new(120, 80), 2, true, true);
        assert!(
            tiny.playback_buttons
                .iter()
                .all(|bounds| bounds.x >= 0.0 && bounds.right() <= 120.0)
        );
        assert!(tiny.status_text.right() <= tiny.button(PlaybackAction::TogglePause).x);
        assert!(tiny.observer_panel.y >= tiny.top_bar.bottom());
        assert!(tiny.observer_panel.bottom() <= 80.0);
    }

    #[test]
    fn hit_test_distinguishes_buttons_gaps_panel_and_map() {
        let layout = hud_layout(PhysicalSize::new(1280, 720), 20, true, false);
        for action in PLAYBACK_ACTIONS {
            let bounds = layout.button(action);
            assert_eq!(
                hud_hit_test(
                    PhysicalPosition::new(
                        f64::from(bounds.x + bounds.width / 2.0),
                        f64::from(bounds.y + bounds.height / 2.0)
                    ),
                    layout
                ),
                HudHit::Playback(action)
            );
        }
        let first = layout.button(PlaybackAction::TogglePause);
        assert_eq!(
            hud_hit_test(
                PhysicalPosition::new(f64::from(first.right() + 2.0), f64::from(first.y + 2.0)),
                layout
            ),
            HudHit::Panel
        );
        assert_eq!(
            hud_hit_test(PhysicalPosition::new(5.0, 200.0), layout),
            HudHit::Outside
        );
        assert_eq!(layout.observer_panel, PanelBounds::default());
    }

    #[test]
    fn playback_colors_match_browser_states() {
        assert_eq!(
            playback_fill(PlaybackAction::TogglePause, playback(false, 1)),
            [0.953, 0.612, 0.071, 1.0]
        );
        assert_eq!(
            playback_fill(PlaybackAction::TogglePause, playback(true, 1)),
            [0.153, 0.682, 0.376, 1.0]
        );
        assert_eq!(
            playback_fill(PlaybackAction::CycleSpeed, playback(false, 2)),
            [0.149, 0.310, 0.224, 1.0]
        );
    }

    #[test]
    fn top_bar_survives_hidden_observer_and_controls_are_runtime_only() {
        let viewport = PhysicalSize::new(800, 600);
        let hidden = build_vertices(
            viewport,
            &["HIDDEN".to_owned()],
            [1.0; 4],
            Some(playback(false, 1)),
            false,
        );
        assert!(!hidden.is_empty());
        let inactive = hud_layout(viewport, 1, false, false);
        assert_eq!(inactive.playback_buttons, [PanelBounds::default(); 4]);
        assert_eq!(
            hud_hit_test(PhysicalPosition::new(780.0, 20.0), inactive),
            HudHit::Panel
        );
    }
}
