//! Deterministic direction field pointing each traversable cell at its nearest
//! hostile frontline cell.
//!
//! This intentionally preserves the ordering of the browser worker: sources
//! are discovered in ascending cell order and breadth-first neighbors are
//! visited right, left, down, then up. That ordering is observable when two
//! sources are equally near.

use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub struct HostilityMatrix<'a> {
    /// Row-major `max_sides * max_sides` relation matrix. When absent, every
    /// pair of distinct valid sides is hostile, matching the web worker.
    pub relations: Option<&'a [u8]>,
    pub max_sides: usize,
}

impl<'a> HostilityMatrix<'a> {
    pub const fn new(relations: Option<&'a [u8]>, max_sides: usize) -> Self {
        Self {
            relations,
            max_sides,
        }
    }

    fn is_hostile(self, left: i16, right: i16) -> bool {
        if left < 0 || right < 0 || left == right {
            return false;
        }
        let left = left as usize;
        let right = right as usize;
        match self.relations {
            None => true,
            Some(relations) => {
                left < self.max_sides
                    && right < self.max_sides
                    && relations[left * self.max_sides + right] == 1
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DirectionFieldInput<'a> {
    pub land_mask: &'a [u8],
    /// Shared signed controller map used by territory, AI, and field layout.
    /// The browser currently transfers Int8 values, but the native runtime
    /// keeps one i16 map so field refreshes do not copy the full world grid.
    pub dominant_side_map: &'a [i16],
    pub hostility: HostilityMatrix<'a>,
    pub grid_width: usize,
    pub grid_height: usize,
    pub grid_res: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectionField {
    pub latitude: Vec<f32>,
    pub longitude: Vec<f32>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DirectionFieldError {
    #[error("grid dimensions must both be non-zero")]
    EmptyGrid,
    #[error("grid dimensions overflow addressable memory")]
    GridSizeOverflow,
    #[error("land mask length {actual} does not match grid size {expected}")]
    LandMaskLength { expected: usize, actual: usize },
    #[error("dominant-side map length {actual} does not match grid size {expected}")]
    DominantSideMapLength { expected: usize, actual: usize },
    #[error("max_sides must be non-zero")]
    EmptyHostilityMatrix,
    #[error("hostility matrix dimensions overflow addressable memory")]
    HostilitySizeOverflow,
    #[error("hostility matrix length {actual} does not match {expected}")]
    HostilityLength { expected: usize, actual: usize },
}

pub fn build_direction_field(
    input: DirectionFieldInput<'_>,
) -> Result<DirectionField, DirectionFieldError> {
    if input.grid_width == 0 || input.grid_height == 0 {
        return Err(DirectionFieldError::EmptyGrid);
    }
    let total = input
        .grid_width
        .checked_mul(input.grid_height)
        .ok_or(DirectionFieldError::GridSizeOverflow)?;
    if input.land_mask.len() != total {
        return Err(DirectionFieldError::LandMaskLength {
            expected: total,
            actual: input.land_mask.len(),
        });
    }
    if input.dominant_side_map.len() != total {
        return Err(DirectionFieldError::DominantSideMapLength {
            expected: total,
            actual: input.dominant_side_map.len(),
        });
    }
    if input.hostility.max_sides == 0 {
        return Err(DirectionFieldError::EmptyHostilityMatrix);
    }
    if let Some(relations) = input.hostility.relations {
        let expected = input
            .hostility
            .max_sides
            .checked_mul(input.hostility.max_sides)
            .ok_or(DirectionFieldError::HostilitySizeOverflow)?;
        if relations.len() != expected {
            return Err(DirectionFieldError::HostilityLength {
                expected,
                actual: relations.len(),
            });
        }
    }

    let mut latitude = vec![0.0_f32; total];
    let mut longitude = vec![0.0_f32; total];
    let mut source_cell = vec![usize::MAX; total];
    // Fixed-capacity queue mirrors the JavaScript worker's Int32Array queue and
    // avoids per-pop bookkeeping in this hot breadth-first traversal.
    let mut queue = vec![0_usize; total];
    let mut queue_head = 0;
    let mut queue_tail = 0;

    for (index, &land) in input.land_mask.iter().enumerate() {
        if land != 2 {
            continue;
        }
        let side = input.dominant_side_map[index];
        if side < 0 {
            continue;
        }
        let x = index % input.grid_width;
        let is_front = (x + 1 < input.grid_width
            && input
                .hostility
                .is_hostile(side, input.dominant_side_map[index + 1]))
            || (x > 0
                && input
                    .hostility
                    .is_hostile(side, input.dominant_side_map[index - 1]))
            || (index + input.grid_width < total
                && input
                    .hostility
                    .is_hostile(side, input.dominant_side_map[index + input.grid_width]))
            || (index >= input.grid_width
                && input
                    .hostility
                    .is_hostile(side, input.dominant_side_map[index - input.grid_width]));
        if is_front {
            source_cell[index] = index;
            queue[queue_tail] = index;
            queue_tail += 1;
        }
    }

    while queue_head < queue_tail {
        let current = queue[queue_head];
        queue_head += 1;
        let source = source_cell[current];
        let current_y = current / input.grid_width;
        let current_x = current % input.grid_width;
        let source_y = source / input.grid_width;
        let source_x = source % input.grid_width;
        let delta_lat = (source_y as f64 - current_y as f64) * input.grid_res;
        let delta_lng = (source_x as f64 - current_x as f64) * input.grid_res;
        let magnitude = (delta_lat * delta_lat + delta_lng * delta_lng).sqrt();
        if magnitude > 0.0 {
            latitude[current] = (delta_lat / magnitude) as f32;
            longitude[current] = (delta_lng / magnitude) as f32;
        }

        // The explicit sequence is part of the parity contract.
        let right = if current % input.grid_width + 1 < input.grid_width {
            Some(current + 1)
        } else {
            None
        };
        let left = if current % input.grid_width > 0 {
            Some(current - 1)
        } else {
            None
        };
        let down = if current + input.grid_width < total {
            Some(current + input.grid_width)
        } else {
            None
        };
        let up = if current >= input.grid_width {
            Some(current - input.grid_width)
        } else {
            None
        };
        for neighbor in [right, left, down, up].into_iter().flatten() {
            if source_cell[neighbor] != usize::MAX || input.land_mask[neighbor] == 0 {
                continue;
            }
            source_cell[neighbor] = source;
            queue[queue_tail] = neighbor;
            queue_tail += 1;
        }
    }

    Ok(DirectionField {
        latitude,
        longitude,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(
        land: &[u8],
        sides: &[i16],
        width: usize,
        height: usize,
        relations: Option<&[u8]>,
        max_sides: usize,
    ) -> Result<DirectionField, DirectionFieldError> {
        build_direction_field(DirectionFieldInput {
            land_mask: land,
            dominant_side_map: sides,
            hostility: HostilityMatrix::new(relations, max_sides),
            grid_width: width,
            grid_height: height,
            grid_res: 0.25,
        })
    }

    #[test]
    fn straight_border_points_cells_toward_front() {
        let result = field(&[2; 6], &[0, 0, 0, 1, 1, 1], 6, 1, None, 2).unwrap();
        assert_eq!(result.longitude, vec![1.0, 1.0, 0.0, 0.0, -1.0, -1.0]);
        assert_eq!(result.latitude, vec![0.0; 6]);
    }

    #[test]
    fn horizontal_edges_do_not_wrap_between_rows() {
        let result = field(&[2; 4], &[0, 0, 1, 1], 2, 2, None, 2).unwrap();
        assert_eq!(result.latitude, vec![0.0; 4]);
        assert_eq!(result.longitude, vec![0.0; 4]);
    }

    #[test]
    fn asymmetric_hostility_is_respected_per_source_side() {
        let relations = [0, 1, 0, 0];
        let result = field(&[2; 4], &[0, 1, 1, 1], 4, 1, Some(&relations), 2).unwrap();
        assert_eq!(result.longitude, vec![0.0, -1.0, -1.0, -1.0]);
    }

    #[test]
    fn water_blocks_breadth_first_traversal() {
        let result = field(&[2, 2, 0, 2, 2], &[0, 1, -1, -1, -1], 5, 1, None, 2).unwrap();
        assert_eq!(result.longitude, vec![0.0; 5]);
    }

    #[test]
    fn mask_one_is_traversable_but_cannot_be_a_source() {
        let result = field(&[2, 2, 1, 1], &[0, 1, -1, -1], 4, 1, None, 2).unwrap();
        assert_eq!(result.longitude, vec![0.0, 0.0, -1.0, -1.0]);
    }

    #[test]
    fn equidistant_tie_uses_ascending_source_and_right_first_bfs() {
        let result = field(&[2, 2, 1, 2, 2], &[0, 1, -1, 1, 2], 5, 1, None, 3).unwrap();
        assert_eq!(result.longitude[2], -1.0);
    }

    #[test]
    fn rejects_invalid_array_lengths() {
        assert_eq!(
            field(&[2], &[0, 1], 2, 1, None, 2),
            Err(DirectionFieldError::LandMaskLength {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(
            field(&[2, 2], &[0], 2, 1, None, 2),
            Err(DirectionFieldError::DominantSideMapLength {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(
            field(&[2, 2], &[0, 1], 2, 1, Some(&[0, 1]), 2),
            Err(DirectionFieldError::HostilityLength {
                expected: 4,
                actual: 2,
            })
        );
    }
}
