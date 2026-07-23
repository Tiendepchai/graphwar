use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use graphwar_game_core::{
    Circle, Expr, GameState, Player, SeededGenerator, Soldier, Team, Terrain, TrajectoryMode,
    constants::{MAX_PLAYERS, MAX_SOLDIERS_PER_PLAYER, PLANE_HEIGHT, PLANE_LENGTH, SOLDIER_RADIUS},
    parse, trace,
};

use graphwar_protocol::{
    GameMode, GameSnapshot, Phase, PlayerSnapshot, RoomSnapshot, RoomVisibility, ShotResolved,
    SoldierPosition, SoldierSnapshot, TerrainCircle,
};
use thiserror::Error;
use uuid::Uuid;

pub type RoomRegistry = std::sync::Arc<tokio::sync::RwLock<Registry>>;

const TURN_DURATION: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Registry {
    rooms: HashMap<Uuid, Room>,
}

struct Room {
    snapshot: RoomSnapshot,
    invite: Option<String>,
    members: HashMap<Uuid, bool>,
    bots: HashMap<Uuid, BotSpec>,
    game: Option<Match>,
}

#[derive(Clone)]
struct BotSpec {
    level: u8,
    seed: u64,
    memory: crate::bot::SearchMemory,
}

struct Match {
    mode: GameMode,
    terrain: Terrain,
    state: GameState,
    player_ids: Vec<Uuid>,
    turn_deadline_at: i64,
}

pub struct StartOutcome {
    pub snapshot: RoomSnapshot,
    pub game: GameSnapshot,
}

pub struct FireOutcome {
    pub snapshot: RoomSnapshot,
    pub shot: ShotResolved,
}

pub struct LeaveOutcome {
    pub room_id: Uuid,
    pub broadcast: Option<LeaveBroadcast>,
}

pub enum LeaveBroadcast {
    Room(RoomSnapshot),
    StateSync {
        snapshot: RoomSnapshot,
        game: GameSnapshot,
    },
    TurnStarted {
        snapshot: RoomSnapshot,
        game: GameSnapshot,
    },
    GameFinished {
        snapshot: RoomSnapshot,
        shot: ShotResolved,
    },
}

#[derive(Clone)]
pub struct BotTurn {
    room_id: Uuid,
    player: Uuid,
    revision: u64,
    pub mode: GameMode,
    pub team: Team,
    pub level: u8,
    pub seed: u64,
    pub memory: crate::bot::SearchMemory,
    pub terrain: Terrain,
    pub state: GameState,
}

#[derive(Debug, Error)]
pub enum RoomError {
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("room not found")]
    NotFound,
    #[error("room is private")]
    Private,
    #[error("not a room member")]
    NotMember,
    #[error("only the room owner may do that")]
    NotOwner,
    #[error("only the player may change their setup")]
    NotSlotOwner,
    #[error("action is invalid during the current phase")]
    WrongPhase,
    #[error("it is not your turn")]
    NotTurn,
}

impl RoomError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound => "not_found",
            Self::Private => "private",
            Self::NotMember => "not_member",
            Self::NotOwner => "not_owner",
            Self::NotSlotOwner => "not_slot_owner",
            Self::WrongPhase => "wrong_phase",
            Self::NotTurn => "not_turn",
        }
    }
}

impl Registry {
    pub fn create(
        &mut self,
        owner: Uuid,
        display_name: String,
        name: String,
        visibility: RoomVisibility,
    ) -> Result<(RoomSnapshot, Option<String>), RoomError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(RoomError::Invalid("room name must be 1-64 characters"));
        }
        if self.room_id_for(owner).is_some() {
            return Err(RoomError::Invalid("leave the current room first"));
        }
        let id = Uuid::new_v4();
        let snapshot = RoomSnapshot {
            id,
            name: name.into(),
            visibility,
            phase: Phase::Lobby,
            revision: 0,
            mode: GameMode::Function,
            players: vec![PlayerSnapshot {
                id: owner,
                display_name,
                owner: true,
                ready: false,
                team: 1,
                soldiers: 2,
                is_bot: false,
            }],
        };
        let invite = (visibility == RoomVisibility::Private).then(|| Uuid::new_v4().to_string());
        self.rooms.insert(
            id,
            Room {
                snapshot: snapshot.clone(),
                invite: invite.clone(),
                members: HashMap::from([(owner, false)]),
                bots: HashMap::new(),
                game: None,
            },
        );
        Ok((snapshot, invite))
    }

    pub fn join(
        &mut self,
        player: Uuid,
        display_name: String,
        room_id: Uuid,
        invite: Option<&str>,
    ) -> Result<RoomSnapshot, RoomError> {
        if self.room_id_for(player).is_some_and(|id| id != room_id) {
            return Err(RoomError::Invalid("leave the current room first"));
        }
        let room = self.rooms.get_mut(&room_id).ok_or(RoomError::NotFound)?;
        if room.snapshot.visibility == RoomVisibility::Private && room.invite.as_deref() != invite {
            return Err(RoomError::Private);
        }
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if room.snapshot.players.len() >= MAX_PLAYERS && !room.members.contains_key(&player) {
            return Err(RoomError::Invalid("room is full"));
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = room.members.entry(player) {
            entry.insert(false);
            room.snapshot.players.push(PlayerSnapshot {
                id: player,
                display_name,
                owner: false,
                ready: false,
                team: 2,
                soldiers: 2,
                is_bot: false,
            });
            room.snapshot.revision += 1;
        }
        Ok(room.snapshot.clone())
    }

    pub fn leave(&mut self, player: Uuid) -> Result<LeaveOutcome, RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        let phase = self.rooms[&id].snapshot.phase;
        if matches!(phase, Phase::Lobby | Phase::Finished) {
            let room = self.rooms.get_mut(&id).expect("room ID came from registry");
            if phase == Phase::Finished {
                room.snapshot.phase = Phase::Lobby;
                room.game = None;
                reset_readiness(room);
            }
            if remove_member(room, player) {
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::Room(room.snapshot.clone())),
                })
            } else {
                self.rooms.remove(&id);
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: None,
                })
            }
        } else {
            let room = self.rooms.get_mut(&id).expect("room ID came from registry");
            let (current, winner_team) = {
                let game = room.game.as_mut().ok_or(RoomError::WrongPhase)?;
                let slot = game
                    .player_ids
                    .iter()
                    .position(|id| *id == player)
                    .ok_or(RoomError::NotMember)?;
                for soldier in &mut game.state.players[slot].soldiers {
                    soldier.alive = false;
                }
                (game.state.turn == slot, winner(&game.state))
            };
            if !remove_member(room, player) {
                self.rooms.remove(&id);
                return Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: None,
                });
            }
            if let Some(winner_team) = winner_team {
                room.snapshot.phase = Phase::Finished;
                let game_snapshot = snapshot_for_game(room);
                return Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::GameFinished {
                        snapshot: room.snapshot.clone(),
                        shot: ShotResolved {
                            path: Vec::new(),
                            hits: Vec::new(),
                            explosion: None,
                            winner_team: Some(winner_team),
                            game: game_snapshot,
                        },
                    }),
                });
            }
            if phase == Phase::Planning && current {
                let game = room.game.as_mut().expect("active game exists");
                advance_turn(&mut game.state);
                game.turn_deadline_at = turn_deadline();
                let game_snapshot = snapshot_for_game(room);
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::TurnStarted {
                        snapshot: room.snapshot.clone(),
                        game: game_snapshot,
                    }),
                })
            } else {
                let game_snapshot = snapshot_for_game(room);
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::StateSync {
                        snapshot: room.snapshot.clone(),
                        game: game_snapshot,
                    }),
                })
            }
        }
    }

    pub fn disconnect(&mut self, player: Uuid) -> Result<Option<RoomSnapshot>, RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        let room = self.rooms.get_mut(&id).expect("room ID came from registry");
        if !matches!(room.snapshot.phase, Phase::Lobby | Phase::Finished) {
            return Err(RoomError::WrongPhase);
        }
        if room.snapshot.phase == Phase::Finished {
            room.snapshot.phase = Phase::Lobby;
            room.game = None;
            reset_readiness(room);
        }
        if remove_member(room, player) {
            Ok(Some(room.snapshot.clone()))
        } else {
            self.rooms.remove(&id);
            Ok(None)
        }
    }

    pub fn set_ready(&mut self, player: Uuid, ready: bool) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        *room.members.get_mut(&player).expect("member room") = ready;
        room.snapshot
            .players
            .iter_mut()
            .find(|member| member.id == player)
            .expect("member room")
            .ready = ready;
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn set_mode(&mut self, player: Uuid, mode: GameMode) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        if room.snapshot.mode != mode {
            room.snapshot.mode = mode;
            reset_readiness(room);
            room.snapshot.revision += 1;
        }
        Ok(room.snapshot.clone())
    }

    pub fn set_team(
        &mut self,
        player: Uuid,
        player_id: Uuid,
        team: u8,
    ) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if player != player_id && !(is_owner(room, player) && room.bots.contains_key(&player_id)) {
            return Err(RoomError::NotSlotOwner);
        }
        if !(1..=2).contains(&team) {
            return Err(RoomError::Invalid("team must be 1 or 2"));
        }
        let member = room
            .snapshot
            .players
            .iter_mut()
            .find(|member| member.id == player_id)
            .ok_or(RoomError::NotMember)?;
        if member.team != team {
            member.team = team;
            reset_readiness(room);
            room.snapshot.revision += 1;
        }
        Ok(room.snapshot.clone())
    }

    pub fn set_soldiers(
        &mut self,
        player: Uuid,
        player_id: Uuid,
        soldiers: u8,
    ) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if player != player_id && !(is_owner(room, player) && room.bots.contains_key(&player_id)) {
            return Err(RoomError::NotSlotOwner);
        }
        if soldiers == 0 || usize::from(soldiers) > MAX_SOLDIERS_PER_PLAYER {
            return Err(RoomError::Invalid("soldiers must be 1-4"));
        }
        let member = room
            .snapshot
            .players
            .iter_mut()
            .find(|member| member.id == player_id)
            .ok_or(RoomError::NotMember)?;
        if member.soldiers != soldiers {
            member.soldiers = soldiers;
            reset_readiness(room);
            room.snapshot.revision += 1;
        }
        Ok(room.snapshot.clone())
    }

    pub fn add_bot(&mut self, player: Uuid, level: u8) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        if !(1..=8).contains(&level) {
            return Err(RoomError::Invalid("bot level must be 1-8"));
        }
        if room.snapshot.players.len() >= MAX_PLAYERS {
            return Err(RoomError::Invalid("room is full"));
        }
        let id = Uuid::new_v4();
        let team = if room
            .snapshot
            .players
            .iter()
            .filter(|slot| slot.team == 1)
            .count()
            <= room
                .snapshot
                .players
                .iter()
                .filter(|slot| slot.team == 2)
                .count()
        {
            1
        } else {
            2
        };
        room.members.insert(id, true);
        room.bots.insert(
            id,
            BotSpec {
                level,
                seed: id.as_u128() as u64,
                memory: crate::bot::SearchMemory::default(),
            },
        );
        room.snapshot.players.push(PlayerSnapshot {
            id,
            display_name: format!("Bot {}", room.bots.len()),
            owner: false,
            ready: true,
            team,
            soldiers: 2,
            is_bot: true,
        });
        reset_readiness(room);
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn remove_bot(&mut self, player: Uuid, player_id: Uuid) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        if room.bots.remove(&player_id).is_none() {
            return Err(RoomError::Invalid("player is not a bot"));
        }
        room.members.remove(&player_id);
        room.snapshot.players.retain(|slot| slot.id != player_id);
        reset_readiness(room);
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn start_game(&mut self, player: Uuid) -> Result<StartOutcome, RoomError> {
        let room = self.member_room_mut(player)?;
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        if room.snapshot.phase != Phase::Lobby || room.snapshot.players.len() < 2 {
            return Err(RoomError::WrongPhase);
        }
        if room.snapshot.players.iter().any(|member| !member.ready)
            || !has_two_teams(&room.snapshot.players)
        {
            return Err(RoomError::WrongPhase);
        }
        let game_state = new_match(
            match_seed(room.snapshot.id),
            &room.snapshot.players,
            room.snapshot.mode,
        )?;
        room.snapshot.phase = Phase::Planning;
        room.snapshot.revision += 1;
        room.game = Some(game_state);
        let game = snapshot_for_game(room);
        Ok(StartOutcome {
            snapshot: room.snapshot.clone(),
            game,
        })
    }

    pub fn fire(
        &mut self,
        player: Uuid,
        function: String,
        angle_deg: f64,
    ) -> Result<FireOutcome, RoomError> {
        if function.trim().is_empty() || function.len() > 256 {
            return Err(RoomError::Invalid("function must be 1-256 characters"));
        }
        if !angle_deg.is_finite() || !(-90.0..=90.0).contains(&angle_deg) {
            return Err(RoomError::Invalid(
                "angle must be finite and between -90 and 90",
            ));
        }
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Planning {
            return Err(RoomError::WrongPhase);
        }
        if room
            .game
            .as_ref()
            .is_some_and(|game| game.turn_deadline_at <= unix_timestamp())
        {
            return Err(RoomError::WrongPhase);
        }
        let active_id = room
            .game
            .as_ref()
            .and_then(|game| game.player_ids.get(game.state.turn))
            .copied()
            .ok_or(RoomError::WrongPhase)?;
        if active_id != player {
            return Err(RoomError::NotTurn);
        }
        let expr = parse(function.trim()).map_err(|_| RoomError::Invalid("invalid function"))?;
        let game = room.game.as_mut().expect("checked above");
        if !mode_allows(&expr, game.mode) {
            return Err(RoomError::Invalid(
                "function uses variables unavailable in this mode",
            ));
        }
        let mode = trajectory_mode(game.mode, angle_deg);
        let inverted = matches!(game.state.players[game.state.turn].team, Team::Two);
        let trajectory = trace(&expr, mode, &game.terrain, &game.state, inverted)
            .map_err(|_| RoomError::Invalid("function produced no finite trajectory"))?;
        let explosion = trajectory.points.last().copied().map(|(x, y)| Circle {
            x,
            y,
            radius: graphwar_game_core::constants::EXPLOSION_RADIUS,
        });
        apply_hits(game, &trajectory.hits);
        if let Some(explosion) = explosion {
            apply_explosion(game, explosion);
            game.terrain
                .explode(explosion.x, explosion.y, explosion.radius);
        }
        let winner_team = winner(&game.state);
        if winner_team.is_some() {
            room.snapshot.phase = Phase::Finished;
        } else {
            room.snapshot.phase = Phase::Resolving;
            game.turn_deadline_at = resolution_deadline();
        }
        room.snapshot.revision += 1;
        let shot = ShotResolved {
            path: trajectory.points,
            hits: trajectory
                .hits
                .into_iter()
                .filter_map(|hit| soldier_snapshot(game, hit.player, hit.soldier))
                .collect(),
            explosion: explosion.map(circle_snapshot),
            winner_team,
            game: snapshot_for_game(room),
        };
        Ok(FireOutcome {
            snapshot: room.snapshot.clone(),
            shot,
        })
    }

    pub fn pending_bot_turns(&self) -> Vec<BotTurn> {
        self.rooms
            .iter()
            .filter_map(|(room_id, room)| {
                if room.snapshot.phase != Phase::Planning {
                    return None;
                }
                let game = room.game.as_ref()?;
                let player = *game.player_ids.get(game.state.turn)?;
                let spec = room.bots.get(&player)?;
                Some(BotTurn {
                    room_id: *room_id,
                    player,
                    revision: room.snapshot.revision,
                    mode: game.mode,
                    team: game.state.players.get(game.state.turn)?.team,
                    level: spec.level,
                    seed: spec.seed ^ room.snapshot.revision,
                    memory: spec.memory.clone(),
                    terrain: game.terrain.clone(),
                    state: game.state.clone(),
                })
            })
            .collect()
    }

    pub fn apply_bot_turn(
        &mut self,
        turn: BotTurn,
        result: crate::bot::SearchOutcome,
    ) -> Result<Option<FireOutcome>, RoomError> {
        let room = self
            .rooms
            .get_mut(&turn.room_id)
            .ok_or(RoomError::NotFound)?;
        let game = room.game.as_ref().ok_or(RoomError::WrongPhase)?;
        let active = game.player_ids.get(game.state.turn);
        let unchanged = room.snapshot.phase == Phase::Planning
            && room.snapshot.revision == turn.revision
            && active.is_some_and(|player| *player == turn.player)
            && room.bots.get(&turn.player).is_some_and(|bot| {
                bot.level == turn.level && (bot.seed ^ turn.revision) == turn.seed
            });
        if !unchanged {
            return Ok(None);
        }
        let (function, angle) = result
            .shot
            .ok_or(RoomError::Invalid("bot produced no valid shot"))?;
        let outcome = self.fire(turn.player, function, angle)?;
        self.rooms
            .get_mut(&turn.room_id)
            .expect("validated room")
            .bots
            .get_mut(&turn.player)
            .expect("validated bot")
            .memory = result.memory;
        Ok(Some(outcome))
    }

    pub fn skip_bot_turn(&mut self, turn: BotTurn) -> Result<Option<StartOutcome>, RoomError> {
        let room = self
            .rooms
            .get_mut(&turn.room_id)
            .ok_or(RoomError::NotFound)?;
        let game = room.game.as_mut().ok_or(RoomError::WrongPhase)?;
        let active = game.player_ids.get(game.state.turn);
        let unchanged = room.snapshot.phase == Phase::Planning
            && room.snapshot.revision == turn.revision
            && active.is_some_and(|player| *player == turn.player)
            && room.bots.get(&turn.player).is_some_and(|bot| {
                bot.level == turn.level && (bot.seed ^ turn.revision) == turn.seed
            });
        if !unchanged {
            return Ok(None);
        }
        advance_turn(&mut game.state);
        game.turn_deadline_at = turn_deadline();
        room.snapshot.revision += 1;
        Ok(Some(StartOutcome {
            snapshot: room.snapshot.clone(),
            game: snapshot_for_game(room),
        }))
    }

    pub fn expire_turns(&mut self) -> Vec<StartOutcome> {
        let now = unix_timestamp();
        self.rooms
            .values_mut()
            .filter_map(|room| {
                let game = room.game.as_mut()?;
                if game.turn_deadline_at > now {
                    return None;
                }
                if room.snapshot.phase == Phase::Resolving {
                    advance_turn(&mut game.state);
                    game.turn_deadline_at = turn_deadline();
                    room.snapshot.phase = Phase::Planning;
                } else if room.snapshot.phase != Phase::Planning {
                    return None;
                } else {
                    advance_turn(&mut game.state);
                    game.turn_deadline_at = turn_deadline();
                }
                room.snapshot.revision += 1;
                Some(StartOutcome {
                    snapshot: room.snapshot.clone(),
                    game: snapshot_for_game(room),
                })
            })
            .collect()
    }

    pub fn require_member(&self, player: Uuid) -> Result<(), RoomError> {
        self.room_id_for(player)
            .map(|_| ())
            .ok_or(RoomError::NotMember)
    }

    pub fn member_snapshot(&self, player: Uuid) -> Result<RoomSnapshot, RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        Ok(self.rooms[&id].snapshot.clone())
    }

    pub fn member_state(
        &self,
        player: Uuid,
    ) -> Result<(RoomSnapshot, Option<GameSnapshot>), RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        let room = &self.rooms[&id];
        Ok((
            room.snapshot.clone(),
            room.game.as_ref().map(|_| snapshot_for_game(room)),
        ))
    }

    pub fn is_member_of(&self, player: Uuid, room_id: Uuid) -> bool {
        self.rooms
            .get(&room_id)
            .is_some_and(|room| room.members.contains_key(&player))
    }

    pub fn member_ids(&self, room_id: Uuid) -> Vec<Uuid> {
        self.rooms
            .get(&room_id)
            .map(|room| {
                room.members
                    .keys()
                    .filter(|player| !room.bots.contains_key(player))
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn public_snapshots(&self) -> Vec<RoomSnapshot> {
        self.rooms
            .values()
            .filter(|room| room.snapshot.visibility == RoomVisibility::Public)
            .map(|room| room.snapshot.clone())
            .collect()
    }

    fn room_id_for(&self, player: Uuid) -> Option<Uuid> {
        self.rooms
            .iter()
            .find_map(|(id, room)| room.members.contains_key(&player).then_some(*id))
    }

    fn member_room_mut(&mut self, player: Uuid) -> Result<&mut Room, RoomError> {
        self.rooms
            .values_mut()
            .find(|room| room.members.contains_key(&player))
            .ok_or(RoomError::NotMember)
    }
}

fn new_match(seed: u64, players: &[PlayerSnapshot], mode: GameMode) -> Result<Match, RoomError> {
    let mut generator = SeededGenerator::new(seed);
    let terrain = Terrain::new(generator.terrain());
    let slots = alternating_players(players);
    let player_ids = slots.iter().map(|player| player.id).collect();
    let mut placed = Vec::new();
    let mut game_players = Vec::with_capacity(slots.len());
    for (index, player) in slots.into_iter().enumerate() {
        let team = team(player.team);
        let soldiers = spawn_soldiers(&terrain, team, player.soldiers, index, seed, &placed)?;
        placed.extend(soldiers.iter().cloned());
        game_players.push(Player::new(index as u32, team, soldiers));
    }
    Ok(Match {
        mode,
        terrain,
        state: GameState::new(game_players),
        player_ids,
        turn_deadline_at: turn_deadline(),
    })
}

fn alternating_players(players: &[PlayerSnapshot]) -> Vec<&PlayerSnapshot> {
    let mut result = Vec::with_capacity(players.len());
    let first_team = if players.len() % 2 == 0 { 1 } else { 2 };
    for offset in 0..2 {
        let team = if offset == 0 {
            first_team
        } else {
            3 - first_team
        };
        result.extend(players.iter().filter(|player| player.team == team));
    }
    result.sort_by_key(|player| {
        let original = players
            .iter()
            .position(|candidate| candidate.id == player.id)
            .unwrap_or(usize::MAX);
        (original / 2, player.team != first_team)
    });
    result
}

fn spawn_soldiers(
    terrain: &Terrain,
    team: Team,
    count: u8,
    player_index: usize,
    match_seed: u64,
    placed: &[Soldier],
) -> Result<Vec<Soldier>, RoomError> {
    let x_start = SOLDIER_RADIUS as i32;
    let x_end = PLANE_LENGTH / 2 - SOLDIER_RADIUS as i32;
    let y_start = SOLDIER_RADIUS as i32;
    let y_end = PLANE_HEIGHT - SOLDIER_RADIUS as i32;
    let x_span = (x_end - x_start) as u64;
    let y_span = (y_end - y_start) as u64;
    let mut result = Vec::with_capacity(usize::from(count));
    for soldier_index in 0..count {
        let seed = match_seed
            .wrapping_add((player_index as u64).wrapping_mul(MAX_SOLDIERS_PER_PLAYER as u64))
            .wrapping_add(u64::from(soldier_index));
        let mut found = None;
        for attempt in 0..10_000_u64 {
            let x = x_start
                + (seed.wrapping_mul(97).wrapping_add(attempt.wrapping_mul(53)) % x_span) as i32;
            let y = y_start
                + (seed
                    .wrapping_mul(193)
                    .wrapping_add(attempt.wrapping_mul(89))
                    % y_span) as i32;
            let x = if team == Team::One {
                f64::from(x)
            } else {
                f64::from(PLANE_LENGTH - 1 - x)
            };
            let y = f64::from(y);
            if !terrain.collides_circle(x, y, SOLDIER_RADIUS)
                && placed.iter().chain(&result).all(|soldier: &Soldier| {
                    (soldier.x - x).abs() >= 20.0 || (soldier.y - y).abs() >= 20.0
                })
            {
                found = Some(Soldier::new(x, y));
                break;
            }
        }
        result.push(found.ok_or(RoomError::Invalid("could not place soldiers"))?);
    }
    Ok(result)
}

fn match_seed(room_id: Uuid) -> u64 {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let room = room_id.as_u128();
    (timestamp as u64)
        ^ ((timestamp >> 64) as u64).rotate_left(32)
        ^ (room as u64)
        ^ ((room >> 64) as u64).rotate_left(17)
}

fn team(value: u8) -> Team {
    if value == 1 { Team::One } else { Team::Two }
}

fn remove_member(room: &mut Room, player: Uuid) -> bool {
    room.members.remove(&player);
    room.bots.remove(&player);
    room.snapshot.players.retain(|member| member.id != player);
    room.snapshot.revision += 1;
    if room.snapshot.players.iter().all(|member| member.is_bot) {
        return false;
    }
    if !room.snapshot.players.iter().any(|member| member.owner) {
        room.snapshot
            .players
            .iter_mut()
            .find(|member| !member.is_bot)
            .expect("remaining room has a human")
            .owner = true;
    }
    true
}

fn is_owner(room: &Room, player: Uuid) -> bool {
    room.snapshot
        .players
        .iter()
        .any(|member| member.id == player && member.owner)
}

fn has_two_teams(players: &[PlayerSnapshot]) -> bool {
    players.iter().any(|player| player.team == 1) && players.iter().any(|player| player.team == 2)
}

fn reset_readiness(room: &mut Room) {
    for (player, ready) in &mut room.members {
        *ready = room.bots.contains_key(player);
    }
    for player in &mut room.snapshot.players {
        player.ready = player.is_bot;
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn turn_deadline() -> i64 {
    unix_timestamp().saturating_add(TURN_DURATION.as_secs() as i64)
}

fn resolution_deadline() -> i64 {
    unix_timestamp().saturating_add(3)
}

fn mode_allows(expression: &Expr, mode: GameMode) -> bool {
    expression.variables_allowed(mode != GameMode::Function, mode == GameMode::SecondOrder)
}

fn trajectory_mode(mode: GameMode, angle_deg: f64) -> TrajectoryMode {
    match mode {
        GameMode::Function => TrajectoryMode::Function,
        GameMode::FirstOrder => TrajectoryMode::FirstOrder,
        GameMode::SecondOrder => TrajectoryMode::SecondOrder {
            angle: angle_deg.to_radians(),
        },
    }
}

fn apply_hits(game: &mut Match, hits: &[graphwar_game_core::Hit]) {
    for hit in hits {
        if let Some(soldier) = game
            .state
            .players
            .get_mut(hit.player)
            .and_then(|player| player.soldiers.get_mut(hit.soldier))
        {
            soldier.alive = false;
        }
    }
}

fn apply_explosion(game: &mut Match, explosion: Circle) {
    let radius_squared = explosion.radius * explosion.radius;
    for player in &mut game.state.players {
        for soldier in &mut player.soldiers {
            let distance_squared = (soldier.x - explosion.x).mul_add(
                soldier.x - explosion.x,
                (soldier.y - explosion.y) * (soldier.y - explosion.y),
            );
            if distance_squared <= radius_squared {
                soldier.alive = false;
            }
        }
    }
}

fn winner(game: &GameState) -> Option<u8> {
    let mut one = false;
    let mut two = false;
    for player in &game.players {
        if player.living().next().is_some() {
            match player.team {
                Team::One => one = true,
                Team::Two => two = true,
            }
        }
    }
    match (one, two) {
        (true, false) => Some(1),
        (false, true) => Some(2),
        (false, false) => Some(0),
        (true, true) => None,
    }
}

fn advance_turn(game: &mut GameState) {
    if game.players.is_empty() {
        return;
    }
    let current = game.turn;
    if let Some(player) = game.players.get_mut(current) {
        let current_soldier = player.current_soldier;
        let next_soldier = player
            .living()
            .find(|(index, _)| *index > current_soldier)
            .map(|(index, _)| index)
            .or_else(|| player.living().next().map(|(index, _)| index));
        if let Some(index) = next_soldier {
            player.current_soldier = index;
        }
    }
    for offset in 1..=game.players.len() {
        let candidate = (current + offset) % game.players.len();
        if game.players[candidate].living().next().is_some() {
            game.turn = candidate;
            return;
        }
    }
}

fn snapshot_for_game(room: &Room) -> GameSnapshot {
    let game = room.game.as_ref().expect("game snapshot requires game");
    let turn_player_id = game.player_ids.get(game.state.turn).copied();
    GameSnapshot {
        room_id: room.snapshot.id,
        revision: room.snapshot.revision,
        mode: game.mode,
        turn_player_id,
        turn_deadline_at: (room.snapshot.phase == Phase::Planning).then_some(game.turn_deadline_at),
        soldiers: game
            .state
            .players
            .iter()
            .enumerate()
            .flat_map(|(player_index, player)| {
                let player_id = game.player_ids[player_index];
                player
                    .soldiers
                    .iter()
                    .enumerate()
                    .map(move |(index, soldier)| SoldierPosition {
                        player_id,
                        index,
                        team: team_id(player.team),
                        x: soldier.x,
                        y: soldier.y,
                        alive: soldier.alive,
                        active: player_index == game.state.turn && index == player.current_soldier,
                    })
            })
            .collect(),
        terrain: game
            .terrain
            .circles
            .iter()
            .copied()
            .map(circle_snapshot)
            .collect(),
        terrain_cuts: game
            .terrain
            .explosions
            .iter()
            .copied()
            .map(circle_snapshot)
            .collect(),
    }
}

fn soldier_snapshot(game: &Match, player_index: usize, index: usize) -> Option<SoldierSnapshot> {
    Some(SoldierSnapshot {
        player_id: *game.player_ids.get(player_index)?,
        index,
        team: team_id(game.state.players.get(player_index)?.team),
        alive: game
            .state
            .players
            .get(player_index)?
            .soldiers
            .get(index)?
            .alive,
    })
}

fn team_id(team: Team) -> u8 {
    match team {
        Team::One => 1,
        Team::Two => 2,
    }
}

fn circle_snapshot(circle: Circle) -> TerrainCircle {
    TerrainCircle {
        x: circle.x,
        y: circle.y,
        radius: circle.radius,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_only_starts_ready_two_player_game() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        let start = registry.start_game(owner).unwrap();
        assert_eq!(start.snapshot.phase, Phase::Planning);
        assert_eq!(start.game.soldiers.len(), 4);
    }

    #[test]
    fn fixed_match_seed_reproduces_layout() {
        let players = [
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "One".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 1,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "Two".into(),
                owner: false,
                ready: true,
                team: 2,
                soldiers: 1,
                is_bot: false,
            },
        ];
        let first = new_match(123, &players, GameMode::Function).unwrap();
        let second = new_match(123, &players, GameMode::Function).unwrap();

        assert_eq!(first.terrain, second.terrain);
        assert_eq!(first.state, second.state);
    }

    #[test]
    fn spawn_seed_changes_first_soldier_position() {
        let terrain = Terrain::default();
        let first = spawn_soldiers(&terrain, Team::One, 1, 0, 0, &[]).unwrap();
        let second = spawn_soldiers(&terrain, Team::One, 1, 0, 1, &[]).unwrap();

        assert_ne!((first[0].x, first[0].y), (second[0].x, second[0].y));
    }

    #[test]
    fn setup_changes_are_authorized_and_clear_readiness() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();

        assert!(matches!(
            registry.set_mode(guest, GameMode::SecondOrder),
            Err(RoomError::NotOwner)
        ));
        assert!(matches!(
            registry.set_team(owner, guest, 1),
            Err(RoomError::NotSlotOwner)
        ));
        let snapshot = registry.set_soldiers(guest, guest, 4).unwrap();
        assert_eq!(snapshot.players[1].soldiers, 4);
        assert!(snapshot.players.iter().all(|player| !player.ready));
        assert!(matches!(
            registry.set_team(guest, guest, 3),
            Err(RoomError::Invalid("team must be 1 or 2"))
        ));
    }

    #[test]
    fn selected_setup_drives_match_and_valid_spawns() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_mode(owner, GameMode::FirstOrder).unwrap();
        registry.set_soldiers(owner, owner, 1).unwrap();
        registry.set_soldiers(guest, guest, 4).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();

        let start = registry.start_game(owner).unwrap();
        assert_eq!(start.game.mode, GameMode::FirstOrder);
        assert_eq!(start.game.soldiers.len(), 5);
        let room = registry.rooms.get(&room.id).unwrap();
        let game = room.game.as_ref().unwrap();
        let soldiers = game
            .state
            .players
            .iter()
            .flat_map(|player| &player.soldiers)
            .collect::<Vec<_>>();
        for soldier in &soldiers {
            assert!(
                !game
                    .terrain
                    .collides_circle(soldier.x, soldier.y, SOLDIER_RADIUS)
            );
            assert!(soldier.x >= SOLDIER_RADIUS);
            assert!(soldier.x + SOLDIER_RADIUS < f64::from(PLANE_LENGTH));
            assert!(soldier.y >= SOLDIER_RADIUS);
            assert!(soldier.y + SOLDIER_RADIUS < f64::from(PLANE_HEIGHT));
        }
        for (index, soldier) in soldiers.iter().enumerate() {
            assert!(soldiers[index + 1..].iter().all(|other| {
                (soldier.x - other.x).abs() >= 20.0 || (soldier.y - other.y).abs() >= 20.0
            }));
        }
    }

    #[test]
    fn game_requires_both_teams() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_team(guest, guest, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        assert!(matches!(
            registry.start_game(owner),
            Err(RoomError::WrongPhase)
        ));
    }

    #[test]
    fn leaving_owner_transfers_to_human_and_removes_bot_only_room() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.add_bot(owner, 1).unwrap();

        let outcome = registry.leave(owner).unwrap();
        let LeaveBroadcast::Room(snapshot) = outcome.broadcast.unwrap() else {
            panic!("lobby leave should broadcast room");
        };
        assert!(
            snapshot
                .players
                .iter()
                .any(|player| player.id == guest && player.owner)
        );
        assert!(
            snapshot
                .players
                .iter()
                .all(|player| !player.is_bot || !player.owner)
        );
        assert!(registry.leave(guest).unwrap().broadcast.is_none());
        assert!(!registry.rooms.contains_key(&room.id));
    }

    fn started_registry() -> (Registry, Uuid, Uuid, Uuid) {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        (registry, room.id, owner, guest)
    }

    fn started_three_player_registry() -> (Registry, Uuid, Uuid, Uuid, Uuid) {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let third = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.join(third, "Third".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.set_ready(third, true).unwrap();
        registry.start_game(owner).unwrap();
        (registry, room.id, owner, guest, third)
    }

    fn started_four_player_registry() -> (Registry, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let (mut registry, room_id, owner, guest, third) = started_three_player_registry();
        let fourth = Uuid::new_v4();
        {
            let room = registry.rooms.get_mut(&room_id).unwrap();
            room.snapshot.phase = Phase::Lobby;
            room.game = None;
        }
        registry
            .join(fourth, "Fourth".into(), room_id, None)
            .unwrap();
        registry.set_team(fourth, fourth, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.set_ready(third, true).unwrap();
        registry.set_ready(fourth, true).unwrap();
        registry.start_game(owner).unwrap();
        (registry, room_id, owner, guest, third, fourth)
    }

    #[test]
    fn planning_leave_advances_only_when_current_player_leaves() {
        let (mut registry, room_id, owner, _guest, _third, _fourth) =
            started_four_player_registry();
        let current = registry
            .member_state(owner)
            .unwrap()
            .1
            .unwrap()
            .turn_player_id
            .unwrap();
        let outcome = registry.leave(current).unwrap();
        let LeaveBroadcast::TurnStarted { snapshot, game } = outcome.broadcast.unwrap() else {
            panic!("current planning leave should start the next turn");
        };
        let next_turn = game.turn_player_id;
        let deadline = game.turn_deadline_at;
        assert_eq!(snapshot.phase, Phase::Planning);
        assert_ne!(next_turn, Some(current));
        assert!(deadline.unwrap() > unix_timestamp());
        assert_eq!(snapshot.players.len(), 3);
        assert!(
            game.soldiers
                .iter()
                .filter(|soldier| soldier.player_id == current)
                .all(|soldier| !soldier.alive)
        );

        let non_current = registry
            .member_state(next_turn.unwrap())
            .unwrap()
            .0
            .players
            .iter()
            .map(|player| player.id)
            .find(|player| *player != next_turn.unwrap())
            .unwrap();
        let outcome = registry.leave(non_current).unwrap();
        let LeaveBroadcast::StateSync { snapshot, game } = outcome.broadcast.unwrap() else {
            panic!("non-current planning leave should sync without advancing");
        };
        assert_eq!(snapshot.phase, Phase::Planning);
        assert_eq!(game.turn_player_id, next_turn);
        assert_eq!(game.turn_deadline_at, deadline);
        assert_eq!(snapshot.players.len(), 2);
        assert!(registry.rooms.contains_key(&room_id));
    }

    #[test]
    fn resolving_nonterminal_leave_syncs_without_advancing_turn() {
        let (mut registry, room_id, owner, guest, third) = started_three_player_registry();
        let active = registry
            .member_state(owner)
            .unwrap()
            .1
            .unwrap()
            .turn_player_id
            .unwrap();
        registry.fire(active, "0".into(), 0.0).unwrap();
        let before = registry.member_state(third).unwrap().1.unwrap();
        let leaver = [owner, guest, third]
            .into_iter()
            .find(|player| *player != active && *player != owner)
            .unwrap();

        let outcome = registry.leave(leaver).unwrap();
        let LeaveBroadcast::StateSync { snapshot, game } = outcome.broadcast.unwrap() else {
            panic!("nonterminal resolving leave should sync");
        };
        assert_eq!(snapshot.phase, Phase::Resolving);
        assert_eq!(game.turn_player_id, before.turn_player_id);
        assert_eq!(game.turn_deadline_at, before.turn_deadline_at);
        assert_eq!(
            registry.rooms[&room_id].game.as_ref().unwrap().state.turn,
            0
        );
        assert!(
            game.soldiers
                .iter()
                .filter(|soldier| soldier.player_id == leaver)
                .all(|soldier| !soldier.alive)
        );
    }

    #[test]
    fn current_turn_leave_forfeits_and_finishes_two_player_match() {
        let (mut registry, room_id, owner, guest) = started_registry();

        let outcome = registry.leave(owner).unwrap();
        let LeaveBroadcast::GameFinished { snapshot, shot } = outcome.broadcast.unwrap() else {
            panic!("sole opposing player should win");
        };

        assert_eq!(snapshot.phase, Phase::Finished);
        assert_eq!(snapshot.players.len(), 1);
        assert_eq!(snapshot.players[0].id, guest);
        assert!(snapshot.players[0].owner);
        assert_eq!(shot.winner_team, Some(2));
        assert!(shot.path.is_empty());
        assert!(
            shot.game
                .soldiers
                .iter()
                .filter(|soldier| soldier.player_id == owner)
                .all(|soldier| !soldier.alive)
        );
        assert_eq!(
            registry.rooms[&room_id]
                .game
                .as_ref()
                .unwrap()
                .player_ids
                .len(),
            2
        );
    }

    #[test]
    fn disconnect_during_active_match_preserves_membership_and_soldiers() {
        let (mut registry, room_id, _owner, guest) = started_registry();
        let before = registry.member_state(guest).unwrap().1.unwrap();

        assert!(matches!(
            registry.disconnect(guest),
            Err(RoomError::WrongPhase)
        ));

        let after = registry.member_state(guest).unwrap().1.unwrap();
        assert_eq!(after.soldiers, before.soldiers);
        assert!(registry.is_member_of(guest, room_id));
    }

    #[test]
    fn only_current_player_can_fire() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        assert!(matches!(
            registry.fire(guest, "0".into(), 0.0),
            Err(RoomError::NotTurn)
        ));
        assert!(registry.fire(owner, "0".into(), 0.0).is_ok());
    }

    #[test]
    fn fire_rejects_variables_unavailable_in_mode() {
        for (mode, function, allowed) in [
            (GameMode::Function, "y", false),
            (GameMode::Function, "y'", false),
            (GameMode::FirstOrder, "y", true),
            (GameMode::FirstOrder, "y'", false),
            (GameMode::SecondOrder, "y + y'", true),
        ] {
            let owner = Uuid::new_v4();
            let guest = Uuid::new_v4();
            let mut registry = Registry::default();
            let room = registry
                .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
                .unwrap()
                .0;
            registry.join(guest, "Guest".into(), room.id, None).unwrap();
            registry.set_mode(owner, mode).unwrap();
            registry.set_ready(owner, true).unwrap();
            registry.set_ready(guest, true).unwrap();
            registry.start_game(owner).unwrap();
            let result = registry.fire(owner, function.into(), 0.0);
            if allowed {
                assert!(result.is_ok(), "{mode:?} should allow {function}");
            } else {
                assert!(matches!(
                    result,
                    Err(RoomError::Invalid(
                        "function uses variables unavailable in this mode"
                    ))
                ));
            }
        }
    }

    #[test]
    fn alternating_roster_keeps_turn_authority_and_snapshot_ids() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_team(owner, owner, 2).unwrap();
        registry.set_team(guest, guest, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();

        let start = registry.start_game(owner).unwrap();
        assert_eq!(start.game.turn_player_id, Some(guest));
        assert!(matches!(
            registry.fire(owner, "0".into(), 0.0),
            Err(RoomError::NotTurn)
        ));
        assert!(registry.fire(guest, "0".into(), 0.0).is_ok());
        assert!(
            start
                .game
                .soldiers
                .iter()
                .all(|soldier| { [owner, guest].contains(&soldier.player_id) })
        );
    }

    #[test]
    fn expired_turn_advances_authoritatively() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        {
            let room = registry.rooms.get_mut(&room.id).unwrap();
            room.game.as_mut().unwrap().turn_deadline_at = 0;
        }

        let outcomes = registry.expire_turns();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].game.turn_player_id, Some(guest));
        assert!(outcomes[0].game.turn_deadline_at.unwrap() > unix_timestamp());
    }

    #[test]
    fn resolving_leave_forfeits_without_advancing_turn() {
        let (mut registry, room_id, owner, guest) = started_registry();
        registry.fire(owner, "0".into(), 0.0).unwrap();
        let turn = registry.rooms[&room_id].game.as_ref().unwrap().state.turn;

        let outcome = registry.leave(guest).unwrap();
        let LeaveBroadcast::GameFinished { snapshot, shot } = outcome.broadcast.unwrap() else {
            panic!("sole remaining team should win");
        };

        assert_eq!(snapshot.phase, Phase::Finished);
        assert_eq!(shot.winner_team, Some(1));
        assert_eq!(
            registry.rooms[&room_id].game.as_ref().unwrap().state.turn,
            turn
        );
        assert!(
            shot.game
                .soldiers
                .iter()
                .filter(|soldier| soldier.player_id == guest)
                .all(|soldier| !soldier.alive)
        );
    }

    #[test]
    fn bot_slot_is_owner_controlled_and_ready() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let snapshot = registry.add_bot(owner, 4).unwrap();
        let bot = snapshot
            .players
            .iter()
            .find(|player| player.is_bot)
            .unwrap();
        assert!(bot.ready);
        assert!(matches!(
            registry.add_bot(guest, 4),
            Err(RoomError::NotOwner)
        ));
        assert!(matches!(
            registry.remove_bot(guest, bot.id),
            Err(RoomError::NotOwner)
        ));
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        let start = registry.start_game(owner).unwrap();
        assert_eq!(start.game.soldiers.len(), 6);
    }

    #[test]
    fn bot_turn_enters_resolution_and_stale_turn_is_ignored() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        let bot_snapshot = registry.add_bot(owner, 1).unwrap();
        let bot = bot_snapshot
            .players
            .iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;
        registry.set_team(owner, owner, 2).unwrap();
        registry.set_team(owner, bot, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        let start = registry.start_game(owner).unwrap();
        assert_eq!(start.game.turn_player_id, Some(bot));
        let pending = registry.pending_bot_turns();
        assert_eq!(pending.len(), 1);
        let turn = pending[0].clone();
        let result = crate::bot::search(crate::bot::SearchInput {
            mode: turn.mode,
            terrain: &turn.terrain,
            state: &turn.state,
            team: turn.team,
            level: turn.level,
            seed: turn.seed,
            memory: turn.memory.clone(),
            budget: Duration::MAX,
        });
        let outcome = registry.apply_bot_turn(turn, result).unwrap().unwrap();
        assert_eq!(outcome.snapshot.phase, Phase::Resolving);
        assert_eq!(registry.rooms[&room.id].snapshot.phase, Phase::Resolving);
        assert!(registry.fire(bot, "0".into(), 0.0).is_err());
    }

    #[test]
    fn invalid_bot_shot_preserves_memory() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        let snapshot = registry.add_bot(owner, 1).unwrap();
        let bot = snapshot
            .players
            .iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;
        registry.set_team(owner, owner, 2).unwrap();
        registry.set_team(owner, bot, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.start_game(owner).unwrap();
        let turn = registry.pending_bot_turns().pop().unwrap();
        let stored_before = registry.rooms[&room.id].bots[&bot].memory.clone();
        let result = crate::bot::search(crate::bot::SearchInput {
            mode: turn.mode,
            terrain: &turn.terrain,
            state: &turn.state,
            team: turn.team,
            level: turn.level,
            seed: turn.seed,
            memory: turn.memory.clone(),
            budget: Duration::ZERO,
        });

        assert!(matches!(
            registry.apply_bot_turn(turn, result),
            Err(RoomError::Invalid("bot produced no valid shot"))
        ));
        assert_eq!(registry.rooms[&room.id].bots[&bot].memory, stored_before);
    }

    #[test]
    fn failed_bot_search_skips_immediately_and_rejects_stale_result() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        let snapshot = registry.add_bot(owner, 1).unwrap();
        let bot = snapshot
            .players
            .iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;
        registry.set_team(owner, owner, 2).unwrap();
        registry.set_team(owner, bot, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.start_game(owner).unwrap();
        let turn = registry.pending_bot_turns().pop().unwrap();

        let outcome = registry.skip_bot_turn(turn.clone()).unwrap().unwrap();
        assert_eq!(outcome.game.turn_player_id, Some(owner));
        let stored_before = registry.rooms[&room.id].bots[&bot].memory.clone();
        let stale_memory = crate::bot::search(crate::bot::SearchInput {
            mode: turn.mode,
            terrain: &turn.terrain,
            state: &turn.state,
            team: turn.team,
            level: turn.level,
            seed: turn.seed,
            memory: turn.memory.clone(),
            budget: Duration::ZERO,
        })
        .memory;
        assert_eq!(outcome.snapshot.phase, Phase::Planning);
        assert!(outcome.game.turn_deadline_at.unwrap() > unix_timestamp());
        assert!(registry.skip_bot_turn(turn.clone()).unwrap().is_none());
        let result = crate::bot::SearchOutcome {
            shot: Some(("0".into(), 0.0)),
            memory: stale_memory,
        };
        assert!(registry.apply_bot_turn(turn, result).unwrap().is_none());
        assert_eq!(registry.rooms[&room.id].bots[&bot].memory, stored_before);
        let room = &registry.rooms[&room.id];
        assert_eq!(room.snapshot.phase, Phase::Planning);
        assert_eq!(
            room.game.as_ref().unwrap().player_ids[room.game.as_ref().unwrap().state.turn],
            owner
        );
    }

    #[test]
    fn non_finite_shot_does_not_mutate_or_advance_turn() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "Owner".into(), "room".into(), RoomVisibility::Public)
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        let (revision, turn, terrain, state) = {
            let room = registry.rooms.get(&room.id).unwrap();
            let game = room.game.as_ref().unwrap();
            (
                room.snapshot.revision,
                game.state.turn,
                game.terrain.clone(),
                game.state.clone(),
            )
        };

        assert!(matches!(
            registry.fire(owner, "sqrt(-1)".into(), 0.0),
            Err(RoomError::Invalid("function produced no finite trajectory"))
        ));

        let room = registry.rooms.get(&room.id).unwrap();
        let game = room.game.as_ref().unwrap();
        assert_eq!(room.snapshot.revision, revision);
        assert_eq!(game.state.turn, turn);
        assert_eq!(game.terrain, terrain);
        assert_eq!(game.state, state);
    }
}
