// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use crate::constants::{INITIAL_NUM_SOLDIERS, MAX_SOLDIERS_PER_PLAYER};
use serde::{Deserialize, Deserializer, Serialize, de};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Team {
    One,
    Two,
}

impl<'de> Deserialize<'de> for Team {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TeamVisitor;
        impl de::Visitor<'_> for TeamVisitor {
            type Value = Team;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a team as u8 or the strings \"One\"/\"Two\"")
            }
            fn visit_u8<E: de::Error>(self, value: u8) -> Result<Self::Value, E> {
                Ok(Team::from_u8(value))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Team::from_u8(value as u8))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "One" => Ok(Team::One),
                    "Two" => Ok(Team::Two),
                    // Accept a numeric string too.
                    _ => value
                        .parse::<u8>()
                        .map(Team::from_u8)
                        .map_err(|_| de::Error::custom("unknown team variant")),
                }
            }
        }
        deserializer.deserialize_any(TeamVisitor)
    }
}

impl Team {
    fn from_u8(value: u8) -> Self {
        if value == 1 { Team::One } else { Team::Two }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Soldier {
    pub x: f64,
    pub y: f64,
    pub alive: bool,
}

impl Soldier {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y, alive: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Player {
    pub id: u32,
    pub team: Team,
    pub soldiers: Vec<Soldier>,
    pub current_soldier: usize,
}

impl Player {
    pub fn new(id: u32, team: Team, soldiers: Vec<Soldier>) -> Self {
        assert!(!soldiers.is_empty() && soldiers.len() <= MAX_SOLDIERS_PER_PLAYER);
        Self {
            id,
            team,
            soldiers,
            current_soldier: 0,
        }
    }
    pub fn current(&self) -> Option<&Soldier> {
        self.soldiers.get(self.current_soldier)
    }
    pub fn current_mut(&mut self) -> Option<&mut Soldier> {
        self.soldiers.get_mut(self.current_soldier)
    }
    pub fn living(&self) -> impl Iterator<Item = (usize, &Soldier)> {
        self.soldiers
            .iter()
            .enumerate()
            .filter(|(_, soldier)| soldier.alive)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameState {
    pub players: Vec<Player>,
    pub turn: usize,
    /// Per-team cursor into `players` so turns rotate through every living
    /// teammate instead of always landing on the same player of a team.
    #[serde(default)]
    pub team_turn: [usize; 2],
}

impl GameState {
    pub fn new(players: Vec<Player>) -> Self {
        Self {
            players,
            turn: 0,
            team_turn: [0, 0],
        }
    }
    pub fn starter(id: u32, team: Team, x: f64, y: f64) -> Player {
        Player::new(
            id,
            team,
            (0..INITIAL_NUM_SOLDIERS)
                .map(|i| Soldier::new(x + i as f64 * 20.0, y))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_json_round_trip() {
        let state = GameState::new(vec![Player::new(
            7,
            Team::One,
            vec![Soldier::new(1.5, 2.5)],
        )]);
        assert_eq!(
            serde_json::from_str::<GameState>(&serde_json::to_string(&state).unwrap()).unwrap(),
            state
        );
    }

    #[test]
    fn team_accepts_numeric_team_values_on_deserialize() {
        // Rows written by the battle-mode build stored team as u8. The Team enum
        // must read those back.
        let json = r#"{"players":[{"id":7,"team":1,"soldiers":[{"x":1.5,"y":2.5,"alive":true}],"current_soldier":0}],"turn":0,"team_turn":[0,0]}"#;
        assert_eq!(
            serde_json::from_str::<GameState>(json).unwrap().players[0].team,
            Team::One
        );
        let json = r#"{"players":[{"id":7,"team":2,"soldiers":[{"x":1.5,"y":2.5,"alive":true}],"current_soldier":0}],"turn":0,"team_turn":[0,0]}"#;
        assert_eq!(
            serde_json::from_str::<GameState>(json).unwrap().players[0].team,
            Team::Two
        );
        let json = r#"{"players":[{"id":7,"team":"One","soldiers":[{"x":1.5,"y":2.5,"alive":true}],"current_soldier":0}],"turn":0,"team_turn":[0,0]}"#;
        assert_eq!(
            serde_json::from_str::<GameState>(json).unwrap().players[0].team,
            Team::One
        );
    }
}
