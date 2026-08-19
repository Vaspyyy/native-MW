//! Shared immutable world-grid access for native simulation kernels.

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldGridView<'a> {
    pub grid_res: f64,
    pub width: usize,
    pub height: usize,
    pub land_mask: &'a [u8],
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorldGridError {
    #[error("world grid resolution must be finite and greater than zero")]
    InvalidResolution,
    #[error("world grid dimensions must be non-zero and addressable")]
    InvalidDimensions,
    #[error("world land-mask length does not match its dimensions")]
    InvalidLandMaskLength,
}

impl<'a> WorldGridView<'a> {
    pub fn new(
        grid_res: f64,
        width: usize,
        height: usize,
        land_mask: &'a [u8],
    ) -> Result<Self, WorldGridError> {
        let view = Self {
            grid_res,
            width,
            height,
            land_mask,
        };
        view.validate()?;
        Ok(view)
    }

    pub fn validate(self) -> Result<(), WorldGridError> {
        if !self.grid_res.is_finite() || self.grid_res <= 0.0 {
            return Err(WorldGridError::InvalidResolution);
        }
        let Some(cell_count) = self.width.checked_mul(self.height) else {
            return Err(WorldGridError::InvalidDimensions);
        };
        if self.width == 0 || self.height == 0 {
            return Err(WorldGridError::InvalidDimensions);
        }
        if self.land_mask.len() != cell_count {
            return Err(WorldGridError::InvalidLandMaskLength);
        }
        Ok(())
    }

    /// Mirrors `src/engine.js::getGridIndex` for finite coordinates.
    pub fn grid_index(self, lat: f64, lng: f64) -> Option<usize> {
        if !lat.is_finite() || !lng.is_finite() {
            return None;
        }
        let wrapped_lng = ((((lng + 180.0) % 360.0) + 360.0) % 360.0) - 180.0;
        let x = ((wrapped_lng + 180.0) / self.grid_res).floor();
        let y = ((lat + 90.0) / self.grid_res).floor();
        if x < 0.0 || y < 0.0 || x >= self.width as f64 || y >= self.height as f64 {
            return None;
        }
        let x = x as usize;
        let y = y as usize;
        y.checked_mul(self.width)?.checked_add(x)
    }

    pub fn is_land(self, lat: f64, lng: f64) -> bool {
        self.grid_index(lat, lng)
            .and_then(|index| self.land_mask.get(index))
            .is_some_and(|value| *value > 0)
    }

    pub fn is_water(self, lat: f64, lng: f64) -> bool {
        self.grid_index(lat, lng)
            .and_then(|index| self.land_mask.get(index))
            .is_some_and(|value| *value == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexing_matches_browser_world_wrap_and_bounds() {
        let land = vec![1; 360 * 180];
        let grid = WorldGridView::new(1.0, 360, 180, &land).unwrap();
        assert_eq!(grid.grid_index(-90.0, -180.0), Some(0));
        assert_eq!(grid.grid_index(-90.0, 180.0), Some(0));
        assert_eq!(grid.grid_index(-90.0, 540.0), Some(0));
        assert_eq!(grid.grid_index(89.999, 179.999), Some(360 * 180 - 1));
        assert_eq!(grid.grid_index(90.0, 0.0), None);
        assert_eq!(grid.grid_index(f64::NAN, 0.0), None);
    }

    #[test]
    fn constructor_rejects_invalid_shapes() {
        assert_eq!(
            WorldGridView::new(0.0, 1, 1, &[1]),
            Err(WorldGridError::InvalidResolution)
        );
        assert_eq!(
            WorldGridView::new(1.0, 2, 2, &[1]),
            Err(WorldGridError::InvalidLandMaskLength)
        );
    }
}
