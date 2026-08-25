//! Browser-matched observer HUD geometry and screen-space typography layout.

use bytemuck::{Pod, Zeroable};
use mw_core::GameDate;
use winit::dpi::{PhysicalPosition, PhysicalSize};

use crate::map_label::{ScreenTextAlign, ScreenTextFace, ScreenTextRun};

const PANEL_WIDTH: f32 = 320.0;
const PANEL_MIN_HEIGHT: f32 = 80.0;
const PANEL_MAX_HEIGHT: f32 = 640.0;
const PANEL_MARGIN: f32 = 12.0;
const PANEL_PADDING: f32 = 14.0;
const PANEL_LINE_HEIGHT: f32 = 13.0;
const TOP_BAR_HEIGHT: f32 = 60.0;
const TOP_BAR_MARGIN: f32 = 16.0;
const CONTROL_HEIGHT: f32 = 30.0;
const CONTROL_GAP: f32 = 4.0;
const CONTROL_RIGHT_MARGIN: f32 = 12.0;
const CONTROL_WIDTHS: [f32; 4] = [32.0, 30.0, 38.0, 30.0];
const DATE_TOP_GAP: f32 = 14.0;
const SECTION_HEADERS: [&str; 5] = ["TERRITORY", "ECONOMY", "FORCES", "AIR POWER", "OPERATIONS"];

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
    let desired_height = (PANEL_PADDING * 2.0 + line_count.max(1) as f32 * PANEL_LINE_HEIGHT)
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
        let control_height = CONTROL_HEIGHT.min(top_bar.height - 12.0).max(0.0);
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
    let text_x = (top_bar.x + 16.0).min(text_right);
    let text_width = (text_right - text_x).max(0.0);
    let status_width = text_width.min(280.0);
    HudLayout {
        top_bar,
        observer_panel: if show_observer {
            panel_bounds(viewport, line_count)
        } else {
            PanelBounds::default()
        },
        status_text: PanelBounds {
            x: text_x,
            y: top_bar.y + 8.0,
            width: status_width,
            height: 25.0_f32.min(top_bar.height),
        },
        unit_text: PanelBounds {
            x: text_x,
            y: top_bar.y + 34.0,
            width: text_width.min(180.0),
            height: 18.0_f32.min(top_bar.height),
        },
        playback_buttons: buttons,
        playback_active,
    }
}

pub fn hud_text_runs(
    viewport: PhysicalSize<u32>,
    lines: &[String],
    playback: Option<PlaybackPresentation>,
    game_date: Option<GameDate>,
    show_observer: bool,
) -> Vec<ScreenTextRun> {
    let layout = hud_layout(viewport, lines.len(), playback.is_some(), show_observer);
    if layout.top_bar.width <= 0.0 || layout.top_bar.height <= 0.0 {
        return Vec::new();
    }
    let mut runs = Vec::new();
    if let Some(playback) = playback {
        runs.push(ScreenTextRun {
            text: if playback.paused {
                "Simulation Paused".to_owned()
            } else {
                "Global Conflict Active".to_owned()
            },
            screen: [layout.status_text.x, layout.status_text.y + 17.0],
            font_size: 17.0,
            color: [0.95, 0.95, 0.93, 1.0],
            face: ScreenTextFace::Serif,
            align: ScreenTextAlign::LeftBaseline,
            halo_radius: 0.0,
            halo_alpha: 0.0,
        });
        if layout.unit_text.width > 0.0 {
            runs.push(ScreenTextRun {
                text: format!("{} UNITS", playback.unit_count),
                screen: [layout.unit_text.x, layout.unit_text.y + 9.0],
                font_size: 10.0,
                color: [0.68, 0.70, 0.70, 1.0],
                face: ScreenTextFace::Sans,
                align: ScreenTextAlign::LeftBaseline,
                halo_radius: 0.0,
                halo_alpha: 0.0,
            });
        }
        let speed = layout.button(PlaybackAction::CycleSpeed);
        runs.push(ScreenTextRun {
            text: format!("{}X", playback.speed.clamp(1, 3)),
            screen: [speed.x + speed.width * 0.5, speed.y + speed.height * 0.5],
            font_size: 11.0,
            color: [0.96, 0.96, 0.94, 1.0],
            face: ScreenTextFace::Mono,
            align: ScreenTextAlign::Center,
            halo_radius: 0.0,
            halo_alpha: 0.0,
        });
    }
    if let Some(game_date) = game_date {
        runs.push(ScreenTextRun {
            text: game_date.to_string(),
            screen: [
                viewport.width as f32 * 0.5,
                layout.top_bar.bottom() + DATE_TOP_GAP + 9.0,
            ],
            font_size: 17.0,
            color: [1.0, 1.0, 1.0, 0.98],
            face: ScreenTextFace::Serif,
            align: ScreenTextAlign::Center,
            halo_radius: 2.0,
            halo_alpha: 0.92,
        });
    }
    if show_observer {
        let panel = layout.observer_panel;
        let mut baseline = panel.y + PANEL_PADDING + 12.0;
        let bottom = panel.bottom() - PANEL_PADDING;
        for (index, line) in lines.iter().enumerate() {
            if baseline > bottom {
                break;
            }
            if line.is_empty() {
                baseline += 5.0;
                continue;
            }
            let section = SECTION_HEADERS.contains(&line.as_str());
            let title = index == 0;
            let selected_country = index >= 3
                && lines
                    .get(index.wrapping_sub(1))
                    .is_some_and(String::is_empty)
                && !section
                && !line.starts_with("LEFT CLICK");
            runs.push(ScreenTextRun {
                text: line.clone(),
                screen: [panel.x + PANEL_PADDING, baseline],
                font_size: if title {
                    16.0
                } else if section {
                    9.5
                } else if selected_country {
                    13.0
                } else {
                    10.5
                },
                color: if section {
                    [0.95, 0.57, 0.22, 1.0]
                } else if index == 1 {
                    [0.69, 0.71, 0.71, 1.0]
                } else {
                    [0.92, 0.92, 0.89, 1.0]
                },
                face: if title || selected_country {
                    ScreenTextFace::Serif
                } else if section {
                    ScreenTextFace::Mono
                } else {
                    ScreenTextFace::Sans
                },
                align: ScreenTextAlign::LeftBaseline,
                halo_radius: 0.0,
                halo_alpha: 0.0,
            });
            baseline += if title { 17.0 } else { PANEL_LINE_HEIGHT };
        }
        runs.push(ScreenTextRun {
            text: "Tiles © Esri".to_owned(),
            screen: [viewport.width as f32 - 8.0, viewport.height as f32 - 8.0],
            font_size: 9.0,
            color: [0.88, 0.88, 0.86, 0.82],
            face: ScreenTextFace::Sans,
            align: ScreenTextAlign::RightBaseline,
            halo_radius: 1.5,
            halo_alpha: 0.8,
        });
    }
    runs
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
        queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [viewport.width as f32, viewport.height as f32],
                padding: [0.0; 2],
            }),
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
    if layout.top_bar.width <= 0.0 || layout.top_bar.height <= 0.0 {
        return Vec::new();
    }
    let accent = accent.map(|channel| channel.clamp(0.0, 1.0));
    let mut vertices = Vec::new();
    push_rounded_rect(
        &mut vertices,
        PanelBounds {
            y: layout.top_bar.y + 4.0,
            ..layout.top_bar
        },
        7.0,
        [0.0, 0.0, 0.0, 0.34],
    );
    push_rounded_rect(
        &mut vertices,
        layout.top_bar,
        7.0,
        [0.035, 0.039, 0.043, 0.92],
    );
    push_outline(&mut vertices, layout.top_bar, [1.0, 1.0, 1.0, 0.11]);
    if let Some(playback) = playback {
        push_playback_controls(&mut vertices, layout, playback);
    }
    if !show_observer {
        return vertices;
    }
    let bounds = layout.observer_panel;
    push_rounded_rect(
        &mut vertices,
        PanelBounds {
            y: bounds.y + 4.0,
            ..bounds
        },
        6.0,
        [0.0, 0.0, 0.0, 0.38],
    );
    push_rounded_rect(&mut vertices, bounds, 6.0, [0.028, 0.032, 0.036, 0.94]);
    push_outline(&mut vertices, bounds, [1.0, 1.0, 1.0, 0.11]);
    push_rect(
        &mut vertices,
        bounds.x,
        bounds.y,
        bounds.width,
        2.0_f32.min(bounds.height),
        [accent[0], accent[1], accent[2], 0.9],
    );
    vertices
}

fn push_playback_controls(
    vertices: &mut Vec<HudVertex>,
    layout: HudLayout,
    state: PlaybackPresentation,
) {
    for action in PLAYBACK_ACTIONS {
        let bounds = layout.button(action);
        push_rounded_rect(vertices, bounds, 3.0, playback_fill(action, state));
        let border = if action == PlaybackAction::CycleSpeed && state.speed > 1 {
            [0.247, 0.663, 0.416, 1.0]
        } else {
            [1.0, 1.0, 1.0, 0.14]
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
            PlaybackAction::CycleSpeed => {}
        }
    }
}

fn playback_fill(action: PlaybackAction, state: PlaybackPresentation) -> [f32; 4] {
    let mut color: [f32; 4] = match action {
        PlaybackAction::TogglePause if state.paused => [0.153, 0.682, 0.376, 1.0],
        PlaybackAction::TogglePause => [0.953, 0.612, 0.071, 1.0],
        PlaybackAction::CycleSpeed if state.speed > 1 => [0.149, 0.310, 0.224, 1.0],
        _ => [1.0, 1.0, 1.0, 0.045],
    };
    if state.hovered == Some(action) {
        for channel in &mut color[..3] {
            *channel = *channel * 0.78 + 0.22;
        }
        color[3] = color[3].max(0.18);
    }
    if state.pressed == Some(action) {
        for channel in &mut color[..3] {
            *channel *= 0.72;
        }
    }
    color
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

fn push_rounded_rect(
    vertices: &mut Vec<HudVertex>,
    bounds: PanelBounds,
    radius: f32,
    color: [f32; 4],
) {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let radius = radius.min(bounds.width * 0.5).min(bounds.height * 0.5);
    if radius <= 0.0 {
        push_rect(
            vertices,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            color,
        );
        return;
    }
    push_rect(
        vertices,
        bounds.x + radius,
        bounds.y,
        bounds.width - radius * 2.0,
        bounds.height,
        color,
    );
    push_rect(
        vertices,
        bounds.x,
        bounds.y + radius,
        radius,
        bounds.height - radius * 2.0,
        color,
    );
    push_rect(
        vertices,
        bounds.right() - radius,
        bounds.y + radius,
        radius,
        bounds.height - radius * 2.0,
        color,
    );
    const SEGMENTS: usize = 5;
    let corners = [
        ([bounds.x + radius, bounds.y + radius], std::f32::consts::PI),
        (
            [bounds.right() - radius, bounds.y + radius],
            -std::f32::consts::FRAC_PI_2,
        ),
        ([bounds.right() - radius, bounds.bottom() - radius], 0.0),
        (
            [bounds.x + radius, bounds.bottom() - radius],
            std::f32::consts::FRAC_PI_2,
        ),
    ];
    for (center, start) in corners {
        for segment in 0..SEGMENTS {
            let a = start + segment as f32 * std::f32::consts::FRAC_PI_2 / SEGMENTS as f32;
            let b = start + (segment + 1) as f32 * std::f32::consts::FRAC_PI_2 / SEGMENTS as f32;
            push_triangle(
                vertices,
                center,
                [center[0] + a.cos() * radius, center[1] + a.sin() * radius],
                [center[0] + b.cos() * radius, center[1] + b.sin() * radius],
                color,
            );
        }
    }
}

fn push_outline(vertices: &mut Vec<HudVertex>, bounds: PanelBounds, color: [f32; 4]) {
    push_rect(
        vertices,
        bounds.x + 5.0,
        bounds.y,
        bounds.width - 10.0,
        1.0,
        color,
    );
    push_rect(
        vertices,
        bounds.x + 5.0,
        bounds.bottom() - 1.0,
        bounds.width - 10.0,
        1.0,
        color,
    );
    push_rect(
        vertices,
        bounds.x,
        bounds.y + 5.0,
        1.0,
        bounds.height - 10.0,
        color,
    );
    push_rect(
        vertices,
        bounds.right() - 1.0,
        bounds.y + 5.0,
        1.0,
        bounds.height - 10.0,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn layout_matches_browser_scale_and_top_right_panel() {
        let layout = hud_layout(PhysicalSize::new(1280, 720), 33, true, true);
        assert_eq!(
            layout.top_bar,
            PanelBounds {
                x: 16.0,
                y: 16.0,
                width: 1248.0,
                height: 60.0
            }
        );
        assert_eq!(layout.observer_panel.x, 948.0);
        assert_eq!(layout.observer_panel.width, 320.0);
        assert_eq!(layout.observer_panel.y, 88.0);
        assert!(layout.observer_panel.bottom() <= 708.0);
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
    }

    #[test]
    fn playback_layout_preserves_browser_order_and_hit_targets() {
        let layout = hud_layout(PhysicalSize::new(1280, 720), 20, true, false);
        for index in 0..3 {
            assert_eq!(
                layout.playback_buttons[index + 1].x - layout.playback_buttons[index].right(),
                4.0
            );
        }
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
        assert_eq!(
            hud_hit_test(PhysicalPosition::new(5.0, 200.0), layout),
            HudHit::Outside
        );
    }

    #[test]
    fn geometry_contains_surfaces_and_controls_but_no_text_pixels() {
        let lines = vec!["MODERN WARS // TICK 1".to_owned(), "TERRITORY".to_owned()];
        let vertices = build_vertices(
            PhysicalSize::new(1280, 720),
            &lines,
            [0.2, 0.7, 1.0, 1.0],
            Some(playback(false, 1)),
            true,
        );
        assert!(!vertices.is_empty());
        assert!(vertices.len() < 1_000);
    }

    #[test]
    fn text_runs_use_browser_typography_hierarchy() {
        let lines = vec![
            "MODERN WARS // TICK 7".to_owned(),
            "RUNNING  42 UNITS".to_owned(),
            String::new(),
            "GERMANY  #64".to_owned(),
            String::new(),
            "TERRITORY".to_owned(),
        ];
        let date = GameDate::new(2024, 2, 29).unwrap();
        let runs = hud_text_runs(
            PhysicalSize::new(1280, 720),
            &lines,
            Some(playback(false, 2)),
            Some(date),
            true,
        );
        assert!(
            runs.iter().any(
                |run| run.text == "Global Conflict Active" && run.face == ScreenTextFace::Serif
            )
        );
        assert!(runs.iter().any(|run| run.text == "2X"
            && run.face == ScreenTextFace::Mono
            && run.align == ScreenTextAlign::Center));
        assert!(
            runs.iter()
                .any(|run| run.text == "TERRITORY" && run.face == ScreenTextFace::Mono)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == date.to_string() && run.halo_radius > 0.0)
        );
        assert!(runs.iter().any(|run| run.text == "Tiles © Esri"));
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
}
