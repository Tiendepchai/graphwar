use graphwar_game_core::{
    Circle, GameState, Player, Soldier, Team, Terrain, TrajectoryMode, parse, trace,
};
use graphwar_protocol::GameMode;

use crate::state::Model;

pub fn trace_preview(
    model: &Model,
    function: &str,
    angle_deg: f64,
) -> Result<Vec<(f64, f64)>, &'static str> {
    let active = model
        .soldiers
        .iter()
        .position(|soldier| soldier.active && soldier.alive)
        .ok_or("No active soldier")?;
    let expression = parse(function).map_err(|_| "Invalid function")?;
    let mut order = Vec::with_capacity(model.soldiers.len());
    order.push(active);
    order.extend((0..model.soldiers.len()).filter(|index| *index != active));
    let players = order
        .into_iter()
        .enumerate()
        .map(|(id, index)| {
            let view = &model.soldiers[index];
            let mut soldier = Soldier::new(view.x, view.y);
            soldier.alive = view.alive;
            Player::new(id as u32, team(view.team), vec![soldier])
        })
        .collect();
    let game = GameState::new(players);
    let terrain = Terrain {
        circles: model
            .terrain
            .iter()
            .filter(|circle| !circle.cut)
            .map(circle)
            .collect(),
        explosions: model
            .terrain
            .iter()
            .filter(|circle| circle.cut)
            .map(circle)
            .collect(),
    };
    let mode = match model.game_mode.unwrap_or(GameMode::Function) {
        GameMode::Function => TrajectoryMode::Function,
        GameMode::FirstOrder => TrajectoryMode::FirstOrder,
        GameMode::SecondOrder => TrajectoryMode::SecondOrder {
            angle: angle_deg.clamp(-90.0, 90.0).to_radians(),
        },
    };
    trace(
        &expression,
        mode,
        &terrain,
        &game,
        team(model.soldiers[active].team) == Team::Two,
    )
    .map(|trajectory| trajectory.points)
    .map_err(|_| "Function has no finite trajectory")
}

fn circle(view: &crate::state::TerrainView) -> Circle {
    Circle {
        x: view.x,
        y: view.y,
        radius: view.radius,
    }
}

fn team(value: u8) -> Team {
    if value == 1 { Team::One } else { Team::Two }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SoldierView;

    #[test]
    fn preview_uses_active_soldier_and_rejects_invalid_functions() {
        let mut model = Model {
            game_mode: Some(GameMode::Function),
            soldiers: vec![
                SoldierView {
                    x: 100.0,
                    y: 225.0,
                    team: 1,
                    alive: true,
                    active: true,
                },
                SoldierView {
                    x: 650.0,
                    y: 225.0,
                    team: 2,
                    alive: true,
                    active: false,
                },
            ],
            ..Model::default()
        };
        assert!(trace_preview(&model, "0", 0.0).unwrap().len() > 1);
        assert_eq!(
            trace_preview(&model, "garbage", 0.0),
            Err("Invalid function")
        );

        model.soldiers[0].active = false;
        assert_eq!(trace_preview(&model, "0", 0.0), Err("No active soldier"));
    }
}
