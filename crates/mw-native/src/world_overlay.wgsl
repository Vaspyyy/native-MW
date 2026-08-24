struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct MarkerOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) value: f32,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) @interpolate(flat) flags: u32,
    @location(4) local: vec2<f32>,
};

@vertex
fn vs_marker(
    @builtin(vertex_index) vertex_index: u32,
    @location(1) world: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) size: f32,
    @location(4) value: f32,
    @location(5) kind: u32,
    @location(6) flags: u32,
) -> MarkerOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0),
        vec2(1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(-1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(-1.0, 1.0),
    );
    let local = corners[vertex_index];
    let screen = (world - view.center) * view.pixels_per_world + view.viewport * 0.5 + local * size;
    var output: MarkerOutput;
    output.position = vec4((screen / view.viewport) * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    output.color = color;
    output.value = value;
    output.kind = kind;
    output.flags = flags;
    output.local = local;
    return output;
}

@fragment
fn fs_marker(input: MarkerOutput) -> @location(0) vec4<f32> {
    let x = input.local.x;
    let y = input.local.y;
    let radius = length(input.local);

    // Airfield: dark circular base, side outline, health ring and runway cross.
    if (input.kind == 0u) {
        if (radius > 1.0) { discard; }
        if (radius > 0.82) {
            var health_color = vec3(0.18, 0.80, 0.44);
            if (input.value <= 0.5) { health_color = vec3(0.95, 0.61, 0.07); }
            if (input.value <= 0.0 || (input.flags & 1u) != 0u) {
                health_color = vec3(0.91, 0.30, 0.24);
            }
            return vec4(health_color, input.color.a);
        }
        if (radius > 0.68) { return input.color; }
        let runway = (abs(x) < 0.09 && abs(y) < 0.55) || (abs(y) < 0.09 && abs(x) < 0.55);
        if (runway) {
            if ((input.flags & 1u) != 0u) { return vec4(0.91, 0.30, 0.24, 0.95); }
            return vec4(0.86, 0.91, 0.94, 0.95);
        }
        return vec4(0.055, 0.075, 0.095, 0.86);
    }

    // Fighter/strike marker: a compact aircraft silhouette facing east.
    if (input.kind == 1u || input.kind == 2u) {
        let body = abs(y) < 0.13 && x > -0.82 && x < 0.88;
        let nose = x >= 0.62 && abs(y) < (0.92 - x) * 0.55;
        let wings = abs(x + 0.03) < 0.20 && abs(y) < 0.76;
        let tail = x < -0.55 && x > -0.82 && abs(y) < 0.34;
        if (!(body || nose || wings || tail)) { discard; }
        var color = input.color;
        if (input.kind == 1u) {
            color = vec4(mix(color.rgb, vec3(0.82, 0.93, 1.0), 0.22), color.a);
        } else {
            color = vec4(mix(color.rgb, vec3(1.0, 0.68, 0.22), 0.28), color.a);
        }
        return vec4(color.rgb, color.a * (0.55 + 0.45 * clamp(input.value, 0.0, 1.0)));
    }

    // Battle cluster: crossed procedural blades with a soft browser-like glow.
    if (input.kind == 3u) {
        if (radius > 1.0) { discard; }
        let blade_a = abs(y - x) < 0.13 && abs(x) < 0.76;
        let blade_b = abs(y + x) < 0.13 && abs(x) < 0.76;
        let guard_a = abs(y - x - 0.30) < 0.10 && abs(x + y) < 0.28;
        let guard_b = abs(y + x + 0.30) < 0.10 && abs(x - y) < 0.28;
        if (blade_a || blade_b || guard_a || guard_b) {
            return vec4(mix(vec3(1.0), input.color.rgb, 0.42), input.color.a);
        }
        return vec4(input.color.rgb, (1.0 - radius) * 0.18);
    }

    // Hostile operational contact: orange diamond and cross, dashed when stale.
    if (input.kind == 4u) {
        let diamond = abs(x) + abs(y);
        if (diamond > 0.90 || diamond < 0.62) {
            let cross = (abs(x) < 0.07 && abs(y) < 0.98) || (abs(y) < 0.07 && abs(x) < 0.98);
            if (!cross) { discard; }
        }
        if ((input.flags & 1u) != 0u && fract((x + y + 2.0) * 4.0) > 0.60) { discard; }
        return input.color;
    }

    // Task-force anchors: assembly circle, objective diamond, withdrawal square.
    if (input.kind == 5u) {
        if (input.flags == 0u) {
            if (radius > 0.88 || radius < 0.58) { discard; }
        } else if (input.flags == 1u) {
            let diamond = abs(x) + abs(y);
            if (diamond > 0.92 || diamond < 0.62) { discard; }
        } else {
            let edge = max(abs(x), abs(y));
            if (edge > 0.88 || edge < 0.62) { discard; }
        }
        return input.color;
    }

    discard;
}

struct SegmentOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) along: f32,
    @location(2) across: f32,
    @location(3) @interpolate(flat) flags: u32,
};

@vertex
fn vs_segment(
    @builtin(vertex_index) vertex_index: u32,
    @location(1) start_world: vec2<f32>,
    @location(2) end_world: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) width: f32,
    @location(5) flags: u32,
) -> SegmentOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, -1.0),
        vec2(1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(0.0, -1.0),
        vec2(1.0, 1.0),
        vec2(0.0, 1.0),
    );
    let corner = corners[vertex_index];
    let start = (start_world - view.center) * view.pixels_per_world + view.viewport * 0.5;
    let end = (end_world - view.center) * view.pixels_per_world + view.viewport * 0.5;
    let delta = end - start;
    let distance = max(length(delta), 0.001);
    let direction = delta / distance;
    let normal = vec2(-direction.y, direction.x);
    let screen = mix(start, end, corner.x) + normal * corner.y * width * 0.5;
    var output: SegmentOutput;
    output.position = vec4((screen / view.viewport) * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    output.color = color;
    output.along = corner.x * distance;
    output.across = corner.y;
    output.flags = flags;
    return output;
}

@fragment
fn fs_segment(input: SegmentOutput) -> @location(0) vec4<f32> {
    if ((input.flags & 1u) != 0u && fract(input.along / 14.0) > 0.58) { discard; }
    let edge_alpha = 1.0 - smoothstep(0.72, 1.0, abs(input.across));
    return vec4(input.color.rgb, input.color.a * edge_alpha * 0.86);
}
