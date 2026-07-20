// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use crate::{constants::*, terrain::Circle};

#[derive(Clone, Debug)]
pub struct SeededGenerator {
    state: u64,
}

impl SeededGenerator {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }
    fn gaussian(&mut self) -> f64 {
        let u = self.unit().max(f64::MIN_POSITIVE);
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * self.unit()).cos()
    }
    pub fn terrain(&mut self) -> Vec<Circle> {
        let count =
            (NUM_CIRCLES_MEAN + NUM_CIRCLES_STANDARD_DEVIATION * self.gaussian()).max(0.0) as usize;
        (0..count)
            .map(|_| Circle {
                x: self.unit() * PLANE_LENGTH as f64,
                y: self.unit() * PLANE_HEIGHT as f64,
                radius: (CIRCLE_MEAN_RADIUS + CIRCLE_STANDARD_DEVIATION * self.gaussian()).max(1.0),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_seeds_generate_equal_terrain() {
        assert_eq!(
            SeededGenerator::new(17).terrain(),
            SeededGenerator::new(17).terrain()
        );
    }
}
