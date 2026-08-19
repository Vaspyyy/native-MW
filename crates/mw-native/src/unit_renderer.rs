use bytemuck::{Pod, Zeroable};
use mw_core::{FrameSnapshot, UnitKind};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct UnitInstance {
    pub world: [f32; 2],
    pub color: [f32; 4],
    pub size: f32,
    pub health_fraction: f32,
    pub kind: u32,
    pub flags: u32,
}

pub struct UnitRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    instances: Vec<UnitInstance>,
}

impl UnitRenderer {
    pub fn new(
        device: &wgpu::Device,
        view_buffer: &wgpu::Buffer,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("unit.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unit bindings"),
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
            label: Some("unit bind group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unit pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UnitInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32x4, 3 => Float32, 4 => Float32, 5 => Uint32, 6 => Uint32],
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
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unit instances"),
            size: std::mem::size_of::<UnitInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind_group,
            instance_buffer,
            instance_capacity: 1,
            instance_count: 0,
            instances: Vec::new(),
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &FrameSnapshot,
        palette: &[[f32; 4]],
    ) {
        self.instances.clear();
        self.instances.extend(snapshot.units.iter().map(|unit| {
            let color = palette
                .get(unit.sovereign as usize)
                .copied()
                .unwrap_or([0.9, 0.9, 0.9, 1.0]);
            let kind = matches!(unit.kind, UnitKind::Armor) as u32;
            let flags = (unit.transport as u32) | ((unit.at_sea as u32) << 1);
            UnitInstance {
                world: geographic_to_world(unit.lat, unit.lng),
                color,
                size: if kind == 1 { 9.0 } else { 7.0 },
                health_fraction: unit.health_fraction.clamp(0.0, 1.0),
                kind,
                flags,
            }
        }));
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unit instances"),
                size: (self.instance_capacity * std::mem::size_of::<UnitInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
        self.instance_count = self.instances.len() as u32;
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.instance_count);
    }

    pub fn instance_count(&self) -> u32 {
        self.instance_count
    }
}

pub fn geographic_to_world(lat: f64, lng: f64) -> [f32; 2] {
    [
        ((lng + 180.0) / 180.0) as f32,
        ((90.0 - lat) / 180.0) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geographic_mapping_matches_map_projection() {
        assert_eq!(geographic_to_world(90.0, -180.0), [0.0, 0.0]);
        assert_eq!(geographic_to_world(0.0, 0.0), [1.0, 0.5]);
        assert_eq!(geographic_to_world(-90.0, 180.0), [2.0, 1.0]);
    }
}
