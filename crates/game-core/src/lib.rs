// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar.
// Graphwar is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// Graphwar is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

pub mod constants;
pub mod expression;
pub mod generation;
pub mod model;
pub mod terrain;
pub mod trajectory;

pub use expression::{Ast, EvalVars, Expr, ParseError, parse};
pub use generation::SeededGenerator;
pub use model::{GameState, Player, Soldier, Team};
pub use terrain::{Circle, Terrain};
pub use trajectory::{Hit, Trajectory, TrajectoryError, TrajectoryMode, trace};
