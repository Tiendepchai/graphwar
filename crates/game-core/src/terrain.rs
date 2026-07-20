// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use crate::constants::{PLANE_HEIGHT, PLANE_LENGTH};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
}

impl Circle {
    pub fn contains(self, x: f64, y: f64) -> bool {
        (x - self.x).mul_add(x - self.x, (y - self.y) * (y - self.y)) <= self.radius * self.radius
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Terrain {
    pub circles: Vec<Circle>,
    pub explosions: Vec<Circle>,
}

impl Terrain {
    pub fn new(circles: Vec<Circle>) -> Self {
        Self {
            circles,
            explosions: Vec::new(),
        }
    }
    pub fn collides_point(&self, x: f64, y: f64) -> bool {
        !(0.0..PLANE_LENGTH as f64).contains(&x)
            || !(0.0..PLANE_HEIGHT as f64).contains(&y)
            || self
                .circles
                .iter()
                .chain(&self.explosions)
                .any(|circle| circle.contains(x, y))
    }
    pub fn collides_circle(&self, x: f64, y: f64, radius: f64) -> bool {
        self.collides_point(x - radius, y)
            || self.collides_point(x + radius, y)
            || self.collides_point(x, y - radius)
            || self.collides_point(x, y + radius)
    }
    pub fn explode(&mut self, x: f64, y: f64, radius: f64) {
        self.explosions.push(Circle { x, y, radius });
    }
    pub fn segment_collides(&self, from: (f64, f64), to: (f64, f64)) -> bool {
        if !from.0.is_finite() || !from.1.is_finite() || !to.0.is_finite() || !to.1.is_finite() {
            return true;
        }
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let length = dx.hypot(dy);
        let samples = (length.ceil() as usize).max(1);
        (0..=samples).any(|index| {
            let t = index as f64 / samples as f64;
            self.collides_point(from.0 + dx * t, from.1 + dy * t)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segment_collision_prevents_tunneling() {
        let terrain = Terrain::new(vec![Circle {
            x: 20.0,
            y: 20.0,
            radius: 2.0,
        }]);
        assert!(terrain.segment_collides((0.0, 20.0), (40.0, 20.0)));
    }
    #[test]
    fn explosions_are_terrain() {
        let mut terrain = Terrain::default();
        terrain.explode(10.0, 10.0, 3.0);
        assert!(terrain.collides_point(10.0, 10.0));
    }
}
