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
pub struct Hit {
    pub player: usize,
    pub soldier: usize,
    pub step: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trajectory {
    pub points: Vec<(f64, f64)>,
    pub hits: Vec<Hit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryError {
    InvalidState,
    NonFinite,
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
    let mut state = State::from_screen(soldier.x, soldier.y, inverted);
    let angle = match mode {
        TrajectoryMode::Function => function_angle(expr, state.x),
        TrajectoryMode::FirstOrder => first_angle(expr, state.x, state.y),
        TrajectoryMode::SecondOrder { angle } => angle,
    };
    if !angle.is_finite() {
        return Err(TrajectoryError::NonFinite);
    }
    let radius = PLANE_GAME_LENGTH * SOLDIER_RADIUS / PLANE_LENGTH as f64;
    state.x += radius * angle.cos();
    state.y += radius * angle.sin();
    state.dy = angle.tan();
    if !state.finite() {
        return Err(TrajectoryError::NonFinite);
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
        return Err(TrajectoryError::NonFinite);
    }
    let mut result = Trajectory {
        points: vec![state.screen(inverted)],
        hits: Vec::new(),
    };
    let mut previous = state;
    for step in 1..FUNC_MAX_STEPS {
        let Some(next) = adaptive_step(expr, mode, previous, offset) else {
            break;
        };
        let from = previous.screen(inverted);
        let to = next.screen(inverted);
        if !to.0.is_finite() || !to.1.is_finite() {
            break;
        }
        if let Some(collision) = terrain.segment_collision_point(from, to) {
            result.points.push(collision);
            collect_hits(&mut result.hits, from, collision, game, step);
            break;
        }
        result.points.push(to);
        collect_hits(&mut result.hits, from, to, game, step);
        previous = next;
    }
    (result.points.len() > 1)
        .then_some(result)
        .ok_or(TrajectoryError::NonFinite)
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

fn collect_hits(
    hits: &mut Vec<Hit>,
    from: (f64, f64),
    to: (f64, f64),
    game: &GameState,
    step: usize,
) {
    for (player_index, player) in game.players.iter().enumerate() {
        for (soldier_index, soldier) in player.living() {
            if player_index == game.turn && soldier_index == player.current_soldier {
                continue;
            }
            if distance_to_segment((soldier.x, soldier.y), from, to) <= SOLDIER_RADIUS
                && !hits
                    .iter()
                    .any(|hit| hit.player == player_index && hit.soldier == soldier_index)
            {
                hits.push(Hit {
                    player: player_index,
                    soldier: soldier_index,
                    step,
                });
            }
        }
    }
}
fn distance_to_segment(point: (f64, f64), from: (f64, f64), to: (f64, f64)) -> f64 {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let length = dx * dx + dy * dy;
    if length == 0.0 {
        return (point.0 - from.0).hypot(point.1 - from.1);
    }
    let t = (((point.0 - from.0) * dx + (point.1 - from.1) * dy) / length).clamp(0.0, 1.0);
    (point.0 - (from.0 + t * dx)).hypot(point.1 - (from.1 + t * dy))
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
    fn nan_stops_without_poisoning_points() {
        assert_eq!(
            trace(
                &parse("sqrt(-1)").unwrap(),
                TrajectoryMode::Function,
                &Terrain::default(),
                &game(),
                false,
            ),
            Err(TrajectoryError::NonFinite)
        );
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
                        + 1e-9
            );
        }
    }
    #[test]
    fn all_modes_produce_finite_points() {
        for mode in [
            TrajectoryMode::Function,
            TrajectoryMode::FirstOrder,
            TrajectoryMode::SecondOrder { angle: 0.0 },
        ] {
            let p = trace(
                &parse("0").unwrap(),
                mode,
                &Terrain::default(),
                &game(),
                false,
            )
            .unwrap();
            assert!(
                p.points.len() > 1
                    && p.points
                        .iter()
                        .all(|point| point.0.is_finite() && point.1.is_finite())
            );
        }
    }

    #[test]
    fn terrain_clips_path_and_blocks_target_behind_it() {
        let terrain = Terrain::new(vec![Circle {
            x: 200.0,
            y: 225.0,
            radius: 10.0,
        }]);
        let path = trace(
            &parse("0").unwrap(),
            TrajectoryMode::Function,
            &terrain,
            &game_with_target(250.0, 225.0),
            false,
        )
        .unwrap();
        let endpoint = path.points.last().unwrap();
        assert!((endpoint.0 - 190.0).abs() < 0.01);
        assert!(path.hits.is_empty());
    }

    #[test]
    fn target_before_terrain_is_hit() {
        let terrain = Terrain::new(vec![Circle {
            x: 250.0,
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
        assert!(
            path.hits
                .iter()
                .any(|hit| hit.player == 1 && hit.soldier == 0)
        );
    }

    #[test]
    fn tangent_target_is_hit() {
        let path = trace(
            &parse("0").unwrap(),
            TrajectoryMode::Function,
            &Terrain::default(),
            &game_with_target(180.0, 225.0 + SOLDIER_RADIUS),
            false,
        )
        .unwrap();
        assert!(
            path.hits
                .iter()
                .any(|hit| hit.player == 1 && hit.soldier == 0)
        );
    }

    #[test]
    fn angle_non_convergence_fails() {
        assert!(converge_angle(|angle| if angle == 0.0 { 1.0 } else { 0.0 }).is_nan());
    }
}
