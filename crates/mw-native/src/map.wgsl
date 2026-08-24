struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
    frontlines_active: u32,
};

@group(0) @binding(0) var ownership: texture_2d<u32>;
@group(0) @binding(1) var<uniform> view: View;
@group(0) @binding(2) var<storage, read> palette: array<vec4<f32>>;
@group(0) @binding(3) var dominant_sides: texture_2d<i32>;

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

fn side_at(cell: vec2<i32>) -> i32 {
    let maximum = vec2<i32>(view.grid_size) - vec2<i32>(1);
    return textureLoad(dominant_sides, clamp(cell, vec2<i32>(0), maximum), 0).r;
}

fn hostile_side_edge(side: i32, other: i32) -> bool {
    return side >= 0 && other >= 0 && side != other;
}

fn segment_distance(point: vec2<f32>, start: vec2<f32>, end: vec2<f32>) -> f32 {
    let delta = end - start;
    let length_squared = max(dot(delta, delta), 0.000001);
    let t = clamp(dot(point - start, delta) / length_squared, 0.0, 1.0);
    return length(point - (start + delta * t));
}

fn frontline_distance(grid_position: vec2<f32>, cell: vec2<i32>) -> f32 {
    let maximum_quad = max(vec2<i32>(view.grid_size) - vec2<i32>(2), vec2<i32>(0));
    let quad = clamp(cell, vec2<i32>(0), maximum_quad);
    let local = grid_position - vec2<f32>(quad);
    let top_left = side_at(quad);
    let top_right = side_at(quad + vec2<i32>(1, 0));
    let bottom_right = side_at(quad + vec2<i32>(1, 1));
    let bottom_left = side_at(quad + vec2<i32>(0, 1));
    var crossings: array<vec2<f32>, 4>;
    var count = 0u;
    if (hostile_side_edge(top_left, top_right)) {
        crossings[count] = vec2<f32>(0.5, 0.0);
        count += 1u;
    }
    if (hostile_side_edge(top_right, bottom_right)) {
        crossings[count] = vec2<f32>(1.0, 0.5);
        count += 1u;
    }
    if (hostile_side_edge(bottom_left, bottom_right)) {
        crossings[count] = vec2<f32>(0.5, 1.0);
        count += 1u;
    }
    if (hostile_side_edge(top_left, bottom_left)) {
        crossings[count] = vec2<f32>(0.0, 0.5);
        count += 1u;
    }

    var distance = 1000000.0;
    if (count >= 2u) {
        distance = segment_distance(local, crossings[0], crossings[1]);
        if (count >= 3u) {
            distance = min(distance, segment_distance(local, crossings[1], crossings[2]));
        }
    } else if (count == 1u) {
        distance = segment_distance(local, crossings[0], vec2<f32>(0.5));
    }
    return distance;
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
    let grid_position = grid_uv * vec2<f32>(view.grid_size);
    let cell = min(vec2<i32>(grid_position), vec2<i32>(view.grid_size) - vec2<i32>(1));
    let local = fract(grid_position);
    let owner = owner_at(cell);
    var color = palette[min(owner, view.palette_len - 1u)];

    let cell_pixels = max(
        min(
            view.pixels_per_world * 2.0 / f32(view.grid_size.x),
            view.pixels_per_world / f32(view.grid_size.y),
        ),
        0.001,
    );
    var frontline_alpha = 0.0;
    if (view.frontlines_active != 0u) {
        let browser_zoom = max(log2(max(view.pixels_per_world / 128.0, 1.0)), 0.0);
        let frontline_width = max(1.2, 3.5 * (browser_zoom / 5.0));
        let frontline_pixels = frontline_distance(grid_position, cell) * cell_pixels;
        frontline_alpha = 1.0 - smoothstep(
            max(0.0, frontline_width * 0.5 - 0.75),
            frontline_width * 0.5 + 0.75,
            frontline_pixels,
        );
    }

    // Browser pass 2 draws the black controller frontline before its thin
    // political-border pass.
    color = vec4<f32>(mix(color.rgb, vec3<f32>(0.0), frontline_alpha), 1.0);

    if (owner != 0u) {
        let border_half = clamp(0.5 / cell_pixels, 0.0, 0.5);
        let border =
            (local.x < border_half && owner_at(cell + vec2<i32>(-1, 0)) != owner) ||
            (1.0 - local.x < border_half && owner_at(cell + vec2<i32>(1, 0)) != owner) ||
            (local.y < border_half && owner_at(cell + vec2<i32>(0, -1)) != owner) ||
            (1.0 - local.y < border_half && owner_at(cell + vec2<i32>(0, 1)) != owner);
        if (border) {
            color = vec4<f32>(mix(color.rgb, vec3<f32>(0.0), 0.3), 1.0);
        }
    }
    return vec4<f32>(color.rgb, 1.0);
}
