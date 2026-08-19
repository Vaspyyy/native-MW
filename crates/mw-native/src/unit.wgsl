struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) health: f32,
    @location(2) kind: u32,
    @location(3) flags: u32,
    @location(4) local: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, @location(1) world: vec2<f32>, @location(2) color: vec4<f32>, @location(3) size: f32, @location(4) health: f32, @location(5) kind: u32, @location(6) flags: u32) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0));
    let local = corners[vertex_index];
    let screen = (world - view.center) * view.pixels_per_world + view.viewport * 0.5 + local * size;
    var output: VertexOutput;
    output.position = vec4((screen / view.viewport) * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    output.color = color;
    output.health = health;
    output.kind = kind;
    output.flags = flags;
    output.local = local;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let diamond = abs(input.local.x) + abs(input.local.y);
    if (diamond > 1.0) { discard; }
    var color = input.color;
    if (input.kind == 1u) { color = vec4(mix(color.rgb, vec3(1.0), 0.35), color.a); }
    if ((input.flags & 3u) != 0u) { color = vec4(mix(color.rgb, vec3(0.35, 0.75, 1.0), 0.45), color.a); }
    if (input.health < 0.35) { color = vec4(mix(color.rgb, vec3(0.45, 0.05, 0.03), 0.65), color.a); }
    return vec4(color.rgb, color.a * (0.35 + 0.65 * input.health));
}
