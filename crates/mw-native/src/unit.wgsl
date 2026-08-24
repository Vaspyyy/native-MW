struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var flag_atlas: texture_2d<f32>;
@group(0) @binding(2) var flag_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) flag_uv: vec4<f32>,
    @location(2) visual_seed: f32,
    @location(3) @interpolate(flat) kind: u32,
    @location(4) @interpolate(flat) flags: u32,
    @location(5) local_px: vec2<f32>,
    @location(6) dimensions: vec2<f32>,
};

fn browser_zoom() -> f32 {
    return log2(max(view.pixels_per_world, 1.0) * 2.0 / 256.0);
}

fn camera_delta(world: vec2<f32>) -> vec2<f32> {
    var delta = world - view.center;
    delta.x -= floor((delta.x + 1.0) * 0.5) * 2.0;
    return delta;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @location(1) world: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) flag_uv: vec4<f32>,
    @location(4) visual_seed: f32,
    @location(5) kind: u32,
    @location(6) flags: u32,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0),
        vec2(1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(-1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(-1.0, 1.0),
    );
    let zoom_scale = pow(1.3, browser_zoom() - 3.0);
    let dimensions = vec2(7.0, 4.5) * zoom_scale;
    let half_extent = dimensions * vec2(0.72, 0.64);
    let local = corners[vertex_index];
    let local_px = local * half_extent;
    let screen = camera_delta(world) * view.pixels_per_world + view.viewport * 0.5 + local_px;

    var output: VertexOutput;
    output.position = vec4(
        (screen / view.viewport) * vec2(2.0, -2.0) + vec2(-1.0, 1.0),
        0.0,
        1.0,
    );
    output.color = color;
    output.flag_uv = flag_uv;
    output.visual_seed = visual_seed;
    output.kind = kind;
    output.flags = flags;
    output.local_px = local_px;
    output.dimensions = dimensions;
    return output;
}

fn alpha_over(bottom: vec4<f32>, top: vec4<f32>) -> vec4<f32> {
    let alpha = top.a + bottom.a * (1.0 - top.a);
    let premultiplied = top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a);
    return vec4(premultiplied / max(alpha, 0.00001), alpha);
}

fn rect_coverage(point: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let distance = max(abs(point.x) - half_size.x, abs(point.y) - half_size.y);
    return 1.0 - smoothstep(-0.65, 0.65, distance);
}

fn circle_coverage(point: vec2<f32>, center: vec2<f32>, radius: f32) -> f32 {
    let distance = length(point - center);
    return 1.0 - smoothstep(radius - 0.65, radius + 0.65, distance);
}

fn cross_2d(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return a.x * b.y - a.y * b.x;
}

fn edge_distance(a: vec2<f32>, b: vec2<f32>, point: vec2<f32>) -> f32 {
    let edge = b - a;
    return cross_2d(edge, point - a) / max(length(edge), 0.00001);
}

fn triangle_coverage(
    point: vec2<f32>,
    a: vec2<f32>,
    b: vec2<f32>,
    c: vec2<f32>,
) -> f32 {
    let distance = min(
        edge_distance(a, b, point),
        min(edge_distance(b, c, point), edge_distance(c, a, point)),
    );
    return smoothstep(-0.65, 0.65, distance);
}

fn solid_layer(color: vec4<f32>, coverage: f32) -> vec4<f32> {
    return vec4(color.rgb, color.a * coverage);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let zoom = browser_zoom();
    let draw_probability = select(select(1.0, 0.5, zoom < 4.0), 0.2, zoom < 3.0);
    if (input.visual_seed > draw_probability) {
        discard;
    }

    let point = input.local_px;
    let width = input.dimensions.x;
    let height = input.dimensions.y;
    let zoom_scale = width / 7.0;
    var result = vec4(0.0);

    if ((input.flags & 2u) != 0u) {
        let hull_a = vec2(-width * 0.5, height * 0.25);
        let hull_b = vec2(width * 0.5, height * 0.25);
        let hull_c = vec2(width * 0.25, height * 0.5);
        let hull_d = vec2(-width * 0.25, height * 0.5);
        let hull = max(
            triangle_coverage(point, hull_a, hull_b, hull_c),
            triangle_coverage(point, hull_a, hull_c, hull_d),
        );
        result = alpha_over(result, solid_layer(input.color, hull));
        let sail = triangle_coverage(
            point,
            vec2(0.0, height * 0.25),
            vec2(0.0, -height * 0.5),
            vec2(width / 3.0, height * 0.125),
        );
        result = alpha_over(result, solid_layer(vec4(1.0), sail));
    } else {
        let flag_half = vec2(width * 0.5, height * 0.5);
        let flag_fill = rect_coverage(point, flag_half);
        if ((input.flags & 1u) != 0u) {
            let local_uv = clamp(point / input.dimensions + vec2(0.5), vec2(0.0), vec2(1.0));
            let atlas_uv = mix(input.flag_uv.xy, input.flag_uv.zw, local_uv);
            var sampled = textureSample(flag_atlas, flag_sampler, atlas_uv);
            sampled.a *= flag_fill;
            result = alpha_over(result, sampled);
            let stroke_width = max(0.3, 0.3 * zoom_scale);
            let outer = rect_coverage(point, flag_half + vec2(stroke_width * 0.5));
            let inner = rect_coverage(
                point,
                max(flag_half - vec2(stroke_width * 0.5), vec2(0.0)),
            );
            result = alpha_over(
                result,
                solid_layer(vec4(0.0, 0.0, 0.0, 0.3), max(outer - inner, 0.0)),
            );
        } else {
            result = alpha_over(result, solid_layer(input.color, flag_fill));
        }

        if (input.kind == 1u) {
            let body_half = vec2(width * 0.55, height * 0.45);
            let body_fill = rect_coverage(point, body_half);
            result = alpha_over(
                result,
                solid_layer(vec4(20.0 / 255.0, 24.0 / 255.0, 22.0 / 255.0, 0.62), body_fill),
            );
            let stroke_width = max(0.8, zoom_scale * 0.7);
            let body_outer = rect_coverage(point, body_half + vec2(stroke_width * 0.5));
            let body_inner = rect_coverage(
                point,
                max(body_half - vec2(stroke_width * 0.5), vec2(0.0)),
            );
            result = alpha_over(
                result,
                solid_layer(input.color, max(body_outer - body_inner, 0.0)),
            );
            let turret = circle_coverage(
                point,
                vec2(0.0, -height * 0.1),
                height * 0.38,
            );
            result = alpha_over(result, solid_layer(input.color, turret));
            let barrel = rect_coverage(
                point - vec2(width * 0.325, -height * 0.11),
                vec2(width * 0.325, height * 0.07),
            );
            result = alpha_over(result, solid_layer(input.color, barrel));
        }
    }

    if (result.a <= 0.001) {
        discard;
    }
    return result;
}
