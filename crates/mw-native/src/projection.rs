//! Shared EPSG:3857 projection used by native map renderers.

pub const WORLD_WIDTH: f64 = 2.0;
pub const TILE_SIZE: f32 = 256.0;
pub const MAX_LATITUDE: f64 = 85.0511287798066;

pub fn geographic_to_world(lat: f64, lng: f64) -> [f32; 2] {
    let lat = lat.clamp(-MAX_LATITUDE, MAX_LATITUDE).to_radians();
    let mut x = ((lng + 180.0) / 180.0).rem_euclid(WORLD_WIDTH);
    if (lng - 180.0).abs() < f64::EPSILON {
        x = WORLD_WIDTH;
    }
    let y = (1.0 - (lat.tan().asinh() / std::f64::consts::PI)).clamp(0.0, 2.0);
    [x as f32, y as f32]
}

pub fn world_to_geographic(world: [f64; 2]) -> [f64; 2] {
    let x = world[0].rem_euclid(WORLD_WIDTH);
    let mercator = (1.0 - world[1]) * std::f64::consts::PI;
    let lat = mercator.sinh().atan().to_degrees();
    let lng = if x == 0.0 && world[0] > 0.0 {
        180.0
    } else {
        x * 180.0 - 180.0
    };
    [lat.clamp(-MAX_LATITUDE, MAX_LATITUDE), lng]
}

pub fn browser_zoom(pixels_per_world: f32) -> f32 {
    (pixels_per_world.max(1.0) * WORLD_WIDTH as f32 / TILE_SIZE).log2()
}

pub fn pixels_per_world_for_zoom(zoom: f32) -> f32 {
    TILE_SIZE / WORLD_WIDTH as f32 * 2.0_f32.powf(zoom)
}

pub fn wrapped_world_delta_x(world_x: f32, center_x: f32) -> f32 {
    (world_x - center_x + 1.0).rem_euclid(WORLD_WIDTH as f32) - 1.0
}

pub fn world_to_grid(world: [f64; 2], width: usize, height: usize) -> Option<(usize, usize)> {
    if width == 0
        || height == 0
        || !world[0].is_finite()
        || !world[1].is_finite()
        || !(0.0..=WORLD_WIDTH).contains(&world[1])
    {
        return None;
    }
    let x = (world[0].rem_euclid(WORLD_WIDTH) * width as f64 / WORLD_WIDTH).floor() as usize;
    let lat = world_to_geographic(world)[0];
    let y = (((lat + 90.0) / 180.0) * height as f64)
        .floor()
        .min((height - 1) as f64) as usize;
    Some((x.min(width - 1), y))
}

pub fn grid_to_world(x: f64, y: f64, width: f64, height: f64) -> [f32; 2] {
    geographic_to_world(
        -90.0 + (y + 0.5) / height * 180.0,
        -180.0 + (x + 0.5) / width * 360.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_values_and_round_trip() {
        assert_eq!(geographic_to_world(0.0, 0.0), [1.0, 1.0]);
        for point in [[0.0, 0.0], [45.0, 12.0], [-70.0, -179.0]] {
            let world = geographic_to_world(point[0], point[1]);
            let back = world_to_geographic([world[0] as f64, world[1] as f64]);
            assert!((back[0] - point[0]).abs() < 1e-4);
            assert!((back[1] - point[1]).abs() < 1e-4);
        }
    }
    #[test]
    fn latitude_is_clamped() {
        assert_eq!(
            geographic_to_world(90.0, 0.0),
            geographic_to_world(MAX_LATITUDE, 0.0)
        );
        assert_eq!(
            geographic_to_world(-90.0, 0.0),
            geographic_to_world(-MAX_LATITUDE, 0.0)
        );
    }

    #[test]
    fn antimeridian_edges_wrap_for_rendering_but_inverse_cleanly() {
        assert_eq!(geographic_to_world(0.0, -180.0), [0.0, 1.0]);
        assert_eq!(geographic_to_world(0.0, 180.0), [2.0, 1.0]);
        assert_eq!(world_to_geographic([0.0, 1.0]), [0.0, -180.0]);
        assert_eq!(world_to_geographic([2.0, 1.0]), [0.0, 180.0]);
        assert_eq!(world_to_grid([2.0, 1.0], 2_400, 1_200), Some((0, 600)));
        assert!((wrapped_world_delta_x(1.99, 0.01) + 0.02).abs() < 1.0e-6);
    }
}
