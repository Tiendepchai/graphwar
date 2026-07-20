// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use crate::constants::{INITIAL_NUM_SOLDIERS, MAX_SOLDIERS_PER_PLAYER};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Team {
    One,
    Two,
}

#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub struct GameState {
    pub players: Vec<Player>,
    pub turn: usize,
}

impl GameState {
    pub fn new(players: Vec<Player>) -> Self {
        Self { players, turn: 0 }
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
