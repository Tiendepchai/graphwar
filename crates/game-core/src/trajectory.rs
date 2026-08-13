// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use crate::{
    constants::*,
    expression::{EvalVars, Expr},
    model::GameState,
    terrain::Terrain,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrajectoryMode {
    Function,
    FirstOrder,
    SecondOrder { angle: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryMissReason {
    WorldExit,
    Numerical,
    StepLimit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrajectoryEnd {
    TerrainImpact { point: (f64, f64) },
    Miss(TrajectoryMissReason),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trajectory {
    pub points: Vec<(f64, f64)>,
    pub end: TrajectoryEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryError {
    InvalidState,
}

pub fn projectile_hits(points: &[(f64, f64)], game: &GameState) -> Vec<(usize, usize)> {
    let shooter = game
        .players
        .get(game.turn)
        .map(|player| (game.turn, player.current_soldier));
    let mut hits = Vec::new();
    for (player_index, player) in game.players.iter().enumerate() {
        for (soldier_index, soldier) in player.living() {
            if !finite_point((soldier.x, soldier.y)) {
                continue;
            }
            if Some((player_index, soldier_index)) != shooter
                && points.windows(2).any(|segment| {
                    segment_hits_circle(
                        segment[0],
                        segment[1],
                        (soldier.x, soldier.y),
                        SOLDIER_RADIUS,
                    )
                })
            {
                hits.push((player_index, soldier_index));
            }
        }
    }
    hits
}

pub fn trace(
    expr: &Expr,
    mode: TrajectoryMode,
    terrain: &Terrain,
    game: &GameState,
    inverted: bool,
) -> Result<Trajectory, TrajectoryError> {
    let Some(shooter) = game.players.get(game.turn) else {
        return Err(TrajectoryError::InvalidState);
    };
    let Some(soldier) = shooter.current() else {
        return Err(TrajectoryError::InvalidState);
    };
    if !soldier.x.is_finite() || !soldier.y.is_finite() || !inside_world((soldier.x, soldier.y)) {
        return Err(TrajectoryError::InvalidState);
    }

    let mut state = State::from_screen(soldier.x, soldier.y, inverted);
    let start = state.screen(inverted);
    let numerical = || Trajectory {
        points: vec![start],
        end: TrajectoryEnd::Miss(TrajectoryMissReason::Numerical),
    };
    let angle = match mode {
        TrajectoryMode::Function => function_angle(expr, state.x),
        TrajectoryMode::FirstOrder => first_angle(expr, state.x, state.y),
        TrajectoryMode::SecondOrder { angle } => angle,
    };
    if !angle.is_finite() {
        return Ok(numerical());
    }
    let radius = PLANE_GAME_LENGTH * SOLDIER_RADIUS / PLANE_LENGTH as f64;
    state.x += radius * angle.cos();
    state.y += radius * angle.sin();
    state.dy = angle.tan();
    if !state.finite() {
        return Ok(numerical());
    }
    let offset = match mode {
        TrajectoryMode::Function => {
            state.y
                - expr.evaluate(EvalVars {
                    x: state.x,
                    y: 0.0,
                    dy: 0.0,
                })
        }
        _ => 0.0,
    };
    if !offset.is_finite() {
        return Ok(numerical());
    }

    let launch = state.screen(inverted);
    if !inside_world(launch) {
        let edge = world_exit_point(start, launch).unwrap_or(start);
        return Ok(Trajectory {
            points: distinct_points(start, edge),
            end: TrajectoryEnd::Miss(TrajectoryMissReason::WorldExit),
        });
    }
    let mut points = distinct_points(start, launch);
    let mut previous = state;
    for _ in 1..FUNC_MAX_STEPS {
        let Some(next) = adaptive_step(expr, mode, previous, offset) else {
            return Ok(Trajectory {
                points,
                end: TrajectoryEnd::Miss(TrajectoryMissReason::Numerical),
            });
        };
        let from = previous.screen(inverted);
        let to = next.screen(inverted);
        if !finite_point(to) {
            return Ok(Trajectory {
                points,
                end: TrajectoryEnd::Miss(TrajectoryMissReason::Numerical),
            });
        }

        let edge = world_exit_point(from, to);
        let segment_end = edge.unwrap_or(to);
        if let Some(point) = terrain.segment_collision_point(from, segment_end) {
            push_distinct(&mut points, point);
            return Ok(Trajectory {
                points,
                end: TrajectoryEnd::TerrainImpact { point },
            });
        }
        push_distinct(&mut points, segment_end);
        if edge.is_some() {
            return Ok(Trajectory {
                points,
                end: TrajectoryEnd::Miss(TrajectoryMissReason::WorldExit),
            });
        }
        previous = next;
    }
    Ok(Trajectory {
        points,
        end: TrajectoryEnd::Miss(TrajectoryMissReason::StepLimit),
    })
}

fn finite_point(point: (f64, f64)) -> bool {
    point.0.is_finite() && point.1.is_finite()
}

fn inside_world(point: (f64, f64)) -> bool {
    (0.0..=PLANE_LENGTH as f64).contains(&point.0) && (0.0..=PLANE_HEIGHT as f64).contains(&point.1)
}

fn world_exit_point(from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
    if !inside_world(from) || inside_world(to) {
        return None;
    }
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let mut t: f64 = 1.0;
    if dx < 0.0 {
        t = t.min((0.0 - from.0) / dx);
    } else if dx > 0.0 {
        t = t.min((PLANE_LENGTH as f64 - from.0) / dx);
    }
    if dy < 0.0 {
        t = t.min((0.0 - from.1) / dy);
    } else if dy > 0.0 {
        t = t.min((PLANE_HEIGHT as f64 - from.1) / dy);
    }
    let point = (from.0 + dx * t, from.1 + dy * t);
    finite_point(point).then_some(point)
}

fn segment_hits_circle(from: (f64, f64), to: (f64, f64), center: (f64, f64), radius: f64) -> bool {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let length_squared = dx.mul_add(dx, dy * dy);
    if length_squared == 0.0 {
        return (from.0 - center.0).hypot(from.1 - center.1) <= radius;
    }
    let t =
        (((center.0 - from.0) * dx + (center.1 - from.1) * dy) / length_squared).clamp(0.0, 1.0);
    let closest = (from.0 + t * dx, from.1 + t * dy);
    (closest.0 - center.0).hypot(closest.1 - center.1) <= radius
}

fn distinct_points(first: (f64, f64), second: (f64, f64)) -> Vec<(f64, f64)> {
    let mut points = vec![first];
    push_distinct(&mut points, second);
    points
}

fn push_distinct(points: &mut Vec<(f64, f64)>, point: (f64, f64)) {
    if points.last().copied() != Some(point) {
        points.push(point);
    }
}

#[derive(Clone, Copy)]
struct State {
    x: f64,
    y: f64,
    dy: f64,
}

impl State {
    fn from_screen(mut x: f64, y: f64, inverted: bool) -> Self {
        if inverted {
            x = PLANE_LENGTH as f64 - x;
        }
        Self {
            x: PLANE_GAME_LENGTH * (x - PLANE_LENGTH as f64 / 2.0) / PLANE_LENGTH as f64,
            y: PLANE_GAME_LENGTH * (-y + PLANE_HEIGHT as f64 / 2.0) / PLANE_LENGTH as f64,
            dy: 0.0,
        }
    }

    fn screen(self, inverted: bool) -> (f64, f64) {
        let mut x = PLANE_LENGTH as f64 * self.x / PLANE_GAME_LENGTH + PLANE_LENGTH as f64 / 2.0;
        if inverted {
            x = PLANE_LENGTH as f64 - x;
        }
        (
            x,
            -PLANE_LENGTH as f64 * self.y / PLANE_GAME_LENGTH + PLANE_HEIGHT as f64 / 2.0,
        )
    }

    fn finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.dy.is_finite()
    }
}

fn adaptive_step(expr: &Expr, mode: TrajectoryMode, previous: State, offset: f64) -> Option<State> {
    let mut step = STEP_SIZE;
    loop {
        let next = integrate(expr, mode, previous, step, offset)?;
        let distance = (next.x - previous.x).mul_add(
            next.x - previous.x,
            (next.y - previous.y) * (next.y - previous.y),
        );
        if distance <= FUNC_MAX_STEP_DISTANCE_SQUARED {
            return Some(next);
        }
        step *= 0.5;
        if step < FUNC_MIN_X_STEP_DISTANCE {
            return None;
        }
    }
}

fn integrate(
    expr: &Expr,
    mode: TrajectoryMode,
    state: State,
    step: f64,
    offset: f64,
) -> Option<State> {
    let next = match mode {
        TrajectoryMode::Function => State {
            x: state.x + step,
            y: expr.evaluate(EvalVars {
                x: state.x + step,
                y: 0.0,
                dy: 0.0,
            }) + offset,
            dy: 0.0,
        },
        TrajectoryMode::FirstOrder => {
            let f = |x, y| expr.evaluate(EvalVars { x, y, dy: 0.0 });
            let k1 = f(state.x, state.y);
            let k2 = f(state.x + step / 2.0, state.y + step * k1 / 2.0);
            let k3 = f(state.x + step / 2.0, state.y + step * k2 / 2.0);
            let k4 = f(state.x + step, state.y + step * k3);
            State {
                x: state.x + step,
                y: state.y + step * (k1 + 2.0 * k2 + 2.0 * k3 + k4) / 6.0,
                dy: 0.0,
            }
        }
        TrajectoryMode::SecondOrder { .. } => {
            let f = |x, y, dy| expr.evaluate(EvalVars { x, y, dy });
            let k11 = state.dy;
            let k12 = f(state.x, state.y, state.dy);
            let k21 = state.dy + step * k12 / 2.0;
            let k22 = f(
                state.x + step / 2.0,
                state.y + step * k11 / 2.0,
                state.dy + step * k12 / 2.0,
            );
            let k31 = state.dy + step * k22 / 2.0;
            let k32 = f(
                state.x + step / 2.0,
                state.y + step * k21 / 2.0,
                state.dy + step * k22 / 2.0,
            );
            let k41 = state.dy + step * k32;
            let k42 = f(state.x + step, state.y + step * k31, state.dy + step * k32);
            State {
                x: state.x + step,
                y: state.y + step * (k11 + 2.0 * k21 + 2.0 * k31 + k41) / 6.0,
                dy: state.dy + step * (k12 + 2.0 * k22 + 2.0 * k32 + k42) / 6.0,
            }
        }
    };
    next.finite().then_some(next)
}

fn function_angle(expr: &Expr, x: f64) -> f64 {
    let f = |x| expr.evaluate(EvalVars { x, y: 0.0, dy: 0.0 });
    converge_angle(|angle| {
        let final_x = x + PLANE_GAME_LENGTH * SOLDIER_RADIUS / PLANE_LENGTH as f64 * angle.cos();
        ((f(final_x + STEP_SIZE) - f(final_x)) / STEP_SIZE).atan()
    })
}

fn first_angle(expr: &Expr, x: f64, y: f64) -> f64 {
    converge_angle(|angle| {
        let radius = PLANE_GAME_LENGTH * SOLDIER_RADIUS / PLANE_LENGTH as f64;
        expr.evaluate(EvalVars {
            x: x + radius * angle.cos(),
            y: y + radius * angle.sin(),
            dy: 0.0,
        })
        .atan()
    })
}

fn converge_angle(mut update: impl FnMut(f64) -> f64) -> f64 {
    let mut angle: f64 = 0.0;
    for _ in 0..MAX_ANGLE_LOOPS {
        let next = update(angle);
        if !next.is_finite() {
            return f64::NAN;
        }
        if (next - angle).abs() <= ANGLE_ERROR {
            return next;
        }
        angle = next;
    }
    f64::NAN
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Player, Soldier, Team},
        parse,
        terrain::Circle,
    };

    fn game() -> GameState {
        GameState::new(vec![Player::new(
            1,
            Team::One,
            vec![Soldier::new(100.0, 225.0)],
        )])
    }

    fn game_with_target(x: f64, y: f64) -> GameState {
        GameState::new(vec![
            Player::new(1, Team::One, vec![Soldier::new(100.0, 225.0)]),
            Player::new(2, Team::Two, vec![Soldier::new(x, y)]),
        ])
    }

    #[test]
    fn nan_is_a_bounded_miss() {
        let path = trace(
            &parse("sqrt(-1)").unwrap(),
            TrajectoryMode::Function,
            &Terrain::default(),
            &game(),
            false,
        )
        .unwrap();
        assert_eq!(
            path.end,
            TrajectoryEnd::Miss(TrajectoryMissReason::Numerical)
        );
        assert!(path.points.iter().all(|point| finite_point(*point)));
    }

    #[test]
    fn adaptive_step_enforces_distance() {
        let path = trace(
            &parse("1000x").unwrap(),
            TrajectoryMode::Function,
            &Terrain::default(),
            &game(),
            false,
        )
        .unwrap();
        for pair in path.points.windows(2) {
            assert!(
                (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1)
                    <= PLANE_LENGTH as f64 / PLANE_GAME_LENGTH
                        * FUNC_MAX_STEP_DISTANCE_SQUARED.sqrt()
                        + SOLDIER_RADIUS
                        + 1e-9
            );
        }
    }

    #[test]
    fn all_modes_exit_world_with_finite_points() {
        for mode in [
            TrajectoryMode::Function,
            TrajectoryMode::FirstOrder,
            TrajectoryMode::SecondOrder { angle: 0.0 },
        ] {
            let path = trace(
                &parse("0").unwrap(),
                mode,
                &Terrain::default(),
                &game(),
                false,
            )
            .unwrap();
            assert_eq!(
                path.end,
                TrajectoryEnd::Miss(TrajectoryMissReason::WorldExit)
            );
            assert!(path.points.len() > 1 && path.points.iter().all(|point| finite_point(*point)));
            assert!((path.points.last().unwrap().0 - PLANE_LENGTH as f64).abs() < 1e-9);
        }
    }

    #[test]
    fn terrain_clips_path_at_first_material() {
        let terrain = Terrain::new(vec![Circle {
            x: 200.0,
            y: 225.0,
            radius: 10.0,
        }]);
        let path = trace(
            &parse("0").unwrap(),
            TrajectoryMode::Function,
            &terrain,
            &game_with_target(180.0, 225.0),
            false,
        )
        .unwrap();
        let endpoint = *path.points.last().unwrap();
        assert!((endpoint.0 - 190.0).abs() < 0.01);
        assert_eq!(path.end, TrajectoryEnd::TerrainImpact { point: endpoint });
    }

    #[test]
    fn soldier_contact_does_not_end_path_but_reports_projectile_hit() {
        let game = game_with_target(180.0, 225.0);
        let path = trace(
            &parse("0").unwrap(),
            TrajectoryMode::Function,
            &Terrain::default(),
            &game,
            false,
        )
        .unwrap();
        assert_eq!(
            path.end,
            TrajectoryEnd::Miss(TrajectoryMissReason::WorldExit)
        );
        assert_eq!(path.points.last().unwrap().0, PLANE_LENGTH as f64);
        assert_eq!(projectile_hits(&path.points, &game), vec![(1, 0)]);
    }

    #[test]
    fn terrain_before_edge_wins() {
        let terrain = Terrain::new(vec![Circle {
            x: 765.0,
            y: 225.0,
            radius: 2.0,
        }]);
        let path = trace(
            &parse("0").unwrap(),
            TrajectoryMode::Function,
            &terrain,
            &game(),
            false,
        )
        .unwrap();
        assert!(matches!(path.end, TrajectoryEnd::TerrainImpact { .. }));
        assert!(path.points.last().unwrap().0 < PLANE_LENGTH as f64);
    }

    #[test]
    fn angle_non_convergence_is_numerical_miss() {
        assert!(converge_angle(|angle| if angle == 0.0 { 1.0 } else { 0.0 }).is_nan());
    }
}
