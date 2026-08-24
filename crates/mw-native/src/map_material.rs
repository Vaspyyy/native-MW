//! Immutable geographic material used exclusively by the native map renderer.
//!
//! This deliberately stays outside runtime publications and checkpoints: it is
//! reconstructed from the scenario baseline when the viewer starts.

use std::collections::BTreeMap;

use mw_core::DecodedScenario;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapMaterial {
    pub sovereign: Vec<u16>,
    pub land: Vec<u8>,
    pub biome: Vec<u8>,
    /// Inclusive target-grid Y extent per sovereign ID, suitable for the
    /// browser WarGames vertical country gradient.
    pub country_y_bounds: Vec<[u32; 2]>,
    pub sovereign_sides: Vec<i32>,
}

impl MapMaterial {
    pub fn from_scenario(scenario: &DecodedScenario) -> Self {
        let max_id = scenario.world_control.iter().copied().max().unwrap_or(0) as usize;
        let mut country_y_bounds = vec![[0, 0]; max_id + 1];
        let mut seen = vec![false; max_id + 1];
        for (cell, &country_id) in scenario.world_control.iter().enumerate() {
            let country_id = country_id as usize;
            if country_id == 0 {
                continue;
            }
            let y = (cell / scenario.target.width) as u32;
            if !seen[country_id] {
                country_y_bounds[country_id] = [y, y];
                seen[country_id] = true;
            } else {
                country_y_bounds[country_id][0] = country_y_bounds[country_id][0].min(y);
                country_y_bounds[country_id][1] = country_y_bounds[country_id][1].max(y);
            }
        }
        Self {
            sovereign: scenario.world_control.clone(),
            land: scenario.land.clone(),
            biome: scenario.biome.clone(),
            country_y_bounds,
            sovereign_sides: vec![-1; max_id + 1],
        }
    }

    pub fn set_sovereign_sides(&mut self, country_to_side: &BTreeMap<u16, usize>) {
        for (&country_id, &side) in country_to_side {
            if let Some(slot) = self.sovereign_sides.get_mut(country_id as usize) {
                *slot = i32::try_from(side).unwrap_or(-1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::MapMaterial;
    use mw_core::{DecodedScenario, GridSpec};

    #[test]
    fn material_keeps_biomes_exact_and_derives_sovereign_bounds() {
        let grid = GridSpec::world(90.0).unwrap();
        let scenario = DecodedScenario {
            metadata: serde_json::Value::Null,
            source: grid,
            target: grid,
            entry_count: 3,
            world_control: vec![0, 2, 2, 1, 0, 2, 0, 0],
            de_jure: vec![0; 8],
            land: vec![0, 1, 1, 1, 0, 1, 0, 0],
            biome: vec![0, 1, 0, 1, 0, 0, 0, 0],
            province: vec![0; 8],
        };
        let material = MapMaterial::from_scenario(&scenario);
        assert_eq!(material.biome, scenario.biome);
        assert_eq!(material.country_y_bounds[1], [0, 0]);
        assert_eq!(material.country_y_bounds[2], [0, 1]);
    }

    #[test]
    fn only_declared_sovereigns_receive_a_static_side() {
        let mut material = MapMaterial {
            sovereign: vec![],
            land: vec![],
            biome: vec![],
            country_y_bounds: vec![],
            sovereign_sides: vec![-1; 4],
        };
        material.set_sovereign_sides(&BTreeMap::from([(1, 0), (3, 2)]));
        assert_eq!(material.sovereign_sides, [-1, 0, -1, 2]);
    }
}
