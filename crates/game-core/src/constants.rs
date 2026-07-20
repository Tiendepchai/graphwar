// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

pub const PLANE_LENGTH: i32 = 770;
pub const PLANE_HEIGHT: i32 = 450;
pub const PLANE_GAME_LENGTH: f64 = 50.0;
pub const CIRCLE_MEAN_RADIUS: f64 = 40.0;
pub const CIRCLE_STANDARD_DEVIATION: f64 = 25.0;
pub const NUM_CIRCLES_MEAN: f64 = 15.0;
pub const NUM_CIRCLES_STANDARD_DEVIATION: f64 = 7.0;
pub const SOLDIER_RADIUS: f64 = 7.0;
pub const EXPLOSION_RADIUS: f64 = 12.0;
pub const FUNC_MAX_STEPS: usize = 20_000;
pub const FUNC_MAX_STEP_DISTANCE_SQUARED: f64 = 0.001;
pub const FUNC_MIN_X_STEP_DISTANCE: f64 = 0.000_01;
pub const STEP_SIZE: f64 = 0.01;
pub const ANGLE_ERROR: f64 = std::f64::consts::PI / 360.0;
pub const MAX_ANGLE_LOOPS: usize = 100;
pub const MAX_PLAYERS: usize = 10;
pub const MAX_SOLDIERS_PER_PLAYER: usize = 4;
pub const INITIAL_NUM_SOLDIERS: usize = 2;
