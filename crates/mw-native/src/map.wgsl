struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
};

@group(0) @binding(0) var ownership: texture_2d<u32>;
@group(0) @binding(1) var<uniform> view: View;
@group(0) @binding(2) var<storage, read> palette: array<vec4<f32>>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return output;
}

fn owner_at(cell: vec2<i32>) -> u32 {
    let maximum = vec2<i32>(view.grid_size) - vec2<i32>(1);
    return textureLoad(ownership, clamp(cell, vec2<i32>(0), maximum), 0).r;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let world = view.center + (position.xy - view.viewport * 0.5) / view.pixels_per_world;
    if (world.x < 0.0 || world.x >= 2.0 || world.y < 0.0 || world.y >= 1.0) {
        return vec4<f32>(0.006, 0.009, 0.016, 1.0);
    }

    // Scenario rows run south-to-north, while framebuffer Y runs top-to-bottom.
    // World X spans two units so 360x180 degrees renders at a true 2:1 aspect.
    let grid_uv = vec2<f32>(world.x * 0.5, 1.0 - world.y);
    let cell = min(vec2<i32>(grid_uv * vec2<f32>(view.grid_size)), vec2<i32>(view.grid_size) - vec2<i32>(1));
    let owner = owner_at(cell);
    var color = palette[min(owner, view.palette_len - 1u)];

    if (owner != 0u) {
        let border = owner_at(cell + vec2<i32>(1, 0)) != owner ||
            owner_at(cell + vec2<i32>(-1, 0)) != owner ||
            owner_at(cell + vec2<i32>(0, 1)) != owner ||
            owner_at(cell + vec2<i32>(0, -1)) != owner;
        if (border) {
            color = vec4<f32>(mix(color.rgb, vec3<f32>(0.015), 0.78), 1.0);
        }
    }
    return vec4<f32>(color.rgb, 1.0);
}
