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
            || (self.circles.iter().any(|circle| circle.contains(x, y))
                && !self
                    .explosions
                    .iter()
                    .any(|explosion| explosion.contains(x, y)))
    }
    pub fn collides_circle(&self, x: f64, y: f64, radius: f64) -> bool {
        let subject = Circle { x, y, radius };
        x - radius < 0.0
            || x + radius >= PLANE_LENGTH as f64
            || y - radius < 0.0
            || y + radius >= PLANE_HEIGHT as f64
            || self.circles.iter().any(|circle| {
                circles_overlap(subject, *circle)
                    && !self
                        .explosions
                        .iter()
                        .any(|cut| circle_contains_circle(*cut, subject))
            })
    }
    pub fn explode(&mut self, x: f64, y: f64, radius: f64) {
        self.explosions.push(Circle { x, y, radius });
    }
    pub fn segment_collision_point(&self, from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
        if !from.0.is_finite() || !from.1.is_finite() || !to.0.is_finite() || !to.1.is_finite() {
            return Some(from);
        }
        if self.collides_point(from.0, from.1) {
            return Some(from);
        }
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        // ponytail: logical-pixel sampling bounds terrain impact precision; replace with boolean circle subtraction if subpixel terrain is added.
        let samples = (dx.hypot(dy).ceil() as usize).max(1);
        let mut low = 0.0;
        for index in 1..=samples {
            let mut high = index as f64 / samples as f64;
            let point = (from.0 + dx * high, from.1 + dy * high);
            if !self.collides_point(point.0, point.1) {
                low = high;
                continue;
            }
            for _ in 0..20 {
                let middle = (low + high) / 2.0;
                let point = (from.0 + dx * middle, from.1 + dy * middle);
                if self.collides_point(point.0, point.1) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            return Some((from.0 + dx * high, from.1 + dy * high));
        }
        None
    }
    pub fn segment_collides(&self, from: (f64, f64), to: (f64, f64)) -> bool {
        self.segment_collision_point(from, to).is_some()
    }
}

fn circles_overlap(left: Circle, right: Circle) -> bool {
    (left.x - right.x).hypot(left.y - right.y) <= left.radius + right.radius
}

fn circle_contains_circle(outer: Circle, inner: Circle) -> bool {
    (outer.x - inner.x).hypot(outer.y - inner.y) + inner.radius <= outer.radius
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
    fn explosions_cut_existing_terrain() {
        let mut terrain = Terrain::new(vec![Circle {
            x: 10.0,
            y: 10.0,
            radius: 8.0,
        }]);
        assert!(terrain.collides_point(10.0, 10.0));
        terrain.explode(10.0, 10.0, 3.0);
        assert!(!terrain.collides_point(10.0, 10.0));
        assert!(terrain.collides_point(17.0, 10.0));
    }

    #[test]
    fn circle_collision_detects_diagonal_and_tangent_overlap() {
        let terrain = Terrain::new(vec![Circle {
            x: 20.0,
            y: 20.0,
            radius: 5.0,
        }]);
        assert!(terrain.collides_circle(24.0, 24.0, 2.0));
        assert!(terrain.collides_circle(27.0, 20.0, 2.0));
        assert!(!terrain.collides_circle(27.01, 20.0, 2.0));
        assert!(!terrain.collides_circle(30.0, 30.0, 2.0));
    }

    #[test]
    fn segment_collision_returns_first_point() {
        let terrain = Terrain::new(vec![Circle {
            x: 20.0,
            y: 20.0,
            radius: 2.0,
        }]);
        let point = terrain
            .segment_collision_point((0.0, 20.0), (40.0, 20.0))
            .unwrap();
        assert!((point.0 - 18.0).abs() < 0.01);
        assert!((point.1 - 20.0).abs() < 0.01);
    }

    #[test]
    fn explosion_cut_allows_fully_cleared_circle_overlap() {
        let mut terrain = Terrain::new(vec![Circle {
            x: 20.0,
            y: 20.0,
            radius: 5.0,
        }]);
        terrain.explode(20.0, 20.0, 10.0);
        assert!(!terrain.collides_circle(20.0, 20.0, 2.0));
    }
}
