struct View {
    viewport: vec2<f32>,
    center: vec2<f32>,
    pixels_per_world: f32,
    palette_len: u32,
    grid_size: vec2<u32>,
    frontlines_active: u32,
    output_is_srgb: u32,
};

@group(0) @binding(0) var ownership: texture_2d<u32>;
@group(0) @binding(1) var<uniform> view: View;
@group(0) @binding(2) var<storage, read> palette: array<vec4<f32>>;
@group(0) @binding(3) var dominant_sides: texture_2d<i32>;
@group(0) @binding(4) var sovereign_ownership: texture_2d<u32>;
@group(0) @binding(5) var geographic_land: texture_2d<u32>;
@group(0) @binding(6) var biome: texture_2d<u32>;
@group(0) @binding(7) var<storage, read> sovereign_sides: array<i32>;
@group(0) @binding(8) var<storage, read> country_y_bounds: array<vec2<u32>>;
@group(0) @binding(9) var<storage, read> occupation_palette: array<vec4<f32>>;

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

fn wrapped_cell(cell: vec2<i32>) -> vec2<i32> {
    let width = max(i32(view.grid_size.x), 1);
    let height = max(i32(view.grid_size.y), 1);
    let x = ((cell.x % width) + width) % width;
    return vec2<i32>(x, clamp(cell.y, 0, height - 1));
}

fn owner_at(cell: vec2<i32>) -> u32 {
    return textureLoad(ownership, wrapped_cell(cell), 0).r;
}

fn side_at(cell: vec2<i32>) -> i32 {
    return textureLoad(dominant_sides, wrapped_cell(cell), 0).r;
}

fn sovereign_at(cell: vec2<i32>) -> u32 {
    return textureLoad(sovereign_ownership, wrapped_cell(cell), 0).r;
}

fn land_at(cell: vec2<i32>) -> u32 {
    return textureLoad(geographic_land, wrapped_cell(cell), 0).r;
}

fn biome_at(cell: vec2<i32>) -> u32 {
    return textureLoad(biome, wrapped_cell(cell), 0).r;
}

fn country_side_at(country_id: u32) -> i32 {
    if (country_id >= view.palette_len) {
        return -1;
    }
    return sovereign_sides[country_id];
}

fn political_id(cell: vec2<i32>) -> i32 {
    if (land_at(cell) == 0u) {
        return -1;
    }
    return i32(sovereign_at(cell));
}

fn desert_transform(color: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        min(1.0, color.r * 1.1 + 30.0 / 255.0),
        min(1.0, color.g * 1.1 + 10.0 / 255.0),
        max(0.0, color.b * 0.85),
    );
}

fn grid_y_to_world_y(grid_y: f32) -> f32 {
    let latitude = clamp(
        (grid_y / max(f32(view.grid_size.y), 1.0)) * 3.141592653589793
            - 1.5707963267948966,
        -1.4844222297453324,
        1.4844222297453324,
    );
    return 1.0
        - log(tan(0.7853981633974483 + latitude * 0.5)) / 3.141592653589793;
}

fn country_gradient(color: vec3<f32>, country_id: u32, world_y: f32) -> vec3<f32> {
    if (country_id == 0u || country_id >= view.palette_len) {
        return color;
    }
    let bounds = country_y_bounds[country_id];
    let south_y = grid_y_to_world_y(f32(bounds.x));
    let north_y = grid_y_to_world_y(f32(bounds.y));
    let span = north_y - south_y;
    var t = 0.0;
    if (abs(span) > 0.000001) {
        t = clamp((world_y - south_y) / span, 0.0, 1.0);
    }
    let bright = min(color + vec3<f32>(25.0 / 255.0), vec3<f32>(1.0));
    if (t < 0.3) {
        return mix(bright, color, t / 0.3);
    }
    return mix(color, floor(color * 0.65 * 255.0) / 255.0, (t - 0.3) / 0.7);
}

fn world_y_to_latitude_radians(world_y: f32) -> f32 {
    // Leaflet's default CRS is EPSG:3857. Native world space keeps the
    // existing two-unit world width, so its square Mercator world is 2x2.
    let mercator_y = 3.141592653589793 * (1.0 - world_y);
    return 2.0 * atan(exp(mercator_y)) - 1.5707963267948966;
}

fn world_y_to_latitude(world_y: f32) -> f32 {
    return world_y_to_latitude_radians(world_y) * (180.0 / 3.141592653589793);
}

fn ocean_color(screen_y: f32) -> vec3<f32> {
    // Browser WarGames paints a 12-stop viewport gradient. Preserve its
    // viewport-relative latitude sample and stop-index/center-longitude noise.
    let pct = clamp(screen_y / max(view.viewport.y, 1.0), 0.0, 1.0);
    let scaled = pct * 12.0;
    let lower = u32(floor(scaled));
    let upper = min(lower + 1u, 12u);
    let center_lng = view.center.x * 180.0 - 180.0;
    let lower_y = view.viewport.y * f32(lower) / 12.0;
    let upper_y = view.viewport.y * f32(upper) / 12.0;
    let lower_world_y = view.center.y + (lower_y - view.viewport.y * 0.5) / view.pixels_per_world;
    let upper_world_y = view.center.y + (upper_y - view.viewport.y * 0.5) / view.pixels_per_world;
    let lower_lat = world_y_to_latitude(lower_world_y);
    let upper_lat = world_y_to_latitude(upper_world_y);
    let lower_noise = sin(f32(lower) * 0.7 + center_lng * 0.04) * 3.5;
    let upper_noise = sin(f32(upper) * 0.7 + center_lng * 0.04) * 3.5;
    let lower_t = clamp((abs(lower_lat) + lower_noise) / 90.0, 0.0, 1.0);
    let upper_t = clamp((abs(upper_lat) + upper_noise) / 90.0, 0.0, 1.0);
    let lower_rgb = floor(mix(vec3<f32>(5.0, 52.0, 72.0), vec3<f32>(2.0, 18.0, 34.0), lower_t) + 0.5) / 255.0;
    let upper_rgb = floor(mix(vec3<f32>(5.0, 52.0, 72.0), vec3<f32>(2.0, 18.0, 34.0), upper_t) + 0.5) / 255.0;
    return mix(lower_rgb, upper_rgb, fract(scaled));
}

fn srgb_channel_to_linear(value: f32) -> f32 {
    if (value <= 0.04045) {
        return value / 12.92;
    }
    return pow((value + 0.055) / 1.055, 2.4);
}

fn srgb_to_linear(color: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_channel_to_linear(color.r),
        srgb_channel_to_linear(color.g),
        srgb_channel_to_linear(color.b),
    );
}

fn output_color(color: vec3<f32>) -> vec3<f32> {
    if (view.output_is_srgb != 0u) {
        return srgb_to_linear(color);
    }
    return color;
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

fn frontline_distance(
    grid_position: vec2<f32>,
    cell: vec2<i32>,
    cell_pixels: vec2<f32>,
) -> f32 {
    // Longitude is cyclic: the final cell's right-hand neighbor is the first
    // cell in the repeated world. Latitude remains bounded at the poles.
    let maximum_quad_y = max(i32(view.grid_size.y) - 2, 0);
    let quad = vec2<i32>(wrapped_cell(cell).x, clamp(cell.y, 0, maximum_quad_y));
    let local = vec2<f32>(
        fract(grid_position.x),
        grid_position.y - f32(quad.y),
    ) * cell_pixels;
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
        distance = segment_distance(local, crossings[0] * cell_pixels, crossings[1] * cell_pixels);
        if (count >= 3u) {
            distance = min(
                distance,
                segment_distance(local, crossings[1] * cell_pixels, crossings[2] * cell_pixels),
            );
        }
    } else if (count == 1u) {
        distance = segment_distance(
            local,
            crossings[0] * cell_pixels,
            vec2<f32>(0.5) * cell_pixels,
        );
    }
    return distance;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let unwrapped_world =
        view.center + (position.xy - view.viewport * 0.5) / view.pixels_per_world;
    let ocean = ocean_color(position.y);
    if (unwrapped_world.y < 0.0 || unwrapped_world.y >= 2.0) {
        return vec4<f32>(output_color(ocean), 1.0);
    }
    let world = vec2<f32>(
        unwrapped_world.x - floor(unwrapped_world.x * 0.5) * 2.0,
        unwrapped_world.y,
    );

    // Scenario rows remain linear latitude south-to-north. Invert Web Mercator
    // only at the renderer boundary so simulation/checkpoint coordinates stay
    // in geographic degrees.
    let latitude_radians = world_y_to_latitude_radians(world.y);
    let latitude = latitude_radians * (180.0 / 3.141592653589793);
    let grid_uv = vec2<f32>(world.x * 0.5, (latitude + 90.0) / 180.0);
    let grid_position = grid_uv * vec2<f32>(view.grid_size);
    let cell = min(vec2<i32>(grid_position), vec2<i32>(view.grid_size) - vec2<i32>(1));
    let local = fract(grid_position);
    let sovereign = sovereign_at(cell);
    let effective_owner = owner_at(cell);
    let land = land_at(cell);
    if (land == 0u) {
        return vec4<f32>(output_color(ocean), 1.0);
    }
    let dominant_side = side_at(cell);
    let sovereign_side = country_side_at(sovereign);
    let war_material_active = view.frontlines_active != 0u;
    let occupied = war_material_active && dominant_side >= 0 && dominant_side != sovereign_side;
    let friendly_war_land = war_material_active && dominant_side >= 0 && dominant_side == sovereign_side;
    var display_owner = sovereign;
    var color = vec3<f32>(20.0 / 255.0, 38.0 / 255.0, 20.0 / 255.0);
    var alpha = 1.0;
    if (sovereign != 0u) {
        if (occupied && effective_owner != 0u) {
            display_owner = effective_owner;
            color = occupation_palette[min(effective_owner, view.palette_len - 1u)].rgb;
            color = mix(color, vec3<f32>(1.0), 0.3);
            alpha = 0.85;
        } else {
            color = palette[min(sovereign, view.palette_len - 1u)].rgb;
            if (friendly_war_land) {
                alpha = 0.70;
            }
        }
    } else if (biome_at(cell) == 1u) {
        color = vec3<f32>(140.0 / 255.0, 120.0 / 255.0, 70.0 / 255.0);
    }
    if (biome_at(cell) == 1u) {
        color = desert_transform(color);
    }
    color = country_gradient(color, display_owner, world.y);
    // The swapchain is opaque. Match browser alpha fills by compositing them
    // against the simplified ocean rather than dropping alpha at the output.
    var map_color = mix(ocean, color, alpha);

    let cell_pixels = max(
        vec2<f32>(
            view.pixels_per_world * 2.0 / f32(view.grid_size.x),
            view.pixels_per_world
                / (f32(view.grid_size.y) * max(cos(latitude_radians), 0.000001)),
        ),
        vec2<f32>(0.001),
    );
    var frontline_alpha = 0.0;
    if (view.frontlines_active != 0u) {
        let browser_zoom = max(log2(max(view.pixels_per_world / 128.0, 1.0)), 0.0);
        let frontline_width = max(1.2, 3.5 * (browser_zoom / 5.0));
        let frontline_pixels = frontline_distance(grid_position, cell, cell_pixels);
        frontline_alpha = 1.0 - smoothstep(
            max(0.0, frontline_width * 0.5 - 0.75),
            frontline_width * 0.5 + 0.75,
            frontline_pixels,
        );
    }

    // Browser pass 2 draws the black controller frontline before its thin
    // political-border pass.
    map_color = mix(map_color, vec3<f32>(0.0), frontline_alpha);

    let political = political_id(cell);
    if (political >= 0) {
        let border_half = clamp(vec2<f32>(0.5) / cell_pixels, vec2<f32>(0.0), vec2<f32>(0.5));
        let border =
            (local.x < border_half.x && political_id(cell + vec2<i32>(-1, 0)) != political) ||
            (1.0 - local.x < border_half.x && political_id(cell + vec2<i32>(1, 0)) != political) ||
            (local.y < border_half.y && political_id(cell + vec2<i32>(0, -1)) != political) ||
            (1.0 - local.y < border_half.y && political_id(cell + vec2<i32>(0, 1)) != political);
        if (border) {
            map_color = mix(map_color, vec3<f32>(0.0), 0.3);
        }
    }
    // The preferred swapchain format is sRGB. All browser material math above
    // operates in Canvas/CSS sRGB space, so linearize only at the final write.
    return vec4<f32>(output_color(map_color), 1.0);
}
