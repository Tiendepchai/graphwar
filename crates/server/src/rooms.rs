use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use graphwar_game_core::{
    Circle, Expr, GameState, Player, SeededGenerator, Soldier, Team, Terrain, TrajectoryEnd,
    TrajectoryMissReason, TrajectoryMode,
    constants::{
        MAX_GAME_TERRAIN_CIRCLES, MAX_PLAYERS, MAX_PRACTICE_TERRAIN_CIRCLES,
        MAX_SOLDIERS_PER_PLAYER, PLANE_HEIGHT, PLANE_LENGTH, PRACTICE_TERRAIN_RADII,
        SOLDIER_RADIUS,
    },
    parse, projectile_hits, trace,
};

use graphwar_protocol::{
    ChatEntry, GameMode, GameSnapshot, Phase, PlayerSnapshot, PracticePlayerPlacement,
    PracticeSetup, RoomKind, RoomSnapshot, RoomVisibility, SetupPoint, ShotHistoryEntry,
    ShotMissReason, ShotOutcome, ShotResolved, SoldierPosition, SoldierSnapshot, TerrainCircle,
};
use password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const REGISTRY_FORMAT_VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct PersistedRegistry {
    version: u32,
    rooms: Vec<PersistedRoom>,
}

#[derive(Serialize, Deserialize)]
struct PersistedRoom {
    snapshot: RoomSnapshot,
    invite: Option<String>,
    members: HashMap<Uuid, bool>,
    bots: HashMap<Uuid, PersistedBot>,
    game: Option<PersistedMatch>,
    #[serde(default)]
    practice_setup: Option<PracticeSetup>,
    #[serde(default)]
    event_sequence: u64,
    #[serde(default)]
    chat_history: Vec<ChatEntry>,
    #[serde(default)]
    lobby_deadline_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct PersistedBot {
    level: u8,
    seed: u64,
}

#[derive(Serialize, Deserialize)]
struct PersistedMatch {
    mode: GameMode,
    terrain: Terrain,
    state: GameState,
    player_ids: Vec<Uuid>,
    turn_deadline_at: i64,
    #[serde(default)]
    shot_history: Vec<ShotHistoryEntry>,
}

use uuid::Uuid;

pub type RoomRegistry = std::sync::Arc<tokio::sync::RwLock<Registry>>;

const TURN_DURATION: Duration = Duration::from_secs(60);
const LOBBY_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_SHOT_HISTORY: usize = 40;
const MAX_CHAT_HISTORY: usize = 100;
const MAX_TERRAIN_CUTS: usize = 512;
const MAX_TRAJECTORY_POINTS: usize = 2_048;
const MAX_ROOM_PASSWORD_BYTES: usize = 1024;
const MAX_ROOMS: usize = 256;

#[derive(Clone, Default)]
pub struct Registry {
    rooms: HashMap<Uuid, Room>,
    needs_save_after_load: bool,
}

#[derive(Clone)]
struct Room {
    snapshot: RoomSnapshot,
    invite: Option<String>,
    members: HashMap<Uuid, bool>,
    bots: HashMap<Uuid, BotSpec>,
    game: Option<Match>,
    practice_setup: Option<PracticeSetup>,
    event_sequence: u64,
    chat_history: Vec<ChatEntry>,
    lobby_deadline_at: Option<i64>,
}

#[derive(Clone)]
struct BotSpec {
    level: u8,
    seed: u64,
    memory: crate::bot::SearchMemory,
}

#[derive(Clone)]
struct Match {
    mode: GameMode,
    terrain: Terrain,
    state: GameState,
    player_ids: Vec<Uuid>,
    turn_deadline_at: i64,
    shot_history: Vec<ShotHistoryEntry>,
}

pub struct StartOutcome {
    pub snapshot: RoomSnapshot,
    pub game: GameSnapshot,
}

pub struct FireOutcome {
    pub snapshot: RoomSnapshot,
    pub shot: ShotResolved,
}

pub struct LobbyExpired {
    pub room_id: Uuid,
    pub members: Vec<Uuid>,
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
        chat_history: Vec<ChatEntry>,
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
    #[error("state storage unavailable")]
    Storage,
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
            Self::Storage => "storage_unavailable",
        }
    }
}

impl Registry {
    pub(crate) fn from_persisted_json(input: &str) -> Result<Self, String> {
        let persisted: PersistedRegistry =
            serde_json::from_str(input).map_err(|error| error.to_string())?;
        if !(1..=REGISTRY_FORMAT_VERSION).contains(&persisted.version) {
            return Err(format!(
                "unsupported registry snapshot version {}",
                persisted.version
            ));
        }
        if persisted.rooms.len() > MAX_ROOMS {
            return Err("too many rooms".into());
        }
        let version = persisted.version;
        let now = unix_timestamp();
        let mut needs_save_after_load = false;
        let mut rooms = HashMap::with_capacity(persisted.rooms.len());
        for mut persisted in persisted.rooms {
            let id = persisted.snapshot.id;
            if rooms.contains_key(&id) {
                return Err("duplicate room ID".into());
            }
            normalize_feed_history(&mut persisted)?;
            if normalize_lobby_deadline(&mut persisted, version, now) {
                needs_save_after_load = true;
            }
            validate_persisted_room(&persisted)?;
            let bots = persisted
                .bots
                .into_iter()
                .map(|(id, bot)| {
                    (
                        id,
                        BotSpec {
                            level: bot.level,
                            seed: bot.seed,
                            memory: crate::bot::SearchMemory::default(),
                        },
                    )
                })
                .collect();
            let game = persisted.game.map(|game| Match {
                mode: game.mode,
                terrain: game.terrain,
                state: game.state,
                player_ids: game.player_ids,
                turn_deadline_at: game.turn_deadline_at,
                shot_history: game.shot_history,
            });
            rooms.insert(
                id,
                Room {
                    snapshot: persisted.snapshot,
                    invite: persisted.invite,
                    members: persisted.members,
                    bots,
                    game,
                    practice_setup: persisted.practice_setup,
                    event_sequence: persisted.event_sequence,
                    chat_history: persisted.chat_history,
                    lobby_deadline_at: persisted.lobby_deadline_at,
                },
            );
        }
        Ok(Self {
            rooms,
            needs_save_after_load,
        })
    }

    pub(crate) fn persisted_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&PersistedRegistry {
            version: REGISTRY_FORMAT_VERSION,
            rooms: self
                .rooms
                .values()
                .map(|room| PersistedRoom {
                    snapshot: room.snapshot.clone(),
                    invite: room.invite.clone(),
                    members: room.members.clone(),
                    bots: room
                        .bots
                        .iter()
                        .map(|(id, bot)| {
                            (
                                *id,
                                PersistedBot {
                                    level: bot.level,
                                    seed: bot.seed,
                                },
                            )
                        })
                        .collect(),
                    game: room.game.as_ref().map(|game| PersistedMatch {
                        mode: game.mode,
                        terrain: game.terrain.clone(),
                        state: game.state.clone(),
                        player_ids: game.player_ids.clone(),
                        turn_deadline_at: game.turn_deadline_at,
                        shot_history: game.shot_history.clone(),
                    }),
                    practice_setup: room.practice_setup.clone(),
                    event_sequence: room.event_sequence,
                    chat_history: room.chat_history.clone(),
                    lobby_deadline_at: room.lobby_deadline_at,
                })
                .collect(),
        })
    }

    pub fn resume_after_restart(&mut self) -> bool {
        let mut changed = std::mem::take(&mut self.needs_save_after_load);
        for room in self.rooms.values_mut() {
            let Some(game) = room.game.as_mut() else {
                continue;
            };
            match room.snapshot.phase {
                Phase::Planning => {
                    game.turn_deadline_at = turn_deadline();
                    room.snapshot.revision += 1;
                    changed = true;
                }
                Phase::Resolving => {
                    advance_turn(&mut game.state);
                    game.turn_deadline_at = turn_deadline();
                    room.snapshot.phase = Phase::Planning;
                    room.snapshot.revision += 1;
                    changed = true;
                }
                Phase::Lobby | Phase::Finished => {}
            }
        }
        changed
    }

    pub fn expire_lobbies(&mut self) -> Vec<LobbyExpired> {
        self.expire_lobbies_at(unix_timestamp())
    }

    pub fn expire_lobbies_at(&mut self, now: i64) -> Vec<LobbyExpired> {
        let expired = self
            .rooms
            .iter()
            .filter_map(|(id, room)| {
                (room.snapshot.phase == Phase::Lobby
                    && room
                        .lobby_deadline_at
                        .is_some_and(|deadline| deadline <= now))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        expired
            .into_iter()
            .filter_map(|room_id| {
                let room = self.rooms.remove(&room_id)?;
                Some(LobbyExpired {
                    room_id,
                    members: room
                        .members
                        .keys()
                        .filter(|member| !room.bots.contains_key(member))
                        .copied()
                        .collect(),
                })
            })
            .collect()
    }

    pub fn normalize_lobby_deadlines(&mut self) -> bool {
        let now = unix_timestamp();
        let mut changed = false;
        for room in self.rooms.values_mut() {
            if normalize_room_lobby_deadline(room, now) {
                changed = true;
            }
        }
        changed
    }

    #[cfg(test)]
    pub(crate) fn set_lobby_deadline_at(&mut self, room_id: Uuid, deadline: i64) {
        self.rooms
            .get_mut(&room_id)
            .expect("test room exists")
            .lobby_deadline_at = Some(deadline);
    }

    pub fn create(
        &mut self,
        owner: Uuid,
        display_name: String,
        name: String,
        visibility: RoomVisibility,
        password: Option<String>,
    ) -> Result<(RoomSnapshot, Option<String>), RoomError> {
        self.create_with_kind(
            owner,
            display_name,
            name,
            visibility,
            RoomKind::Standard,
            password,
        )
    }

    pub fn create_with_kind(
        &mut self,
        owner: Uuid,
        display_name: String,
        name: String,
        visibility: RoomVisibility,
        kind: RoomKind,
        password: Option<String>,
    ) -> Result<(RoomSnapshot, Option<String>), RoomError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(RoomError::Invalid("room name must be 1-64 characters"));
        }
        if self.room_id_for(owner).is_some() {
            return Err(RoomError::Invalid("leave the current room first"));
        }
        if self.rooms.len() >= MAX_ROOMS {
            return Err(RoomError::Invalid("room limit reached"));
        }
        let invite = match (visibility, password) {
            (RoomVisibility::Private, Some(password)) => Some(hash_room_password(&password)?),
            (RoomVisibility::Private, None) => {
                return Err(RoomError::Invalid("private room password is required"));
            }
            (RoomVisibility::Public, None) => None,
            (RoomVisibility::Public, Some(_)) => {
                return Err(RoomError::Invalid("public rooms cannot have a password"));
            }
        };
        let id = Uuid::new_v4();
        let snapshot = RoomSnapshot {
            id,
            name: name.into(),
            visibility,
            phase: Phase::Lobby,
            revision: 0,
            mode: GameMode::Function,
            kind,
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
        let practice_setup = (kind == RoomKind::Practice)
            .then(|| reconcile_practice_setup(None, &snapshot.players, id))
            .transpose()?;
        self.rooms.insert(
            id,
            Room {
                snapshot: snapshot.clone(),
                invite,
                members: HashMap::from([(owner, false)]),
                bots: HashMap::new(),
                game: None,
                practice_setup,
                event_sequence: 0,
                chat_history: Vec::new(),
                lobby_deadline_at: Some(lobby_deadline()),
            },
        );
        Ok((snapshot, None))
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
        let legacy_password = if room.snapshot.visibility == RoomVisibility::Private {
            let stored = room.invite.as_deref().ok_or(RoomError::Private)?;
            let submitted = invite
                .filter(|password| {
                    !password.is_empty() && password.len() <= MAX_ROOM_PASSWORD_BYTES
                })
                .ok_or(RoomError::Private)?;
            if !verify_room_password(stored, submitted) {
                return Err(RoomError::Private);
            }
            PasswordHash::new(stored).is_err()
        } else {
            false
        };
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if room.snapshot.players.len() >= MAX_PLAYERS && !room.members.contains_key(&player) {
            return Err(RoomError::Invalid("room is full"));
        }
        if !room.members.contains_key(&player) {
            let mut candidate = room.clone();
            if legacy_password {
                candidate.invite = Some(hash_room_password(invite.expect("verified password"))?);
            }
            candidate.members.insert(player, false);
            candidate.snapshot.players.push(PlayerSnapshot {
                id: player,
                display_name,
                owner: false,
                ready: false,
                team: 2,
                soldiers: 2,
                is_bot: false,
            });
            reconcile_room_practice_setup(&mut candidate)?;
            reset_readiness(&mut candidate);
            reset_lobby_deadline(&mut candidate);
            candidate.snapshot.revision += 1;
            *room = candidate;
        }
        Ok(room.snapshot.clone())
    }

    pub fn leave(&mut self, player: Uuid) -> Result<LeaveOutcome, RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        let phase = self.rooms[&id].snapshot.phase;
        if matches!(phase, Phase::Lobby | Phase::Finished) {
            let room = self.rooms.get_mut(&id).expect("room ID came from registry");
            let mut candidate = room.clone();
            if remove_member(&mut candidate, player) {
                reconcile_room_practice_setup(&mut candidate)?;
                reset_readiness(&mut candidate);
                if phase == Phase::Lobby {
                    reset_lobby_deadline(&mut candidate);
                }
                let broadcast = if phase == Phase::Finished {
                    LeaveBroadcast::StateSync {
                        snapshot: candidate.snapshot.clone(),
                        game: snapshot_for_game(&candidate),
                        chat_history: candidate.chat_history.clone(),
                    }
                } else {
                    LeaveBroadcast::Room(candidate.snapshot.clone())
                };
                *room = candidate;
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(broadcast),
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
            let mut candidate = room.clone();
            let (current, winner_team) = {
                let game = candidate.game.as_mut().ok_or(RoomError::WrongPhase)?;
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
            if !remove_member(&mut candidate, player) {
                self.rooms.remove(&id);
                return Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: None,
                });
            }
            reconcile_room_practice_setup(&mut candidate)?;
            if let Some(winner_team) = winner_team {
                candidate.snapshot.phase = Phase::Finished;
                let game_snapshot = snapshot_for_game(&candidate);
                let snapshot = candidate.snapshot.clone();
                *room = candidate;
                return Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::GameFinished {
                        snapshot,
                        shot: ShotResolved {
                            path: Vec::new(),
                            outcome: ShotOutcome::Forfeit,
                            winner_team: Some(winner_team),
                            game: game_snapshot,
                        },
                    }),
                });
            }
            if phase == Phase::Planning && current {
                let game = candidate.game.as_mut().expect("active game exists");
                advance_turn(&mut game.state);
                game.turn_deadline_at = turn_deadline();
                let game_snapshot = snapshot_for_game(&candidate);
                let snapshot = candidate.snapshot.clone();
                *room = candidate;
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::TurnStarted {
                        snapshot,
                        game: game_snapshot,
                    }),
                })
            } else {
                let game_snapshot = snapshot_for_game(&candidate);
                let snapshot = candidate.snapshot.clone();
                let chat_history = candidate.chat_history.clone();
                *room = candidate;
                Ok(LeaveOutcome {
                    room_id: id,
                    broadcast: Some(LeaveBroadcast::StateSync {
                        snapshot,
                        game: game_snapshot,
                        chat_history,
                    }),
                })
            }
        }
    }

    pub fn return_to_lobby(&mut self, player: Uuid) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Finished {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        room.snapshot.phase = Phase::Lobby;
        room.game = None;
        reset_readiness(room);
        reset_lobby_deadline(room);
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn set_ready(&mut self, player: Uuid, ready: bool) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if room.members[&player] != ready {
            *room.members.get_mut(&player).expect("member room") = ready;
            room.snapshot
                .players
                .iter_mut()
                .find(|member| member.id == player)
                .expect("member room")
                .ready = ready;
            reset_lobby_deadline(room);
            room.snapshot.revision += 1;
        }
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
            reset_lobby_deadline(room);
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
        if player != player_id && !is_owner(room, player) {
            return Err(RoomError::NotSlotOwner);
        }
        if !(1..=2).contains(&team) {
            return Err(RoomError::Invalid("team must be 1 or 2"));
        }
        let current_team = room
            .snapshot
            .players
            .iter()
            .find(|member| member.id == player_id)
            .ok_or(RoomError::NotMember)?
            .team;
        if current_team != team {
            let mut candidate = room.clone();
            candidate
                .snapshot
                .players
                .iter_mut()
                .find(|member| member.id == player_id)
                .expect("validated member")
                .team = team;
            reconcile_room_practice_setup(&mut candidate)?;
            reset_readiness(&mut candidate);
            reset_lobby_deadline(&mut candidate);
            candidate.snapshot.revision += 1;
            *room = candidate;
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
        let current = room
            .snapshot
            .players
            .iter()
            .find(|member| member.id == player_id)
            .ok_or(RoomError::NotMember)?
            .soldiers;
        if current != soldiers {
            let mut candidate = room.clone();
            candidate
                .snapshot
                .players
                .iter_mut()
                .find(|member| member.id == player_id)
                .expect("validated member")
                .soldiers = soldiers;
            reconcile_room_practice_setup(&mut candidate)?;
            reset_readiness(&mut candidate);
            reset_lobby_deadline(&mut candidate);
            candidate.snapshot.revision += 1;
            *room = candidate;
        }
        Ok(room.snapshot.clone())
    }

    pub fn set_practice_setup(
        &mut self,
        player: Uuid,
        base_revision: u64,
        setup: PracticeSetup,
    ) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, player) {
            return Err(RoomError::NotOwner);
        }
        if room.snapshot.kind != RoomKind::Practice {
            return Err(RoomError::Invalid("room is not a practice room"));
        }
        if room.snapshot.revision != base_revision {
            return Err(RoomError::Invalid("room setup is stale"));
        }
        validate_practice_setup(&setup, &room.snapshot.players)?;
        if room.practice_setup.as_ref() != Some(&setup) {
            room.practice_setup = Some(setup);
            reset_readiness(room);
            reset_lobby_deadline(room);
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
        let mut candidate = room.clone();
        candidate.members.insert(id, true);
        candidate.bots.insert(
            id,
            BotSpec {
                level,
                seed: id.as_u128() as u64,
                memory: crate::bot::SearchMemory::default(),
            },
        );
        candidate.snapshot.players.push(PlayerSnapshot {
            id,
            display_name: format!("Bot {}", candidate.bots.len()),
            owner: false,
            ready: true,
            team,
            soldiers: 2,
            is_bot: true,
        });
        reconcile_room_practice_setup(&mut candidate)?;
        reset_readiness(&mut candidate);
        reset_lobby_deadline(&mut candidate);
        candidate.snapshot.revision += 1;
        *room = candidate;
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
        if !room.bots.contains_key(&player_id) {
            return Err(RoomError::Invalid("player is not a bot"));
        }
        let mut candidate = room.clone();
        candidate.bots.remove(&player_id);
        candidate.members.remove(&player_id);
        candidate
            .snapshot
            .players
            .retain(|slot| slot.id != player_id);
        reconcile_room_practice_setup(&mut candidate)?;
        reset_readiness(&mut candidate);
        reset_lobby_deadline(&mut candidate);
        candidate.snapshot.revision += 1;
        *room = candidate;
        Ok(room.snapshot.clone())
    }

    pub fn kick_player(&mut self, owner: Uuid, player_id: Uuid) -> Result<RoomSnapshot, RoomError> {
        let room_id = self.room_id_for(owner).ok_or(RoomError::NotMember)?;
        let room = self.rooms.get_mut(&room_id).expect("owner room");
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if !is_owner(room, owner) {
            return Err(RoomError::NotOwner);
        }
        if owner == player_id {
            return Err(RoomError::Invalid("owner cannot kick themself"));
        }
        if room.bots.contains_key(&player_id) {
            return Err(RoomError::Invalid("use remove bot for bot players"));
        }
        if !room.members.contains_key(&player_id) {
            return Err(RoomError::NotMember);
        }
        let mut candidate = room.clone();
        remove_member(&mut candidate, player_id);
        reconcile_room_practice_setup(&mut candidate)?;
        reset_readiness(&mut candidate);
        reset_lobby_deadline(&mut candidate);
        *room = candidate;
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
        let seed = match_seed(room.snapshot.id);
        let game_state = match room.snapshot.kind {
            RoomKind::Standard => new_match(seed, &room.snapshot.players, room.snapshot.mode)?,
            RoomKind::Practice => new_practice_match(
                seed,
                room.practice_setup
                    .as_ref()
                    .ok_or(RoomError::Invalid("practice setup is missing"))?,
                &room.snapshot.players,
                room.snapshot.mode,
            )?,
        };
        room.snapshot.phase = Phase::Planning;
        room.snapshot.revision += 1;
        room.lobby_deadline_at = None;
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
        let normalized_function = function.trim().to_owned();
        let expr =
            parse(&normalized_function).map_err(|_| RoomError::Invalid("invalid function"))?;
        let (display_name, team) = room
            .snapshot
            .players
            .iter()
            .find(|member| member.id == player)
            .map(|member| (member.display_name.clone(), member.team))
            .ok_or(RoomError::NotMember)?;
        let trajectory = {
            let game = room.game.as_ref().expect("checked above");
            if !mode_allows(&expr, game.mode) {
                return Err(RoomError::Invalid(
                    "function uses variables unavailable in this mode",
                ));
            }
            let mode = trajectory_mode(game.mode, angle_deg);
            let inverted = matches!(game.state.players[game.state.turn].team, Team::Two);
            trace(&expr, mode, &game.terrain, &game.state, inverted)
                .map_err(|_| RoomError::Invalid("function produced no finite trajectory"))?
        };
        if matches!(trajectory.end, TrajectoryEnd::TerrainImpact { .. })
            && room
                .game
                .as_ref()
                .is_some_and(|game| game.terrain.explosions.len() >= MAX_TERRAIN_CUTS)
        {
            return Err(RoomError::Invalid("terrain cut limit reached"));
        }
        let sequence = next_event_sequence(room)?;
        let history_entry = ShotHistoryEntry {
            sequence,
            player_id: player,
            display_name,
            team,
            function: normalized_function,
            angle_deg,
        };
        let projectile_hit_indexes = {
            let game = room.game.as_ref().expect("checked above");
            projectile_hits(&trajectory.points, &game.state)
        };
        let game = room.game.as_mut().expect("checked above");
        append_shot_history(game, history_entry);
        let projectile_casualties = apply_projectile_hits(game, &projectile_hit_indexes);
        let outcome = match trajectory.end {
            TrajectoryEnd::TerrainImpact { point: (x, y) } => {
                let explosion = Circle {
                    x,
                    y,
                    radius: graphwar_game_core::constants::EXPLOSION_RADIUS,
                };
                let mut casualties = projectile_casualties;
                casualties.extend(apply_explosion(game, explosion));
                game.terrain
                    .explode(explosion.x, explosion.y, explosion.radius);
                ShotOutcome::TerrainImpact {
                    explosion: circle_snapshot(explosion),
                    hits: casualties,
                }
            }
            TrajectoryEnd::Miss(reason) => ShotOutcome::Miss {
                reason: shot_miss_reason(reason),
                hits: projectile_casualties,
            },
        };
        let winner_team = winner(&game.state);
        if winner_team.is_some() {
            room.snapshot.phase = Phase::Finished;
        } else {
            room.snapshot.phase = Phase::Resolving;
            game.turn_deadline_at = resolution_deadline();
        }
        room.snapshot.revision += 1;
        let shot = ShotResolved {
            path: downsample_path(trajectory.points, MAX_TRAJECTORY_POINTS),
            outcome,
            winner_team,
            game: snapshot_for_game(room),
        };
        Ok(FireOutcome {
            snapshot: room.snapshot.clone(),
            shot,
        })
    }

    pub fn chat(&mut self, player: Uuid, text: String) -> Result<(Uuid, ChatEntry), RoomError> {
        if text.trim().is_empty() || text.len() > 500 {
            return Err(RoomError::Invalid("chat must be 1-500 characters"));
        }
        let room = self.member_room_mut(player)?;
        let display_name = room
            .snapshot
            .players
            .iter()
            .find(|member| member.id == player)
            .map(|member| member.display_name.clone())
            .ok_or(RoomError::NotMember)?;
        let entry = ChatEntry {
            room_id: room.snapshot.id,
            sequence: next_event_sequence(room)?,
            player_id: player,
            display_name,
            text,
        };
        room.chat_history.push(entry.clone());
        let excess = room.chat_history.len().saturating_sub(MAX_CHAT_HISTORY);
        room.chat_history.drain(..excess);
        if room.snapshot.phase == Phase::Lobby {
            reset_lobby_deadline(room);
        }
        Ok((room.snapshot.id, entry))
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

    pub fn practice_setup(&self, room_id: Uuid) -> Option<PracticeSetup> {
        self.rooms
            .get(&room_id)
            .and_then(|room| room.practice_setup.clone())
    }

    pub fn member_practice_setup(&self, player: Uuid) -> Result<Option<PracticeSetup>, RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        Ok(self.rooms[&id].practice_setup.clone())
    }

    pub fn member_state(
        &self,
        player: Uuid,
    ) -> Result<(RoomSnapshot, Option<GameSnapshot>, Vec<ChatEntry>), RoomError> {
        let id = self.room_id_for(player).ok_or(RoomError::NotMember)?;
        let room = &self.rooms[&id];
        Ok((
            room.snapshot.clone(),
            room.game.as_ref().map(|_| snapshot_for_game(room)),
            room.chat_history.clone(),
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
            .filter(|room| {
                room.snapshot.phase == Phase::Lobby && room.snapshot.players.len() < MAX_PLAYERS
            })
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

fn normalize_feed_history(room: &mut PersistedRoom) -> Result<(), String> {
    let mut used = std::collections::HashSet::new();
    let mut maximum = room.event_sequence;
    for sequence in room
        .chat_history
        .iter()
        .map(|entry| entry.sequence)
        .chain(
            room.game
                .iter()
                .flat_map(|game| game.shot_history.iter().map(|entry| entry.sequence)),
        )
        .filter(|sequence| *sequence != 0)
    {
        if !used.insert(sequence) {
            return Err("duplicate feed sequence".into());
        }
        maximum = maximum.max(sequence);
    }
    if let Some(game) = &mut room.game {
        for shot in &mut game.shot_history {
            if shot.sequence == 0 {
                maximum = maximum
                    .checked_add(1)
                    .ok_or_else(|| "feed sequence exhausted".to_string())?;
                shot.sequence = maximum;
            }
        }
    }
    room.event_sequence = maximum;
    Ok(())
}

fn hash_room_password(password: &str) -> Result<String, RoomError> {
    if password.is_empty() || password.len() > MAX_ROOM_PASSWORD_BYTES {
        return Err(RoomError::Invalid("room password must be 1-1024 bytes"));
    }
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| RoomError::Storage)
}

fn verify_room_password(stored: &str, submitted: &str) -> bool {
    PasswordHash::new(stored).map_or_else(
        |_| Uuid::parse_str(stored).is_ok() && stored == submitted,
        |hash| {
            Argon2::default()
                .verify_password(submitted.as_bytes(), &hash)
                .is_ok()
        },
    )
}

fn valid_persisted_room_secret(secret: &str) -> bool {
    Uuid::parse_str(secret).is_ok()
        || (secret.starts_with("$argon2") && PasswordHash::new(secret).is_ok())
}

fn normalize_lobby_deadline(room: &mut PersistedRoom, version: u32, now: i64) -> bool {
    let before = room.lobby_deadline_at;
    room.lobby_deadline_at = match room.snapshot.phase {
        Phase::Lobby if version < 3 && room.lobby_deadline_at.is_none() => {
            Some(now.saturating_add(LOBBY_INACTIVITY_TIMEOUT.as_secs() as i64))
        }
        Phase::Lobby => room.lobby_deadline_at,
        Phase::Planning | Phase::Resolving | Phase::Finished => None,
    };
    version != REGISTRY_FORMAT_VERSION || before != room.lobby_deadline_at
}

fn normalize_room_lobby_deadline(room: &mut Room, now: i64) -> bool {
    let normalized = match room.snapshot.phase {
        Phase::Lobby => room
            .lobby_deadline_at
            .or_else(|| Some(now.saturating_add(LOBBY_INACTIVITY_TIMEOUT.as_secs() as i64))),
        Phase::Planning | Phase::Resolving | Phase::Finished => None,
    };
    let changed = room.lobby_deadline_at != normalized;
    room.lobby_deadline_at = normalized;
    changed
}

fn validate_persisted_room(room: &PersistedRoom) -> Result<(), String> {
    let snapshot = &room.snapshot;
    if snapshot.name.trim().is_empty()
        || snapshot.name.len() > 64
        || snapshot.players.is_empty()
        || snapshot.revision == u64::MAX
    {
        return Err("invalid room snapshot".into());
    }
    if snapshot.players.len() > MAX_PLAYERS
        || snapshot
            .players
            .iter()
            .filter(|player| player.owner)
            .count()
            != 1
        || snapshot.players.iter().any(|player| {
            !(2..=32).contains(&player.display_name.chars().count())
                || !(1..=2).contains(&player.team)
                || player.soldiers == 0
                || usize::from(player.soldiers) > MAX_SOLDIERS_PER_PLAYER
        })
    {
        return Err("invalid room players".into());
    }
    let player_ids = snapshot
        .players
        .iter()
        .map(|player| player.id)
        .collect::<std::collections::HashSet<_>>();
    if player_ids.len() != snapshot.players.len()
        || room.members.len() != player_ids.len()
        || room.members.keys().any(|id| !player_ids.contains(id))
        || room.bots.keys().any(|id| !player_ids.contains(id))
        || snapshot
            .players
            .iter()
            .any(|player| player.is_bot != room.bots.contains_key(&player.id))
        || room.bots.values().any(|bot| !(1..=8).contains(&bot.level))
    {
        return Err("inconsistent room membership".into());
    }
    match (snapshot.visibility, room.invite.as_deref()) {
        (RoomVisibility::Private, Some(secret)) if valid_persisted_room_secret(secret) => {}
        (RoomVisibility::Private, _) => return Err("private room has invalid password data".into()),
        (RoomVisibility::Public, None) => {}
        (RoomVisibility::Public, Some(_)) => return Err("public room has password data".into()),
    }
    match (snapshot.kind, room.practice_setup.as_ref()) {
        (RoomKind::Standard, None) => {}
        (RoomKind::Practice, Some(setup)) => {
            validate_practice_setup(setup, &snapshot.players).map_err(|error| error.to_string())?
        }
        _ => return Err("inconsistent practice setup".into()),
    }
    if room.chat_history.len() > MAX_CHAT_HISTORY
        || room.chat_history.iter().any(|entry| {
            entry.room_id != snapshot.id
                || entry.sequence == 0
                || entry.sequence > room.event_sequence
                || !(2..=32).contains(&entry.display_name.chars().count())
                || entry.text.trim().is_empty()
                || entry.text.len() > 500
        })
    {
        return Err("invalid chat history".into());
    }
    if room.lobby_deadline_at.is_some() != (snapshot.phase == Phase::Lobby) {
        return Err("invalid lobby deadline".into());
    }
    match (&snapshot.phase, &room.game) {
        (Phase::Lobby, None) => return Ok(()),
        (Phase::Planning | Phase::Resolving | Phase::Finished, Some(game)) => {
            let slots = game
                .player_ids
                .iter()
                .copied()
                .enumerate()
                .map(|(index, id)| (id, index))
                .collect::<HashMap<_, _>>();
            let finished = snapshot.phase == Phase::Finished;
            if game.mode != snapshot.mode
                || game.state.players.len() != game.player_ids.len()
                || game.state.turn >= game.state.players.len()
                || slots.len() != game.player_ids.len()
                || (winner(&game.state).is_some() != finished)
                || snapshot.players.iter().any(|player| {
                    slots.get(&player.id).is_none_or(|index| {
                        let slot = &game.state.players[*index];
                        slot.team != team(player.team)
                            || slot.soldiers.len() != usize::from(player.soldiers)
                    })
                })
                || (!finished
                    && game.player_ids.iter().enumerate().any(|(index, id)| {
                        !player_ids.contains(id)
                            && game.state.players[index]
                                .soldiers
                                .iter()
                                .any(|soldier| soldier.alive)
                    }))
                || game.terrain.circles.len() > MAX_GAME_TERRAIN_CIRCLES
                || (!finished && game.terrain.explosions.len() > MAX_TERRAIN_CUTS)
                || game.shot_history.len() > MAX_SHOT_HISTORY
                || game.shot_history.iter().any(|shot| {
                    shot.sequence == 0
                        || shot.sequence > room.event_sequence
                        || !(1..=2).contains(&shot.team)
                        || !(2..=32).contains(&shot.display_name.chars().count())
                        || shot.function.is_empty()
                        || shot.function.len() > 256
                        || !shot.angle_deg.is_finite()
                })
            {
                return Err("invalid match state".into());
            }
            if game
                .terrain
                .circles
                .iter()
                .chain(&game.terrain.explosions)
                .any(|circle| {
                    !circle.x.is_finite()
                        || !circle.y.is_finite()
                        || !circle.radius.is_finite()
                        || circle.radius <= 0.0
                })
                || game.state.players.iter().any(|player| {
                    player.current_soldier >= player.soldiers.len()
                        || player.soldiers.is_empty()
                        || player.soldiers.len() > MAX_SOLDIERS_PER_PLAYER
                        || player.soldiers.iter().any(|soldier| {
                            !soldier.x.is_finite()
                                || !soldier.y.is_finite()
                                || soldier.x - SOLDIER_RADIUS < 0.0
                                || soldier.x + SOLDIER_RADIUS >= f64::from(PLANE_LENGTH)
                                || soldier.y - SOLDIER_RADIUS < 0.0
                                || soldier.y + SOLDIER_RADIUS >= f64::from(PLANE_HEIGHT)
                        })
                })
            {
                return Err("invalid terrain or soldiers".into());
            }
        }
        _ => return Err("inconsistent room phase".into()),
    }
    Ok(())
}

fn new_match(seed: u64, players: &[PlayerSnapshot], mode: GameMode) -> Result<Match, RoomError> {
    let mut generator = SeededGenerator::new(seed);
    let terrain = Terrain::new(generator.terrain());
    let slots = alternating_players(players, seed);
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
        shot_history: Vec::new(),
    })
}

fn new_practice_match(
    seed: u64,
    setup: &PracticeSetup,
    players: &[PlayerSnapshot],
    mode: GameMode,
) -> Result<Match, RoomError> {
    validate_practice_setup(setup, players)?;
    let terrain = Terrain::new(
        setup
            .terrain
            .iter()
            .map(|circle| Circle {
                x: circle.x,
                y: circle.y,
                radius: circle.radius,
            })
            .collect(),
    );
    let placements = setup
        .players
        .iter()
        .map(|placement| (placement.player_id, &placement.soldiers))
        .collect::<HashMap<_, _>>();
    let slots = alternating_players(players, seed);
    let player_ids = slots.iter().map(|player| player.id).collect();
    let game_players = slots
        .into_iter()
        .enumerate()
        .map(|(index, player)| {
            let soldiers = placements[&player.id]
                .iter()
                .map(|point| Soldier::new(point.x, point.y))
                .collect();
            Player::new(index as u32, team(player.team), soldiers)
        })
        .collect();
    Ok(Match {
        mode,
        terrain,
        state: GameState::new(game_players),
        player_ids,
        turn_deadline_at: turn_deadline(),
        shot_history: Vec::new(),
    })
}

fn validate_practice_setup(
    setup: &PracticeSetup,
    players: &[PlayerSnapshot],
) -> Result<(), RoomError> {
    if setup.terrain.len() > MAX_PRACTICE_TERRAIN_CIRCLES {
        return Err(RoomError::Invalid("practice terrain has too many circles"));
    }
    if setup.terrain.iter().any(|circle| {
        !circle.x.is_finite()
            || !circle.y.is_finite()
            || !PRACTICE_TERRAIN_RADII.contains(&circle.radius)
            || circle.x - circle.radius < 0.0
            || circle.x + circle.radius > f64::from(PLANE_LENGTH)
            || circle.y - circle.radius < 0.0
            || circle.y + circle.radius > f64::from(PLANE_HEIGHT)
    }) {
        return Err(RoomError::Invalid("practice terrain circle is invalid"));
    }
    if setup.players.len() != players.len() {
        return Err(RoomError::Invalid(
            "practice placements do not match roster",
        ));
    }
    let terrain = Terrain::new(
        setup
            .terrain
            .iter()
            .map(|circle| Circle {
                x: circle.x,
                y: circle.y,
                radius: circle.radius,
            })
            .collect(),
    );
    let roster = players
        .iter()
        .map(|player| (player.id, usize::from(player.soldiers)))
        .collect::<HashMap<_, _>>();
    let mut seen = std::collections::HashSet::with_capacity(players.len());
    let mut placed = Vec::new();
    for placement in &setup.players {
        let Some(expected) = roster.get(&placement.player_id) else {
            return Err(RoomError::Invalid("practice placement has unknown player"));
        };
        if !seen.insert(placement.player_id) || placement.soldiers.len() != *expected {
            return Err(RoomError::Invalid(
                "practice placements do not match roster",
            ));
        }
        for point in &placement.soldiers {
            if !point.x.is_finite()
                || !point.y.is_finite()
                || point.x - SOLDIER_RADIUS < 0.0
                || point.x + SOLDIER_RADIUS >= f64::from(PLANE_LENGTH)
                || point.y - SOLDIER_RADIUS < 0.0
                || point.y + SOLDIER_RADIUS >= f64::from(PLANE_HEIGHT)
                || terrain.collides_circle(point.x, point.y, SOLDIER_RADIUS)
                || placed.iter().any(|other: &SetupPoint| {
                    (other.x - point.x).abs() < 20.0 && (other.y - point.y).abs() < 20.0
                })
            {
                return Err(RoomError::Invalid("practice soldier position is invalid"));
            }
            placed.push(point.clone());
        }
    }
    if seen.len() != players.len() {
        return Err(RoomError::Invalid(
            "practice placements do not match roster",
        ));
    }
    Ok(())
}

fn reconcile_practice_setup(
    setup: Option<&PracticeSetup>,
    players: &[PlayerSnapshot],
    seed: Uuid,
) -> Result<PracticeSetup, RoomError> {
    let terrain = setup.map_or_else(Vec::new, |setup| setup.terrain.clone());
    let old = setup
        .into_iter()
        .flat_map(|setup| &setup.players)
        .map(|placement| (placement.player_id, &placement.soldiers))
        .collect::<HashMap<_, _>>();
    let terrain_model = Terrain::new(
        terrain
            .iter()
            .map(|circle| Circle {
                x: circle.x,
                y: circle.y,
                radius: circle.radius,
            })
            .collect(),
    );
    let mut placed = Vec::<SetupPoint>::new();
    let mut reconciled = Vec::with_capacity(players.len());
    for (player_index, player) in players.iter().enumerate() {
        let mut soldiers = Vec::with_capacity(usize::from(player.soldiers));
        if let Some(existing) = old.get(&player.id) {
            for point in existing.iter().take(usize::from(player.soldiers)) {
                if !terrain_model.collides_circle(point.x, point.y, SOLDIER_RADIUS)
                    && placed.iter().all(|other| {
                        (other.x - point.x).abs() >= 20.0 || (other.y - point.y).abs() >= 20.0
                    })
                {
                    soldiers.push((*point).clone());
                    placed.push((*point).clone());
                }
            }
        }
        while soldiers.len() < usize::from(player.soldiers) {
            let point = default_practice_point(
                &terrain_model,
                player.team,
                player_index,
                soldiers.len(),
                seed,
                &placed,
            )?;
            placed.push(point.clone());
            soldiers.push(point);
        }
        reconciled.push(PracticePlayerPlacement {
            player_id: player.id,
            soldiers,
        });
    }
    let setup = PracticeSetup {
        terrain,
        players: reconciled,
    };
    validate_practice_setup(&setup, players)?;
    Ok(setup)
}

fn default_practice_point(
    terrain: &Terrain,
    team: u8,
    player_index: usize,
    soldier_index: usize,
    seed: Uuid,
    placed: &[SetupPoint],
) -> Result<SetupPoint, RoomError> {
    let x_start = SOLDIER_RADIUS as i32;
    let x_end = PLANE_LENGTH / 2 - SOLDIER_RADIUS as i32;
    let y_start = SOLDIER_RADIUS as i32;
    let y_end = PLANE_HEIGHT - SOLDIER_RADIUS as i32;
    let x_span = (x_end - x_start) as u64;
    let y_span = (y_end - y_start) as u64;
    let seed = seed.as_u128() as u64
        ^ (player_index as u64).wrapping_mul(MAX_SOLDIERS_PER_PLAYER as u64)
        ^ soldier_index as u64;
    for attempt in 0..10_000_u64 {
        let x = x_start
            + (seed.wrapping_mul(97).wrapping_add(attempt.wrapping_mul(52)) % x_span) as i32;
        let y = y_start
            + (seed
                .wrapping_mul(193)
                .wrapping_add(attempt.wrapping_mul(89))
                % y_span) as i32;
        let x = if team == 1 {
            f64::from(x)
        } else {
            f64::from(PLANE_LENGTH - 1 - x)
        };
        let y = f64::from(y);
        if !terrain.collides_circle(x, y, SOLDIER_RADIUS)
            && placed
                .iter()
                .all(|point| (point.x - x).abs() >= 20.0 || (point.y - y).abs() >= 20.0)
        {
            return Ok(SetupPoint { x, y });
        }
    }
    Err(RoomError::Invalid("could not place practice soldiers"))
}

fn alternating_players(players: &[PlayerSnapshot], seed: u64) -> Vec<&PlayerSnapshot> {
    // Interleave teams, pick the opener from the match seed.
    // Equal teams no longer always give team 1 first shot.
    let mut team_one: Vec<&PlayerSnapshot> =
        players.iter().filter(|player| player.team == 1).collect();
    let mut team_two: Vec<&PlayerSnapshot> =
        players.iter().filter(|player| player.team == 2).collect();
    let mut last_team = None;
    let mut result = Vec::with_capacity(players.len());
    while !team_one.is_empty() || !team_two.is_empty() {
        let take_one = if team_one.is_empty() {
            false
        } else if team_two.is_empty() {
            true
        } else if last_team == Some(1) {
            false
        } else if last_team == Some(2) {
            true
        } else if team_one.len() == team_two.len() {
            seed % 2 == 0
        } else if team_one.len() > team_two.len() {
            seed % 3 != 0
        } else {
            seed % 3 == 0
        };
        if take_one {
            result.push(team_one.remove(0));
            last_team = Some(1);
        } else {
            result.push(team_two.remove(0));
            last_team = Some(2);
        }
    }
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

#[cfg(not(test))]
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

#[cfg(test)]
fn match_seed(room_id: Uuid) -> u64 {
    room_id.as_u128() as u64 & !1
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

fn reconcile_room_practice_setup(room: &mut Room) -> Result<(), RoomError> {
    if room.snapshot.kind == RoomKind::Practice {
        room.practice_setup = Some(reconcile_practice_setup(
            room.practice_setup.as_ref(),
            &room.snapshot.players,
            room.snapshot.id,
        )?);
    }
    Ok(())
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

fn reset_lobby_deadline(room: &mut Room) {
    if room.snapshot.phase == Phase::Lobby {
        room.lobby_deadline_at = Some(lobby_deadline());
    }
}

fn next_event_sequence(room: &mut Room) -> Result<u64, RoomError> {
    room.event_sequence = room
        .event_sequence
        .checked_add(1)
        .ok_or(RoomError::Invalid("feed sequence exhausted"))?;
    Ok(room.event_sequence)
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

fn lobby_deadline() -> i64 {
    unix_timestamp().saturating_add(LOBBY_INACTIVITY_TIMEOUT.as_secs() as i64)
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

fn append_shot_history(game: &mut Match, entry: ShotHistoryEntry) {
    game.shot_history.push(entry);
    let excess = game.shot_history.len().saturating_sub(MAX_SHOT_HISTORY);
    game.shot_history.drain(..excess);
}

fn downsample_path(points: Vec<(f64, f64)>, limit: usize) -> Vec<(f64, f64)> {
    if points.len() <= limit {
        return points;
    }
    let last = points.len() - 1;
    (0..limit)
        .map(|index| points[index * last / (limit - 1)])
        .collect()
}

fn apply_projectile_hits(game: &mut Match, hits: &[(usize, usize)]) -> Vec<SoldierSnapshot> {
    let mut casualties = Vec::new();
    for &(player_index, soldier_index) in hits {
        if let Some(soldier) = game
            .state
            .players
            .get_mut(player_index)
            .and_then(|player| player.soldiers.get_mut(soldier_index))
            && soldier.alive
        {
            soldier.alive = false;
            casualties.push((player_index, soldier_index));
        }
    }
    casualties
        .into_iter()
        .filter_map(|(player, soldier)| soldier_snapshot(game, player, soldier))
        .collect()
}

fn apply_explosion(game: &mut Match, explosion: Circle) -> Vec<SoldierSnapshot> {
    let hit_radius = explosion.radius + SOLDIER_RADIUS;
    let hit_radius_squared = hit_radius * hit_radius;
    let mut casualties = Vec::new();
    for (player_index, player) in game.state.players.iter_mut().enumerate() {
        for (soldier_index, soldier) in player.soldiers.iter_mut().enumerate() {
            let distance_squared = (soldier.x - explosion.x).mul_add(
                soldier.x - explosion.x,
                (soldier.y - explosion.y) * (soldier.y - explosion.y),
            );
            if soldier.alive && distance_squared <= hit_radius_squared {
                soldier.alive = false;
                casualties.push((player_index, soldier_index));
            }
        }
    }
    casualties
        .into_iter()
        .filter_map(|(player, soldier)| soldier_snapshot(game, player, soldier))
        .collect()
}

fn shot_miss_reason(reason: TrajectoryMissReason) -> ShotMissReason {
    match reason {
        TrajectoryMissReason::WorldExit => ShotMissReason::WorldExit,
        TrajectoryMissReason::Numerical => ShotMissReason::Numerical,
        TrajectoryMissReason::StepLimit => ShotMissReason::StepLimit,
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
    // Turns strictly alternate between teams. Within the target team, rotate
    // through the living players so no teammate is starved: each team keeps a
    // cursor into the roster.
    let current_team = game.players[current].team;
    let current_team_idx = match current_team {
        Team::One => 0,
        Team::Two => 1,
    };
    let target_team_idx = 1 - current_team_idx;
    let target_team = if target_team_idx == 0 {
        Team::One
    } else {
        Team::Two
    };
    let roster_len = game.players.len();
    let mut cursor = game.team_turn[target_team_idx];
    for _ in 0..roster_len {
        let candidate = cursor % roster_len;
        let candidate_team = game.players[candidate].team;
        let next_cursor = cursor.wrapping_add(1);
        if candidate_team == target_team && game.players[candidate].living().next().is_some() {
            game.team_turn[target_team_idx] = next_cursor;
            let player = &mut game.players[candidate];
            if player.current().is_none_or(|soldier| !soldier.alive) {
                let index = player
                    .living()
                    .next()
                    .map(|(index, _)| index)
                    .expect("candidate has a living soldier");
                player.current_soldier = index;
            }
            game.turn = candidate;
            return;
        }
        cursor = next_cursor;
    }
    // No living player on the opposite team left.
}

fn snapshot_for_game(room: &Room) -> GameSnapshot {
    let game = room.game.as_ref().expect("game snapshot requires game");
    let turn_player_id = game.player_ids.get(game.state.turn).copied();
    GameSnapshot {
        room_id: room.snapshot.id,
        revision: room.snapshot.revision,
        mode: game.mode,
        winner_team: (room.snapshot.phase == Phase::Finished)
            .then(|| winner(&game.state))
            .flatten(),
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
        shot_history: game.shot_history.clone(),
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
    fn advance_turn_selects_a_living_soldier() {
        let mut dead = Soldier::new(3.0, 4.0);
        dead.alive = false;
        let mut game = GameState::new(vec![
            Player::new(1, Team::One, vec![Soldier::new(1.0, 2.0)]),
            Player::new(2, Team::Two, vec![dead, Soldier::new(5.0, 6.0)]),
        ]);

        advance_turn(&mut game);

        assert_eq!(game.turn, 1);
        assert_eq!(game.players[1].current_soldier, 1);
        assert!(game.players[1].current().unwrap().alive);
    }

    #[test]
    fn explosion_uses_soldier_radius_hitbox() {
        let mut game = Match {
            mode: GameMode::Function,
            terrain: Terrain::default(),
            state: GameState::new(vec![Player::new(
                1,
                Team::One,
                vec![
                    Soldier::new(100.0 + 12.0 + SOLDIER_RADIUS - 0.1, 100.0),
                    Soldier::new(100.0, 100.0 + 12.0 + SOLDIER_RADIUS + 0.1),
                ],
            )]),
            player_ids: vec![Uuid::new_v4()],
            turn_deadline_at: 0,
            shot_history: Vec::new(),
        };

        let hits = apply_explosion(
            &mut game,
            Circle {
                x: 100.0,
                y: 100.0,
                radius: 12.0,
            },
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 0);
        assert!(!hits[0].alive);
        assert!(!game.state.players[0].soldiers[0].alive);
        assert!(game.state.players[0].soldiers[1].alive);
    }

    #[test]
    fn projectile_passing_through_soldier_damages_target() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_soldiers(owner, owner, 1).unwrap();
        registry.set_soldiers(guest, guest, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        let game = registry
            .rooms
            .get_mut(&room.id)
            .unwrap()
            .game
            .as_mut()
            .unwrap();
        game.terrain = Terrain::default();
        game.state.players[0].soldiers[0] = Soldier::new(100.0, 225.0);
        game.state.players[1].soldiers[0] = Soldier::new(300.0, 225.0);

        let shot = registry.fire(owner, "0".into(), 0.0).unwrap().shot;

        let ShotOutcome::Miss {
            reason: ShotMissReason::WorldExit,
            hits,
        } = &shot.outcome
        else {
            panic!("expected world-exit miss");
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].player_id, guest);
        assert!(!hits[0].alive);
        assert_eq!(shot.winner_team, Some(1));
        let guest_soldier = shot
            .game
            .soldiers
            .iter()
            .find(|soldier| soldier.player_id == guest)
            .unwrap();
        assert!(!guest_soldier.alive);
    }

    #[test]
    fn terrain_impact_explosion_damages_overlapping_soldier() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        registry.set_soldiers(owner, owner, 1).unwrap();
        registry.set_soldiers(guest, guest, 1).unwrap();
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        let game = registry
            .rooms
            .get_mut(&room.id)
            .unwrap()
            .game
            .as_mut()
            .unwrap();
        game.terrain = Terrain {
            circles: vec![Circle {
                x: 158.0,
                y: 225.0,
                radius: 20.0,
            }],
            explosions: Vec::new(),
        };
        game.state.players[0].soldiers[0] = Soldier::new(100.0, 225.0);
        game.state.players[1].soldiers[0] = Soldier::new(138.0, 243.0);

        let shot = registry.fire(owner, "0".into(), 0.0).unwrap().shot;

        let ShotOutcome::TerrainImpact { explosion, hits } = shot.outcome else {
            panic!("expected terrain impact");
        };
        assert!((explosion.x - 138.0).hypot(explosion.y - 243.0) <= 12.0 + SOLDIER_RADIUS);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].player_id, guest);
        assert!(!hits[0].alive);
        assert_eq!(shot.winner_team, Some(1));
        let guest_soldier = shot
            .game
            .soldiers
            .iter()
            .find(|soldier| soldier.player_id == guest)
            .unwrap();
        assert!(!guest_soldier.alive);
    }

    #[test]
    fn advance_turn_strictly_alternates_teams() {
        // Two players on team 1, one on team 2 (3-player roster). Turns must
        // never land on the same team twice in a row.
        let mut team_one = Player::new(1, Team::One, vec![Soldier::new(1.0, 2.0)]);
        team_one.current_soldier = 0;
        let team_two = Player::new(2, Team::Two, vec![Soldier::new(3.0, 4.0)]);
        let mut team_one_b = Player::new(3, Team::One, vec![Soldier::new(5.0, 6.0)]);
        team_one_b.current_soldier = 0;
        let mut game = GameState::new(vec![team_one, team_two, team_one_b]);

        let mut sequence = Vec::new();
        for _ in 0..6 {
            let team = game.players[game.turn].team;
            sequence.push(team);
            advance_turn(&mut game);
        }
        assert!(
            sequence.windows(2).all(|w| w[0] != w[1]),
            "turns must strictly alternate teams, got {sequence:?}"
        );
        assert_eq!(
            sequence,
            vec![
                Team::One,
                Team::Two,
                Team::One,
                Team::Two,
                Team::One,
                Team::Two
            ]
        );
    }

    #[test]
    fn advance_turn_rotates_through_teammates() {
        // Two teams of two. After several shots every player must have had a
        // turn — the per-team cursor must not starve a teammate.
        let mk = |id: u32, team: Team| Player::new(id, team, vec![Soldier::new(1.0, 2.0)]);
        // Roster: players[0]=T1, [1]=T2, [2]=T1, [3]=T2.
        let mut game = GameState::new(vec![
            mk(1, Team::One),
            mk(2, Team::Two),
            mk(3, Team::One),
            mk(4, Team::Two),
        ]);
        let mut sequence = Vec::new();
        for _ in 0..8 {
            let id = game.players[game.turn].id;
            let team = game.players[game.turn].team;
            sequence.push((id, team));
            advance_turn(&mut game);
        }
        let mut seen: Vec<u32> = sequence.iter().map(|(id, _)| *id).collect();
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.contains(&1) && seen.contains(&2) && seen.contains(&3) && seen.contains(&4),
            "every player must get a turn, got {sequence:?}"
        );
        assert!(
            sequence.windows(2).all(|w| w[0].1 != w[1].1),
            "turns must alternate teams, got {sequence:?}"
        );
        // Expected pattern: T1,T2,T1,T2,... starting at players[0] (team 1).
        assert_eq!(
            sequence.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
            vec![
                Team::One,
                Team::Two,
                Team::One,
                Team::Two,
                Team::One,
                Team::Two,
                Team::One,
                Team::Two
            ]
        );
    }

    #[test]
    fn lobby_inactivity_expiry_removes_room_members_only_at_deadline() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        let bot = registry.add_bot(owner, 1).unwrap().players[1].id;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let deadline = registry.rooms[&room.id].lobby_deadline_at.unwrap();

        assert!(registry.expire_lobbies_at(deadline - 1).is_empty());
        let expired = registry.expire_lobbies_at(deadline);

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].room_id, room.id);
        assert!(expired[0].members.contains(&owner));
        assert!(expired[0].members.contains(&guest));
        assert!(!expired[0].members.contains(&bot));
        assert!(matches!(
            registry.member_state(owner),
            Err(RoomError::NotMember)
        ));
        assert!(matches!(
            registry.member_state(guest),
            Err(RoomError::NotMember)
        ));
        assert!(registry.public_snapshots().is_empty());
    }

    #[test]
    fn lobby_activity_resets_deadline_and_game_phases_clear_it() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.set_lobby_deadline_at(room.id, 1);
        assert_eq!(
            registry.set_ready(owner, false).unwrap().revision,
            room.revision
        );
        assert_eq!(registry.rooms[&room.id].lobby_deadline_at, Some(1));

        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let joined_deadline = registry.rooms[&room.id].lobby_deadline_at.unwrap();
        assert!(joined_deadline > 1);
        registry.set_lobby_deadline_at(room.id, 1);
        registry.set_ready(owner, true).unwrap();
        assert!(registry.rooms[&room.id].lobby_deadline_at.unwrap() > 1);
        registry.set_lobby_deadline_at(room.id, 1);
        registry.set_soldiers(owner, owner, 1).unwrap();
        assert!(registry.rooms[&room.id].lobby_deadline_at.unwrap() > 1);
        registry.set_lobby_deadline_at(room.id, 1);
        registry.chat(owner, "ready".into()).unwrap();
        assert!(registry.rooms[&room.id].lobby_deadline_at.unwrap() > 1);
        registry.set_lobby_deadline_at(room.id, 1);
        assert!(matches!(
            registry.chat(owner, "   ".into()),
            Err(RoomError::Invalid(_))
        ));
        assert_eq!(registry.rooms[&room.id].lobby_deadline_at, Some(1));

        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        registry.start_game(owner).unwrap();
        assert_eq!(registry.rooms[&room.id].lobby_deadline_at, None);
        assert!(registry.expire_lobbies_at(i64::MAX).is_empty());
        registry.rooms.get_mut(&room.id).unwrap().snapshot.phase = Phase::Finished;
        registry.return_to_lobby(owner).unwrap();
        assert!(registry.rooms[&room.id].lobby_deadline_at.unwrap() > 1);
    }

    #[test]
    fn persisted_lobby_deadlines_normalize_by_version() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.set_lobby_deadline_at(room.id, 42);
        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        assert_eq!(restored.rooms[&room.id].lobby_deadline_at, Some(42));

        let mut legacy: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        legacy["version"] = 2.into();
        legacy["rooms"][0]
            .as_object_mut()
            .unwrap()
            .remove("lobby_deadline_at");
        let restored = Registry::from_persisted_json(&legacy.to_string()).unwrap();
        assert!(restored.rooms[&room.id].lobby_deadline_at.unwrap() > unix_timestamp());

        let (active, active_room, active_owner, _) = started_registry();
        let active_restored =
            Registry::from_persisted_json(&active.persisted_json().unwrap()).unwrap();
        assert_eq!(active_restored.rooms[&active_room].lobby_deadline_at, None);
        assert!(
            active_restored
                .member_state(active_owner)
                .unwrap()
                .1
                .is_some()
        );
    }

    #[test]
    fn room_limit_rejects_creation_and_corrupt_persistence() {
        let mut registry = Registry::default();
        for index in 0..MAX_ROOMS {
            registry
                .create(
                    Uuid::new_v4(),
                    "Owner".into(),
                    format!("room-{index}"),
                    RoomVisibility::Public,
                    None,
                )
                .unwrap();
        }
        assert!(matches!(
            registry.create(
                Uuid::new_v4(),
                "Owner".into(),
                "overflow".into(),
                RoomVisibility::Public,
                None,
            ),
            Err(RoomError::Invalid("room limit reached"))
        ));
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        let extra = json["rooms"][0].clone();
        json["rooms"].as_array_mut().unwrap().push(extra);
        assert!(Registry::from_persisted_json(&json.to_string()).is_err());
    }

    #[test]
    fn owner_only_starts_ready_two_player_game() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
    fn persisted_finished_match_allows_legacy_cut_overflow() {
        let (mut registry, room_id, _owner, _guest) = started_registry();
        let room = registry.rooms.get_mut(&room_id).unwrap();
        room.snapshot.phase = Phase::Finished;
        let game = room.game.as_mut().unwrap();
        for soldier in &mut game.state.players[0].soldiers {
            soldier.alive = false;
        }
        game.terrain.explosions = vec![
            Circle {
                x: 1.0,
                y: 1.0,
                radius: 1.0
            };
            MAX_TERRAIN_CUTS + 1
        ];

        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();

        assert_eq!(restored.rooms[&room_id].snapshot.phase, Phase::Finished);
        assert_eq!(
            restored.rooms[&room_id]
                .game
                .as_ref()
                .unwrap()
                .terrain
                .explosions
                .len(),
            MAX_TERRAIN_CUTS + 1
        );
    }

    #[test]
    fn persisted_active_match_round_trips_and_resets_bot_memory() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Private,
                Some("room password".into()),
            )
            .unwrap()
            .0;
        let bot = registry
            .add_bot(owner, 2)
            .unwrap()
            .players
            .into_iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;
        registry.set_ready(owner, true).unwrap();
        registry.start_game(owner).unwrap();
        let memory = {
            let game = registry.rooms[&room.id].game.as_ref().unwrap();
            crate::bot::search(crate::bot::SearchInput {
                mode: game.mode,
                terrain: &game.terrain,
                state: &game.state,
                team: Team::Two,
                level: 2,
                seed: 0,
                memory: crate::bot::SearchMemory::default(),
                budget: Duration::ZERO,
            })
            .memory
        };
        assert_ne!(memory, crate::bot::SearchMemory::default());
        registry
            .rooms
            .get_mut(&room.id)
            .unwrap()
            .bots
            .get_mut(&bot)
            .unwrap()
            .memory = memory;

        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        let (restored_snapshot, restored_game, restored_chat) =
            restored.member_state(owner).unwrap();
        let (snapshot, game, chat) = registry.member_state(owner).unwrap();
        assert_eq!(restored_chat, chat);
        assert_eq!(restored_snapshot, snapshot);
        let (Some(restored_game), Some(game)) = (restored_game, game) else {
            panic!("active match missing after restore");
        };
        assert_eq!(restored_game.room_id, game.room_id);
        assert_eq!(restored_game.revision, game.revision);
        assert_eq!(restored_game.mode, game.mode);
        assert_eq!(restored_game.turn_player_id, game.turn_player_id);
        assert_eq!(restored_game.shot_history, game.shot_history);
        assert_eq!(restored_game.soldiers.len(), game.soldiers.len());
        assert!(restored_game.soldiers.iter().zip(game.soldiers.iter()).all(
            |(restored, original)| {
                restored.player_id == original.player_id
                    && restored.index == original.index
                    && restored.team == original.team
                    && restored.alive == original.alive
                    && restored.active == original.active
                    && (restored.x - original.x).abs() < 1e-9
                    && (restored.y - original.y).abs() < 1e-9
            }
        ));
        assert_eq!(restored_game.terrain.len(), game.terrain.len());
        assert!(restored_game.terrain.iter().zip(game.terrain.iter()).all(
            |(restored, original)| {
                (restored.x - original.x).abs() < 1e-9
                    && (restored.y - original.y).abs() < 1e-9
                    && (restored.radius - original.radius).abs() < 1e-9
            }
        ));
        assert_eq!(restored_game.terrain_cuts, game.terrain_cuts);
        assert_eq!(restored.rooms[&room.id].bots[&bot].level, 2);
        assert_eq!(
            restored.rooms[&room.id].bots[&bot].memory,
            crate::bot::SearchMemory::default()
        );
        assert_eq!(
            restored.rooms[&room.id].bots[&bot].memory,
            crate::bot::SearchMemory::default()
        );
    }

    #[test]
    fn restart_keeps_planning_turn_and_settles_resolving_once() {
        let (mut registry, room_id, owner, guest) = started_registry();
        let before = registry
            .member_state(owner)
            .unwrap()
            .1
            .unwrap()
            .turn_player_id;
        assert!(registry.resume_after_restart());
        let resumed = registry.member_state(owner).unwrap().1.unwrap();
        assert_eq!(resumed.turn_player_id, before);
        assert!(resumed.turn_deadline_at.unwrap() > unix_timestamp());

        registry.fire(owner, "0".into(), 0.0).unwrap();
        assert!(registry.resume_after_restart());
        let resolved = registry.member_state(guest).unwrap().1.unwrap();
        assert_eq!(resolved.turn_player_id, Some(guest));
        assert_eq!(registry.rooms[&room_id].snapshot.phase, Phase::Planning);
    }

    #[test]
    fn corrupt_persisted_registry_is_rejected() {
        assert!(Registry::from_persisted_json(r#"{"version":1,"rooms":[{"snapshot":{"id":"00000000-0000-0000-0000-000000000000","name":"","visibility":"public","phase":"lobby","revision":0,"mode":"function","players":[]},"invite":null,"members":{},"bots":{},"game":null}]}"#).is_err());
    }

    #[test]
    fn protected_rooms_hash_passwords_list_and_enforce_credentials() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let password = "correct horse battery staple";
        let (room, returned_secret) = registry
            .create(
                owner,
                "Owner".into(),
                "protected".into(),
                RoomVisibility::Private,
                Some(password.into()),
            )
            .unwrap();

        assert!(returned_secret.is_none());
        assert_eq!(registry.public_snapshots(), std::slice::from_ref(&room));
        let stored = registry.rooms[&room.id].invite.as_deref().unwrap();
        assert_ne!(stored, password);
        assert!(stored.starts_with("$argon2"));
        assert!(!registry.persisted_json().unwrap().contains(password));
        assert!(matches!(
            registry.join(guest, "Guest".into(), room.id, None),
            Err(RoomError::Private)
        ));
        assert!(matches!(
            registry.join(guest, "Guest".into(), room.id, Some("wrong")),
            Err(RoomError::Private)
        ));
        assert!(matches!(
            registry.join(
                guest,
                "Guest".into(),
                room.id,
                Some(&"x".repeat(MAX_ROOM_PASSWORD_BYTES + 1)),
            ),
            Err(RoomError::Private)
        ));
        assert_eq!(
            registry
                .join(guest, "Guest".into(), room.id, Some(password))
                .unwrap()
                .players
                .len(),
            2
        );
    }

    #[test]
    fn room_password_validation_keeps_public_rooms_passwordless() {
        let mut registry = Registry::default();
        let owner = Uuid::new_v4();
        assert!(matches!(
            registry.create(
                owner,
                "Owner".into(),
                "protected".into(),
                RoomVisibility::Private,
                None,
            ),
            Err(RoomError::Invalid("private room password is required"))
        ));
        assert!(matches!(
            registry.create(
                owner,
                "Owner".into(),
                "protected".into(),
                RoomVisibility::Private,
                Some(String::new()),
            ),
            Err(RoomError::Invalid("room password must be 1-1024 bytes"))
        ));
        assert!(matches!(
            registry.create(
                owner,
                "Owner".into(),
                "protected".into(),
                RoomVisibility::Private,
                Some("é".repeat(513)),
            ),
            Err(RoomError::Invalid("room password must be 1-1024 bytes"))
        ));
        assert!(matches!(
            registry.create(
                owner,
                "Owner".into(),
                "public".into(),
                RoomVisibility::Public,
                Some("unused".into()),
            ),
            Err(RoomError::Invalid("public rooms cannot have a password"))
        ));

        let room = registry
            .create(
                owner,
                "Owner".into(),
                "public".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        assert!(
            registry
                .join(Uuid::new_v4(), "Guest".into(), room.id, Some("ignored"),)
                .is_ok()
        );
        assert!(registry.rooms[&room.id].invite.is_none());
    }

    #[test]
    fn legacy_private_credentials_upgrade_after_successful_join() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let legacy = Uuid::new_v4().to_string();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "legacy".into(),
                RoomVisibility::Private,
                Some("temporary".into()),
            )
            .unwrap()
            .0;
        registry.rooms.get_mut(&room.id).unwrap().invite = Some(legacy.clone());

        let mut restored =
            Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        assert!(
            restored
                .join(guest, "Guest".into(), room.id, Some(&legacy))
                .is_ok()
        );
        let upgraded = restored.rooms[&room.id].invite.as_deref().unwrap();
        assert!(upgraded.starts_with("$argon2"));
        assert!(verify_room_password(upgraded, &legacy));
        assert!(!restored.persisted_json().unwrap().contains(&legacy));
    }

    #[test]
    fn malformed_persisted_private_password_is_rejected() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        registry
            .create(
                owner,
                "Owner".into(),
                "protected".into(),
                RoomVisibility::Private,
                Some("password".into()),
            )
            .unwrap();
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        json["rooms"][0]["invite"] = serde_json::Value::String("malformed".into());
        assert!(Registry::from_persisted_json(&json.to_string()).is_err());
    }

    #[test]
    fn practice_setup_is_authoritative_and_persists() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create_with_kind(
                owner,
                "Owner".into(),
                "practice".into(),
                RoomVisibility::Public,
                RoomKind::Practice,
                None,
            )
            .unwrap()
            .0;
        assert!(registry.practice_setup(room.id).unwrap().terrain.is_empty());
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let mut setup = registry.practice_setup(room.id).unwrap();
        let terrain = (20..=750)
            .step_by(20)
            .flat_map(|x| {
                (20..=430)
                    .step_by(20)
                    .map(move |y| (f64::from(x), f64::from(y)))
            })
            .find(|(x, y)| {
                setup.players.iter().all(|placement| {
                    placement
                        .soldiers
                        .iter()
                        .all(|point| (point.x - x).hypot(point.y - y) > 20.0 + SOLDIER_RADIUS)
                })
            })
            .expect("blank practice map has room for terrain");
        setup.terrain.push(TerrainCircle {
            x: terrain.0,
            y: terrain.1,
            radius: 20.0,
        });
        let revision = registry.member_snapshot(owner).unwrap().revision;

        assert!(matches!(
            registry.set_practice_setup(guest, revision, setup.clone()),
            Err(RoomError::NotOwner)
        ));
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        let snapshot = registry
            .set_practice_setup(owner, revision + 2, setup.clone())
            .unwrap();
        assert!(snapshot.players.iter().all(|player| !player.ready));
        assert!(matches!(
            registry.set_practice_setup(owner, revision, setup.clone()),
            Err(RoomError::Invalid("room setup is stale"))
        ));

        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        let started = registry.start_game(owner).unwrap();
        assert_eq!(started.game.terrain, setup.terrain);
        for placement in &setup.players {
            for (index, point) in placement.soldiers.iter().enumerate() {
                let soldier = started
                    .game
                    .soldiers
                    .iter()
                    .find(|soldier| {
                        soldier.player_id == placement.player_id && soldier.index == index
                    })
                    .unwrap();
                assert_eq!((soldier.x, soldier.y), (point.x, point.y));
            }
        }

        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        assert_eq!(restored.practice_setup(room.id), Some(setup));
    }

    #[test]
    fn practice_setup_validation_rejects_invalid_geometry() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create_with_kind(
                owner,
                "Owner".into(),
                "practice".into(),
                RoomVisibility::Public,
                RoomKind::Practice,
                None,
            )
            .unwrap()
            .0;
        let original = registry.practice_setup(room.id).unwrap();
        let revision = room.revision;

        for mutation in [
            |setup: &mut PracticeSetup| {
                setup.terrain.push(TerrainCircle {
                    x: 10.0,
                    y: 10.0,
                    radius: 20.0,
                })
            },
            |setup: &mut PracticeSetup| {
                setup.terrain.push(TerrainCircle {
                    x: 100.0,
                    y: 100.0,
                    radius: 21.0,
                })
            },
            |setup: &mut PracticeSetup| setup.players[0].soldiers[0].x = f64::NAN,
            |setup: &mut PracticeSetup| setup.players[0].soldiers[0].x = SOLDIER_RADIUS - 0.1,
            |setup: &mut PracticeSetup| {
                setup.players[0].soldiers[0].x = f64::from(PLANE_LENGTH) - SOLDIER_RADIUS
            },
            |setup: &mut PracticeSetup| setup.players[0].soldiers[0].y = SOLDIER_RADIUS - 0.1,
            |setup: &mut PracticeSetup| {
                setup.players[0].soldiers[0].y = f64::from(PLANE_HEIGHT) - SOLDIER_RADIUS
            },
            |setup: &mut PracticeSetup| {
                setup.players[0].soldiers[1] = setup.players[0].soldiers[0].clone()
            },
        ] {
            let mut invalid = original.clone();
            mutation(&mut invalid);
            assert!(matches!(
                registry.set_practice_setup(owner, revision, invalid),
                Err(RoomError::Invalid(_))
            ));
            assert_eq!(registry.practice_setup(room.id), Some(original.clone()));
        }
    }

    #[test]
    fn version_one_registry_defaults_to_standard_rooms() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "standard".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        json["version"] = 1.into();
        json["rooms"][0]["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        json["rooms"][0]
            .as_object_mut()
            .unwrap()
            .remove("practice_setup");

        let restored = Registry::from_persisted_json(&json.to_string()).unwrap();
        assert_eq!(
            restored.member_snapshot(owner).unwrap().kind,
            RoomKind::Standard
        );
        assert_eq!(restored.practice_setup(room.id), None);
    }

    #[test]
    fn corrupt_persisted_practice_setup_is_rejected() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        registry
            .create_with_kind(
                owner,
                "Owner".into(),
                "practice".into(),
                RoomVisibility::Public,
                RoomKind::Practice,
                None,
            )
            .unwrap();
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        json["rooms"][0]["practice_setup"]["terrain"] = serde_json::json!([{
            "x": 10.0,
            "y": 10.0,
            "radius": 20.0
        }]);
        assert!(Registry::from_persisted_json(&json.to_string()).is_err());
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
        let first = new_match(124, &players, GameMode::Function).unwrap();
        let second = new_match(124, &players, GameMode::Function).unwrap();

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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let bot = registry
            .add_bot(owner, 1)
            .unwrap()
            .players
            .into_iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();

        assert!(matches!(
            registry.set_mode(guest, GameMode::SecondOrder),
            Err(RoomError::NotOwner)
        ));
        let snapshot = registry.set_team(owner, guest, 1).unwrap();
        assert_eq!(
            snapshot
                .players
                .iter()
                .find(|player| player.id == guest)
                .unwrap()
                .team,
            1
        );
        assert!(
            snapshot
                .players
                .iter()
                .all(|player| !player.ready || player.is_bot)
        );

        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        let snapshot = registry.set_team(owner, bot, 2).unwrap();
        assert_eq!(
            snapshot
                .players
                .iter()
                .find(|player| player.id == bot)
                .unwrap()
                .team,
            2
        );
        assert!(
            snapshot
                .players
                .iter()
                .all(|player| !player.ready || player.is_bot)
        );

        let snapshot = registry.set_team(guest, guest, 2).unwrap();
        assert_eq!(
            snapshot
                .players
                .iter()
                .find(|player| player.id == guest)
                .unwrap()
                .team,
            2
        );
        let revision = snapshot.revision;
        assert_eq!(
            registry.set_team(guest, guest, 2).unwrap().revision,
            revision
        );
        assert!(matches!(
            registry.set_team(guest, owner, 2),
            Err(RoomError::NotSlotOwner)
        ));
        assert!(matches!(
            registry.set_team(guest, bot, 1),
            Err(RoomError::NotSlotOwner)
        ));
        assert!(matches!(
            registry.set_team(guest, guest, 3),
            Err(RoomError::Invalid("team must be 1 or 2"))
        ));
    }

    #[test]
    fn team_changes_are_lobby_only() {
        let (mut registry, _room_id, owner, guest) = started_registry();
        assert!(matches!(
            registry.set_team(owner, guest, 1),
            Err(RoomError::WrongPhase)
        ));
    }

    #[test]
    fn lobby_kick_removes_human_and_enforces_authority() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;
        registry.join(guest, "Guest".into(), room.id, None).unwrap();
        let third = Uuid::new_v4();
        registry.join(third, "Third".into(), room.id, None).unwrap();
        let bot = registry
            .add_bot(owner, 1)
            .unwrap()
            .players
            .into_iter()
            .find(|player| player.is_bot)
            .unwrap()
            .id;

        assert!(matches!(
            registry.kick_player(guest, third),
            Err(RoomError::NotOwner)
        ));
        assert!(matches!(
            registry.kick_player(owner, owner),
            Err(RoomError::Invalid("owner cannot kick themself"))
        ));
        assert!(matches!(
            registry.kick_player(owner, bot),
            Err(RoomError::Invalid("use remove bot for bot players"))
        ));

        let snapshot = registry.kick_player(owner, guest).unwrap();
        assert!(snapshot.players.iter().all(|player| player.id != guest));
        assert!(matches!(
            registry.member_state(guest),
            Err(RoomError::NotMember)
        ));
        assert!(
            registry
                .member_state(owner)
                .unwrap()
                .0
                .players
                .iter()
                .any(|player| player.id == owner)
        );
    }

    #[test]
    fn kick_is_lobby_only() {
        let (mut registry, _room_id, owner, guest) = started_registry();
        assert!(matches!(
            registry.kick_player(owner, guest),
            Err(RoomError::WrongPhase)
        ));
    }

    #[test]
    fn selected_setup_drives_match_and_valid_spawns() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
        let LeaveBroadcast::StateSync { snapshot, game, .. } = outcome.broadcast.unwrap() else {
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
        let LeaveBroadcast::StateSync { snapshot, game, .. } = outcome.broadcast.unwrap() else {
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
    fn finished_room_returns_to_lobby_only_by_owner() {
        let (mut registry, room_id, owner, guest) = started_registry();
        registry.leave(guest).unwrap();
        let setup_before = registry.practice_setup(room_id);

        assert!(matches!(
            registry.return_to_lobby(guest),
            Err(RoomError::NotMember)
        ));
        let snapshot = registry.return_to_lobby(owner).unwrap();

        assert_eq!(snapshot.phase, Phase::Lobby);
        assert!(registry.rooms[&room_id].game.is_none());
        assert_eq!(registry.practice_setup(room_id), setup_before);
        assert!(snapshot.players.iter().all(|player| !player.ready));
    }

    #[test]
    fn finished_member_leave_round_trips_final_result() {
        for leaver_team in [1, 2] {
            let (mut registry, room_id, owner, guest) = started_registry();
            {
                let room = registry.rooms.get_mut(&room_id).unwrap();
                let game = room.game.as_mut().unwrap();
                for player in &mut game.state.players {
                    if player.team == Team::Two {
                        for soldier in &mut player.soldiers {
                            soldier.alive = false;
                        }
                    }
                }
                room.snapshot.phase = Phase::Finished;
                room.snapshot.revision += 1;
            }
            let leaver = if leaver_team == 1 { owner } else { guest };
            let remaining = if leaver == owner { guest } else { owner };
            registry.leave(leaver).unwrap();

            let mut restored =
                Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
            let (snapshot, game, _) = restored.member_state(remaining).unwrap();
            let game = game.unwrap();
            assert_eq!(snapshot.phase, Phase::Finished);
            assert_eq!(game.winner_team, Some(1));
            assert!(snapshot.players.iter().all(|player| player.id != leaver));
            assert!(game.soldiers.iter().any(|soldier| {
                soldier.player_id == leaver && soldier.alive == (leaver_team == 1)
            }));

            let lobby = restored.return_to_lobby(remaining).unwrap();
            assert_eq!(lobby.phase, Phase::Lobby);
            assert!(restored.rooms[&room_id].game.is_none());
            assert!(lobby.players.iter().all(|player| !player.ready));
        }
    }

    #[test]
    fn no_op_ready_keeps_revision() {
        let owner = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
            .unwrap()
            .0;

        let snapshot = registry.set_ready(owner, false).unwrap();

        assert_eq!(snapshot.revision, room.revision);
    }

    #[test]
    fn downsample_path_preserves_endpoints_and_bound() {
        let points = (0..10).map(|index| (index as f64, 0.0)).collect();
        let sampled = downsample_path(points, 4);
        assert_eq!(
            sampled,
            vec![(0.0, 0.0), (3.0, 0.0), (6.0, 0.0), (9.0, 0.0)]
        );
    }

    #[test]
    fn only_current_player_can_fire() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
    fn accepted_shots_are_authoritative_and_bounded() {
        let (mut registry, room_id, owner, _) = started_registry();
        let outcome = registry.fire(owner, " sin(x) ".into(), 0.0).unwrap();
        assert_eq!(outcome.shot.game.shot_history.len(), 1);
        assert_eq!(outcome.shot.game.shot_history[0].display_name, "Owner");
        assert_eq!(outcome.shot.game.shot_history[0].function, "sin(x)");

        let game = registry
            .rooms
            .get_mut(&room_id)
            .unwrap()
            .game
            .as_mut()
            .unwrap();
        for index in 0..MAX_SHOT_HISTORY {
            append_shot_history(
                game,
                ShotHistoryEntry {
                    sequence: index as u64 + 2,
                    player_id: owner,
                    display_name: "Owner".into(),
                    team: 1,
                    function: format!("x+{index}"),
                    angle_deg: 0.0,
                },
            );
        }
        assert_eq!(game.shot_history.len(), MAX_SHOT_HISTORY);
        assert_eq!(game.shot_history[0].function, "x+0");
    }

    #[test]
    fn terrain_cut_limit_preserves_every_existing_hole() {
        let (mut registry, room_id, owner, _) = started_registry();
        {
            let game = registry
                .rooms
                .get_mut(&room_id)
                .unwrap()
                .game
                .as_mut()
                .unwrap();
            let turn = game.state.turn;
            let shooter = game.state.players[turn].current_mut().unwrap();
            shooter.x = 100.0;
            shooter.y = 225.0;
            game.terrain = Terrain::new(vec![
                Circle {
                    x: 150.0,
                    y: 225.0,
                    radius: 20.0,
                },
                Circle {
                    x: 700.0,
                    y: 400.0,
                    radius: 20.0,
                },
            ]);
            game.terrain.explosions = vec![
                Circle {
                    x: 700.0,
                    y: 400.0,
                    radius: 5.0,
                };
                MAX_TERRAIN_CUTS - 1
            ];
        }

        let accepted = registry.fire(owner, "0".into(), 0.0).unwrap();
        assert!(matches!(
            accepted.shot.outcome,
            ShotOutcome::TerrainImpact { .. }
        ));
        let room = registry.rooms.get_mut(&room_id).unwrap();
        assert_eq!(
            room.game.as_ref().unwrap().terrain.explosions.len(),
            MAX_TERRAIN_CUTS
        );
        assert!(
            !room
                .game
                .as_ref()
                .unwrap()
                .terrain
                .collides_point(700.0, 400.0)
        );
        room.snapshot.phase = Phase::Planning;
        room.game.as_mut().unwrap().turn_deadline_at = turn_deadline();
        let revision = room.snapshot.revision;
        let event_sequence = room.event_sequence;
        let game_before = room.game.as_ref().unwrap();
        let terrain_before = game_before.terrain.clone();
        let state_before = game_before.state.clone();
        let history_before = game_before.shot_history.clone();

        assert!(matches!(
            registry.fire(owner, "0".into(), 0.0),
            Err(RoomError::Invalid("terrain cut limit reached"))
        ));
        let room = &registry.rooms[&room_id];
        let game = room.game.as_ref().unwrap();
        assert_eq!(room.snapshot.revision, revision);
        assert_eq!(room.event_sequence, event_sequence);
        assert_eq!(game.terrain, terrain_before);
        assert_eq!(game.state, state_before);
        assert_eq!(game.shot_history, history_before);
        assert!(!game.terrain.collides_point(700.0, 400.0));

        let persisted = registry.persisted_json().unwrap();
        let restored = Registry::from_persisted_json(&persisted).unwrap();
        assert_eq!(
            restored.rooms[&room_id]
                .game
                .as_ref()
                .unwrap()
                .terrain
                .explosions,
            game.terrain.explosions
        );
        registry
            .rooms
            .get_mut(&room_id)
            .unwrap()
            .game
            .as_mut()
            .unwrap()
            .terrain
            .explosions
            .push(Circle {
                x: 650.0,
                y: 400.0,
                radius: 5.0,
            });
        assert!(Registry::from_persisted_json(&registry.persisted_json().unwrap()).is_err());
    }

    #[test]
    fn invalid_shots_do_not_enter_history() {
        let (mut registry, room_id, owner, _) = started_registry();
        assert!(registry.fire(owner, "x+".into(), 0.0).is_err());
        assert_eq!(registry.rooms[&room_id].event_sequence, 0);
        assert!(
            registry.rooms[&room_id]
                .game
                .as_ref()
                .unwrap()
                .shot_history
                .is_empty()
        );
    }

    #[test]
    fn chat_and_shot_share_monotonic_sequence() {
        let (mut registry, room_id, owner, guest) = started_registry();
        let (_, before) = registry.chat(owner, "before".into()).unwrap();
        let shot = registry.fire(owner, "sin(x)".into(), 0.0).unwrap();
        let (_, after) = registry.chat(guest, "after".into()).unwrap();
        assert_eq!(
            (
                before.sequence,
                shot.shot.game.shot_history[0].sequence,
                after.sequence
            ),
            (1, 2, 3)
        );
        assert_eq!(registry.rooms[&room_id].event_sequence, 3);
        assert_eq!(registry.member_state(owner).unwrap().2, vec![before, after]);
    }

    #[test]
    fn invalid_chat_does_not_consume_sequence() {
        let (mut registry, room_id, owner, _) = started_registry();
        assert!(matches!(
            registry.chat(owner, "   ".into()),
            Err(RoomError::Invalid(_))
        ));
        assert_eq!(registry.rooms[&room_id].event_sequence, 0);
        let (_, entry) = registry.chat(owner, "accepted".into()).unwrap();
        assert_eq!(entry.sequence, 1);
    }

    #[test]
    fn departed_player_name_remains_in_history() {
        let (mut registry, _room_id, owner, guest) = started_registry();
        let (_, entry) = registry.chat(owner, "hello".into()).unwrap();
        registry.leave(owner).unwrap();
        let history = registry.member_state(guest).unwrap().2;
        assert_eq!(history, vec![entry]);
        assert_eq!(history[0].display_name, "Owner");
    }

    #[test]
    fn persisted_active_match_without_legacy_shot_history_is_accepted() {
        let (registry, _room_id, owner, _) = started_registry();
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        json["rooms"][0]["game"]
            .as_object_mut()
            .unwrap()
            .remove("shot_history");
        let restored = Registry::from_persisted_json(&json.to_string()).unwrap();
        assert!(
            restored
                .member_state(owner)
                .unwrap()
                .1
                .unwrap()
                .shot_history
                .is_empty()
        );
    }

    #[test]
    fn persisted_chat_with_wrong_room_id_is_rejected() {
        let (mut registry, _room_id, owner, _) = started_registry();
        registry.chat(owner, "chat".into()).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_str(&registry.persisted_json().unwrap()).unwrap();
        json["rooms"][0]["chat_history"][0]["room_id"] =
            serde_json::Value::String(Uuid::new_v4().to_string());
        assert!(Registry::from_persisted_json(&json.to_string()).is_err());
    }

    #[test]
    fn persisted_legacy_zero_shot_sequence_is_normalized() {
        let (mut registry, room_id, owner, _) = started_registry();
        registry.fire(owner, "sin(x)".into(), 0.0).unwrap();
        registry.rooms.get_mut(&room_id).unwrap().event_sequence = 0;
        registry
            .rooms
            .get_mut(&room_id)
            .unwrap()
            .game
            .as_mut()
            .unwrap()
            .shot_history[0]
            .sequence = 0;
        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        let room = &restored.rooms[&room_id];
        assert_eq!(room.event_sequence, 1);
        assert_eq!(room.game.as_ref().unwrap().shot_history[0].sequence, 1);
    }

    #[test]
    fn persisted_duplicate_feed_sequence_is_rejected() {
        let (mut registry, room_id, owner, _) = started_registry();
        registry.chat(owner, "chat".into()).unwrap();
        registry.fire(owner, "sin(x)".into(), 0.0).unwrap();
        registry
            .rooms
            .get_mut(&room_id)
            .unwrap()
            .game
            .as_mut()
            .unwrap()
            .shot_history[0]
            .sequence = 1;
        assert!(Registry::from_persisted_json(&registry.persisted_json().unwrap()).is_err());
    }

    #[test]
    fn chat_history_is_bounded_and_persisted() {
        let (mut registry, _room_id, owner, _) = started_registry();
        for index in 0..(MAX_CHAT_HISTORY + 1) {
            registry.chat(owner, format!("chat-{index}")).unwrap();
        }
        let history = registry.member_state(owner).unwrap().2;
        assert_eq!(history.len(), MAX_CHAT_HISTORY);
        assert_eq!(history[0].text, "chat-1");
        let restored = Registry::from_persisted_json(&registry.persisted_json().unwrap()).unwrap();
        assert_eq!(restored.member_state(owner).unwrap().2, history);
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
                .create(
                    owner,
                    "Owner".into(),
                    "room".into(),
                    RoomVisibility::Public,
                    None,
                )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
    fn grouped_snapshot_still_alternates_teams() {
        // Snapshot order is join order and can list all of one team before
        // the other. Turns must still alternate between teams.
        let shots: Vec<PlayerSnapshot> = vec![
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "A".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "B".into(),
                owner: false,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "C".into(),
                owner: false,
                ready: true,
                team: 2,
                soldiers: 2,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "D".into(),
                owner: false,
                ready: true,
                team: 2,
                soldiers: 2,
                is_bot: false,
            },
        ];
        let order = alternating_players(&shots, 124);
        let teams: Vec<u8> = order.iter().map(|p| p.team).collect();
        assert_eq!(teams, vec![1, 2, 1, 2], "turns must alternate across teams");
        assert!(order.iter().all(|p| shots.contains(p)));
    }

    #[test]
    fn equal_teams_can_start_from_either_side() {
        let players = [
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "A".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 1,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "B".into(),
                owner: false,
                ready: true,
                team: 2,
                soldiers: 1,
                is_bot: false,
            },
        ];

        assert_eq!(alternating_players(&players, 2)[0].team, 1);
        assert_eq!(alternating_players(&players, 1)[0].team, 2);
    }

    #[test]
    fn odd_grouped_snapshot_does_not_start_with_same_team_twice() {
        let players = vec![
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "A".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "B".into(),
                owner: false,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            },
            PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "C".into(),
                owner: false,
                ready: true,
                team: 2,
                soldiers: 2,
                is_bot: false,
            },
        ];

        let order = alternating_players(&players, 1);
        let teams = order.iter().map(|player| player.team).collect::<Vec<_>>();

        assert_eq!(teams, vec![1, 2, 1]);
    }

    #[test]
    fn expired_turn_advances_authoritatively() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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
    fn numerical_shot_is_a_miss_without_damage_or_terrain_mutation() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(
                owner,
                "Owner".into(),
                "room".into(),
                RoomVisibility::Public,
                None,
            )
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

        let outcome = registry.fire(owner, "sqrt(-1)".into(), 0.0).unwrap();
        assert!(matches!(
            outcome.shot.outcome,
            ShotOutcome::Miss {
                reason: ShotMissReason::Numerical,
                hits
            } if hits.is_empty()
        ));
        assert_eq!(outcome.snapshot.phase, Phase::Resolving);

        let room = registry.rooms.get(&room.id).unwrap();
        let game = room.game.as_ref().unwrap();
        assert_eq!(room.snapshot.revision, revision + 1);
        assert_eq!(game.state.turn, turn);
        assert_eq!(game.terrain, terrain);
        assert_eq!(game.state, state);
        assert_eq!(game.shot_history.len(), 1);
    }
}
