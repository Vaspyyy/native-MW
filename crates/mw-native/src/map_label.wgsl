struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // radius in screen pixels, halo alpha, softness, atlas pixels per screen pixel
    @location(2) effect: vec4<f32>,
    @location(3) textured: f32,
};

fn camera_delta(world: vec2<f32>) -> vec2<f32> {
    var delta = world - view.center;
    delta.x -= floor((delta.x + 1.0) * 0.5) * 2.0;
    return delta;
}

@vertex
fn vs_main(
    @location(0) world: vec2<f32>,
    @location(1) offset: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) effect: vec4<f32>,
    @location(5) textured: f32,
) -> VertexOutput {
    let screen = camera_delta(world) * view.pixels_per_world + view.viewport * 0.5 + offset;
    var output: VertexOutput;
    output.position = vec4((screen / view.viewport) * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    output.uv = uv;
    output.color = color;
    output.effect = effect;
    output.textured = textured;
    return output;
}

fn glyph_coverage(uv: vec2<f32>) -> f32 {
    return textureSample(glyph_atlas, glyph_sampler, uv).r;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.textured < 0.5 {
        return input.color;
    }

    let fill = glyph_coverage(input.uv);
    if input.effect.x <= 0.0 || input.effect.y <= 0.0 {
        return vec4(input.color.rgb, input.color.a * fill);
    }

    let dimensions = vec2<f32>(textureDimensions(glyph_atlas));
    let radius = input.effect.x * input.effect.w / dimensions;
    let half_radius = radius * 0.5;
    let diagonal = vec2<f32>(0.70710678);

    var inner = fill;
    inner = max(inner, glyph_coverage(input.uv + vec2<f32>(half_radius.x, 0.0)));
    inner = max(inner, glyph_coverage(input.uv - vec2<f32>(half_radius.x, 0.0)));
    inner = max(inner, glyph_coverage(input.uv + vec2<f32>(0.0, half_radius.y)));
    inner = max(inner, glyph_coverage(input.uv - vec2<f32>(0.0, half_radius.y)));
    inner = max(inner, glyph_coverage(input.uv + half_radius * diagonal));
    inner = max(inner, glyph_coverage(input.uv - half_radius * diagonal));
    inner = max(inner, glyph_coverage(input.uv + half_radius * vec2<f32>(diagonal.x, -diagonal.y)));
    inner = max(inner, glyph_coverage(input.uv + half_radius * vec2<f32>(-diagonal.x, diagonal.y)));

    var outer = inner;
    outer = max(outer, glyph_coverage(input.uv + vec2<f32>(radius.x, 0.0)));
    outer = max(outer, glyph_coverage(input.uv - vec2<f32>(radius.x, 0.0)));
    outer = max(outer, glyph_coverage(input.uv + vec2<f32>(0.0, radius.y)));
    outer = max(outer, glyph_coverage(input.uv - vec2<f32>(0.0, radius.y)));
    outer = max(outer, glyph_coverage(input.uv + radius * diagonal));
    outer = max(outer, glyph_coverage(input.uv - radius * diagonal));
    outer = max(outer, glyph_coverage(input.uv + radius * vec2<f32>(diagonal.x, -diagonal.y)));
    outer = max(outer, glyph_coverage(input.uv + radius * vec2<f32>(-diagonal.x, diagonal.y)));

    let sharp_halo = outer;
    let soft_halo = max(inner * 0.72, outer * 0.38);
    let halo_coverage = mix(sharp_halo, soft_halo, clamp(input.effect.z, 0.0, 1.0));
    let fill_alpha = fill * input.color.a;
    let halo_alpha = halo_coverage * input.effect.y;
    let combined_alpha = fill_alpha + halo_alpha * (1.0 - fill_alpha);
    let premultiplied_rgb = input.color.rgb * fill_alpha;
    let straight_rgb = select(
        vec3<f32>(0.0),
        premultiplied_rgb / max(combined_alpha, 0.00001),
        combined_alpha > 0.00001,
    );
    return vec4(straight_rgb, combined_alpha);
}
