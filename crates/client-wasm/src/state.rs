use graphwar_protocol::{
    GameMode, GameSnapshot, Phase, PlayerSnapshot, RoomSnapshot, ServerMessage,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Connecting,
    Online,
    Reconnecting {
        attempt: u32,
    },
    Offline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Login,
    Lobby,
    Room,
    Game,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomSummary {
    pub id: String,
    pub name: String,
    pub players: u16,
    pub capacity: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerSummary {
    pub id: String,
    pub name: String,
    pub owner: bool,
    pub ready: bool,
    pub team: u8,
    pub soldiers: u8,
    pub is_bot: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoldierView {
    pub player_id: String,
    pub index: usize,
    pub x: f64,
    pub y: f64,
    pub team: u8,
    pub alive: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerrainView {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub cut: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitView {
    pub player_id: String,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExplosionView {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatView {
    pub player_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub screen: Screen,
    pub connection: Connection,
    pub player_id: Option<String>,
    pub player_name: String,
    pub room_id: Option<String>,
    pub room_revision: Option<u64>,
    pub room_name: String,
    pub room_phase: Option<Phase>,
    pub game_mode: Option<GameMode>,
    pub rooms: Vec<RoomSummary>,
    pub players: Vec<PlayerSummary>,
    pub soldiers: Vec<SoldierView>,
    pub terrain: Vec<TerrainView>,
    pub authoritative_path: Vec<(f64, f64)>,
    pub preview_path: Vec<(f64, f64)>,
    pub shot_hits: Vec<HitView>,
    pub shot_explosion: Option<ExplosionView>,
    pub shot_sequence: u64,
    pub pending_game: Option<GameSnapshot>,
    pub draft_function: String,
    pub aim_angle_deg: f64,
    pub turn_player_id: Option<String>,
    pub turn_deadline_at: Option<i64>,
    pub chat: Vec<ChatView>,
    pub notices: Vec<String>,
}

impl Model {
    pub fn local_ready(&self) -> bool {
        self.player_id.as_deref().is_some_and(|id| {
            self.players
                .iter()
                .any(|player| player.id == id && player.ready)
        })
    }

    pub fn local_owner(&self) -> bool {
        self.player_id.as_deref().is_some_and(|id| {
            self.players
                .iter()
                .any(|player| player.id == id && player.owner)
        })
    }

    pub fn can_start(&self) -> bool {
        self.local_owner()
            && self.players.len() >= 2
            && self.players.iter().all(|player| player.ready)
            && self.players.iter().any(|player| player.team == 1)
            && self.players.iter().any(|player| player.team == 2)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Connecting,
    Connected,
    Disconnected {
        attempt: u32,
    },
    GiveUp,
    LoggedOut,
    SessionExpired,
    Authenticated {
        player_id: String,
        display_name: String,
    },
    Message(Box<ServerMessage>),
    LeftRoom,
}

pub fn reduce(model: &mut Model, action: Action) {
    match action {
        Action::Connecting => model.connection = Connection::Connecting,
        Action::Connected => model.connection = Connection::Online,
        Action::Disconnected { attempt } => {
            model.connection = Connection::Reconnecting { attempt };
        }
        Action::GiveUp => model.connection = Connection::Offline,
        Action::LoggedOut | Action::SessionExpired => *model = Model::default(),
        Action::Authenticated {
            player_id,
            display_name,
        } => {
            model.player_id = Some(player_id);
            model.player_name = display_name;
            model.screen = Screen::Lobby;
        }
        Action::LeftRoom => leave_room(model),
        Action::Message(message) => match *message {
            ServerMessage::Hello { .. } => {}
            ServerMessage::RoomCreated { snapshot, invite } => {
                let room_id = snapshot.id;
                if apply_room(model, snapshot)
                    && let Some(invite) = invite
                {
                    model
                        .notices
                        .push(format!("Private room: {room_id} · invite: {invite}"));
                    trim_notices(&mut model.notices);
                }
            }
            ServerMessage::Room { snapshot } => {
                apply_room(model, snapshot);
            }
            ServerMessage::RoomList { rooms } => {
                model.rooms = rooms.iter().map(room_summary).collect();
            }
            ServerMessage::GameStarted { snapshot, game }
            | ServerMessage::TurnStarted { snapshot, game } => {
                if apply_room(model, snapshot) && apply_game(model, game) {
                    model.pending_game = None;
                    model.authoritative_path.clear();
                    model.preview_path.clear();
                    model.shot_hits.clear();
                    model.shot_explosion = None;
                }
            }
            ServerMessage::ShotResolved { snapshot, shot } => {
                if apply_room(model, snapshot) {
                    apply_shot(model, shot);
                }
            }
            ServerMessage::GameFinished { snapshot, shot } => {
                if apply_room(model, snapshot) {
                    apply_shot(model, shot);
                }
            }
            ServerMessage::StateSync { snapshot, game } => {
                if apply_room(model, snapshot)
                    && let Some(game) = game
                {
                    if model.pending_game.is_some() && !model.authoritative_path.is_empty() {
                        if game_matches_model(model, &game) {
                            model.pending_game = Some(game);
                        }
                    } else if apply_game(model, game) {
                        model.pending_game = None;
                        model.authoritative_path.clear();
                        model.shot_hits.clear();
                        model.shot_explosion = None;
                    }
                }
            }
            ServerMessage::LeftRoom => leave_room(model),
            ServerMessage::Chat { player_id, text } => {
                model.chat.push(ChatView {
                    player_id: player_id.to_string(),
                    text,
                });
                trim_chat(&mut model.chat);
            }
            ServerMessage::SessionExpired => reduce(model, Action::SessionExpired),
            ServerMessage::Error { message, .. } => {
                model.notices.push(message);
                trim_notices(&mut model.notices);
            }
        },
    }
}

fn apply_room(model: &mut Model, snapshot: RoomSnapshot) -> bool {
    let room_id = snapshot.id.to_string();
    if model.room_id.as_deref() == Some(room_id.as_str())
        && model
            .room_revision
            .is_some_and(|revision| snapshot.revision < revision)
    {
        return false;
    }
    model.room_id = Some(room_id);
    model.room_revision = Some(snapshot.revision);
    model.room_name = snapshot.name;
    model.room_phase = Some(snapshot.phase);
    model.game_mode = Some(snapshot.mode);
    model.players = snapshot.players.iter().map(player_summary).collect();
    model.screen = match snapshot.phase {
        Phase::Planning | Phase::Resolving | Phase::Finished => Screen::Game,
        Phase::Lobby => Screen::Room,
    };
    true
}

fn apply_shot(model: &mut Model, shot: graphwar_protocol::ShotResolved) {
    if model.room_id.as_deref() != Some(shot.game.room_id.to_string().as_str())
        || model
            .room_revision
            .is_some_and(|revision| shot.game.revision < revision)
    {
        return;
    }
    let hit_count = shot.hits.len();
    let winner_team = shot.winner_team;
    model.authoritative_path = shot.path;
    model.preview_path.clear();
    model.shot_hits = shot
        .hits
        .into_iter()
        .map(|hit| HitView {
            player_id: hit.player_id.to_string(),
            index: hit.index,
        })
        .collect();
    model.shot_explosion = shot.explosion.map(|explosion| ExplosionView {
        x: explosion.x,
        y: explosion.y,
        radius: explosion.radius,
    });
    model.shot_sequence = model.shot_sequence.wrapping_add(1);
    model.pending_game = Some(shot.game);
    if hit_count > 0 {
        model.notices.push(format!("{hit_count} soldier(s) hit"));
    }
    if let Some(winner_team) = winner_team {
        model.notices.push(match winner_team {
            1 => "Team 1 wins".into(),
            2 => "Team 2 wins".into(),
            _ => "Draw".into(),
        });
    }
    trim_notices(&mut model.notices);
}

pub fn apply_pending_game(model: &mut Model) {
    if let Some(game) = model.pending_game.take() {
        apply_game(model, game);
    }
}

fn game_matches_model(model: &Model, game: &GameSnapshot) -> bool {
    model.room_id.as_deref() == Some(game.room_id.to_string().as_str())
        && model
            .room_revision
            .is_none_or(|revision| game.revision >= revision)
}

fn apply_game(model: &mut Model, game: GameSnapshot) -> bool {
    if !game_matches_model(model, &game) {
        return false;
    }
    model.room_revision = Some(game.revision);
    model.screen = Screen::Game;
    model.turn_player_id = game.turn_player_id.map(|id| id.to_string());
    model.turn_deadline_at = game.turn_deadline_at;
    model.soldiers = game
        .soldiers
        .into_iter()
        .map(|soldier| SoldierView {
            player_id: soldier.player_id.to_string(),
            index: soldier.index,
            x: soldier.x,
            y: soldier.y,
            team: soldier.team,
            alive: soldier.alive,
            active: soldier.active,
        })
        .collect();
    model.terrain = game
        .terrain
        .into_iter()
        .map(|circle| TerrainView {
            x: circle.x,
            y: circle.y,
            radius: circle.radius,
            cut: false,
        })
        .chain(game.terrain_cuts.into_iter().map(|circle| TerrainView {
            x: circle.x,
            y: circle.y,
            radius: circle.radius,
            cut: true,
        }))
        .collect();
    true
}

fn room_summary(room: &RoomSnapshot) -> RoomSummary {
    RoomSummary {
        id: room.id.to_string(),
        name: room.name.clone(),
        players: room.players.len().try_into().unwrap_or(u16::MAX),
        capacity: 10,
    }
}

fn player_summary(player: &PlayerSnapshot) -> PlayerSummary {
    PlayerSummary {
        id: player.id.to_string(),
        name: player.display_name.clone(),
        owner: player.owner,
        ready: player.ready,
        team: player.team,
        soldiers: player.soldiers,
        is_bot: player.is_bot,
    }
}

fn leave_room(model: &mut Model) {
    model.screen = Screen::Lobby;
    model.room_id = None;
    model.room_revision = None;
    model.room_name.clear();
    model.room_phase = None;
    model.game_mode = None;
    model.players.clear();
    model.soldiers.clear();
    model.terrain.clear();
    model.authoritative_path.clear();
    model.preview_path.clear();
    model.shot_hits.clear();
    model.shot_explosion = None;
    model.shot_sequence = 0;
    model.pending_game = None;
    model.turn_player_id = None;
    model.turn_deadline_at = None;
    model.chat.clear();
}

fn trim_chat(chat: &mut Vec<ChatView>) {
    const MAX_CHAT: usize = 100;
    let excess = chat.len().saturating_sub(MAX_CHAT);
    chat.drain(..excess);
}

fn trim_notices(notices: &mut Vec<String>) {
    const MAX_NOTICES: usize = 40;
    let excess = notices.len().saturating_sub(MAX_NOTICES);
    notices.drain(..excess);
}

#[cfg(test)]
mod tests {
    use graphwar_protocol::{RoomVisibility, ServerMessage};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn start_requires_ready_players_on_both_teams() {
        let owner = "owner".to_string();
        let mut model = Model {
            player_id: Some(owner.clone()),
            players: vec![
                PlayerSummary {
                    id: owner,
                    name: "Owner".into(),
                    owner: true,
                    ready: true,
                    team: 1,
                    soldiers: 2,
                    is_bot: false,
                },
                PlayerSummary {
                    id: "guest".into(),
                    name: "Guest".into(),
                    owner: false,
                    ready: true,
                    team: 1,
                    soldiers: 2,
                    is_bot: false,
                },
            ],
            ..Model::default()
        };
        assert!(!model.can_start());
        model.players[1].team = 2;
        assert!(model.can_start());
        model.players[1].ready = false;
        assert!(!model.can_start());
    }

    #[test]
    fn room_snapshot_advances_to_room() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::Room {
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Lobby,
                    revision: 0,
                    mode: GameMode::Function,
                    players: vec![PlayerSnapshot {
                        id: player_id,
                        display_name: "Ada".into(),
                        owner: true,
                        ready: false,
                        team: 1,
                        soldiers: 2,
                        is_bot: false,
                    }],
                },
            })),
        );
        assert_eq!(model.screen, Screen::Room);
        assert_eq!(model.room_id.as_deref(), Some(room_id.to_string().as_str()));
        assert_eq!(model.players[0].name, "Ada");
    }

    #[test]
    fn state_sync_restores_active_room() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Private,
                    phase: Phase::Planning,
                    revision: 4,
                    mode: GameMode::Function,
                    players: vec![PlayerSnapshot {
                        id: player_id,
                        display_name: "Ada".into(),
                        owner: true,
                        ready: true,
                        team: 1,
                        soldiers: 2,
                        is_bot: false,
                    }],
                },
                game: Some(GameSnapshot {
                    room_id,
                    revision: 4,
                    mode: graphwar_protocol::GameMode::Function,
                    turn_player_id: Some(player_id),
                    turn_deadline_at: Some(1_800_000_000),
                    soldiers: vec![graphwar_protocol::SoldierPosition {
                        player_id,
                        index: 0,
                        team: 1,
                        x: 110.0,
                        y: 150.0,
                        alive: true,
                        active: true,
                    }],
                    terrain: vec![graphwar_protocol::TerrainCircle {
                        x: 200.0,
                        y: 250.0,
                        radius: 40.0,
                    }],
                    terrain_cuts: Vec::new(),
                }),
            })),
        );
        assert_eq!(model.screen, Screen::Game);
        assert_eq!(model.soldiers.len(), 1);
        assert_eq!(model.terrain.len(), 1);
    }

    #[test]
    fn readiness_tracks_current_player_transitions() {
        let player_id = Uuid::new_v4();
        let mut model = Model {
            player_id: Some(player_id.to_string()),
            ..Model::default()
        };
        for ready in [true, false] {
            reduce(
                &mut model,
                Action::Message(Box::new(ServerMessage::Room {
                    snapshot: RoomSnapshot {
                        id: Uuid::new_v4(),
                        name: "Calculus club".into(),
                        visibility: RoomVisibility::Public,
                        phase: Phase::Lobby,
                        revision: 0,
                        mode: GameMode::Function,
                        players: vec![PlayerSnapshot {
                            id: player_id,
                            display_name: "Ada".into(),
                            owner: true,
                            ready,
                            team: 1,
                            soldiers: 2,
                            is_bot: false,
                        }],
                    },
                })),
            );
            assert_eq!(model.local_ready(), ready);
        }
    }

    #[test]
    fn stale_room_snapshot_cannot_regress_readiness() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            player_id: Some(player_id.to_string()),
            ..Model::default()
        };
        for (revision, ready) in [(2, true), (1, false)] {
            reduce(
                &mut model,
                Action::Message(Box::new(ServerMessage::Room {
                    snapshot: RoomSnapshot {
                        id: room_id,
                        name: "Calculus club".into(),
                        visibility: RoomVisibility::Public,
                        phase: Phase::Lobby,
                        revision,
                        mode: GameMode::Function,
                        players: vec![PlayerSnapshot {
                            id: player_id,
                            display_name: "Ada".into(),
                            owner: true,
                            ready,
                            team: 1,
                            soldiers: 2,
                            is_bot: false,
                        }],
                    },
                })),
            );
        }
        assert!(model.local_ready());
        assert_eq!(model.room_revision, Some(2));
    }

    #[test]
    fn departed_room_ignores_queued_snapshot() {
        let room_id = Uuid::new_v4();
        let mut model = Model::default();
        reduce(&mut model, Action::LeftRoom);
        apply_game(
            &mut model,
            GameSnapshot {
                room_id,
                revision: 1,
                mode: graphwar_protocol::GameMode::Function,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
            },
        );
        assert_eq!(model.screen, Screen::Lobby);
        assert!(model.room_id.is_none());
    }

    #[test]
    fn leaving_room_clears_private_state() {
        let mut model = Model {
            screen: Screen::Game,
            room_id: Some("r1".into()),
            room_name: "Calculus club".into(),
            players: vec![PlayerSummary {
                id: "p1".into(),
                name: "Ada".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            }],
            authoritative_path: vec![(0.0, 0.0)],
            soldiers: vec![SoldierView {
                player_id: "p1".into(),
                index: 0,
                x: 2.0,
                y: 3.0,
                team: 1,
                alive: true,
                active: true,
            }],
            ..Model::default()
        };
        reduce(&mut model, Action::LeftRoom);
        assert_eq!(model.screen, Screen::Lobby);
        assert!(model.room_id.is_none());
        assert!(model.players.is_empty());
        assert!(model.soldiers.is_empty());
        assert!(model.authoritative_path.is_empty());
        assert!(model.preview_path.is_empty());
        assert_eq!(model.shot_sequence, 0);
        assert!(model.pending_game.is_none());
    }

    #[test]
    fn logged_out_clears_identity_and_room_state() {
        let mut model = Model {
            screen: Screen::Game,
            connection: Connection::Online,
            player_id: Some("player".into()),
            player_name: "Ada".into(),
            room_id: Some("room".into()),
            room_name: "Calculus club".into(),
            players: vec![PlayerSummary {
                id: "player".into(),
                name: "Ada".into(),
                owner: true,
                ready: true,
                team: 1,
                soldiers: 2,
                is_bot: false,
            }],
            ..Model::default()
        };
        reduce(&mut model, Action::LoggedOut);
        assert_eq!(model.screen, Screen::Login);
        assert_eq!(model.connection, Connection::Connecting);
        assert!(model.player_id.is_none());
        assert!(model.player_name.is_empty());
        assert!(model.room_id.is_none());
        assert!(model.players.is_empty());
    }

    #[test]
    fn expired_session_clears_identity_and_room_state() {
        let mut model = Model {
            screen: Screen::Game,
            connection: Connection::Online,
            player_id: Some("player".into()),
            room_id: Some("room".into()),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::SessionExpired)),
        );
        assert_eq!(model, Model::default());
    }

    #[test]
    fn state_sync_keeps_pending_shot_board_until_animation_finishes() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            authoritative_path: vec![(100.0, 225.0), (120.0, 225.0)],
            pending_game: Some(GameSnapshot {
                room_id,
                revision: 1,
                mode: GameMode::Function,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
            }),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Resolving,
                    revision: 1,
                    mode: GameMode::Function,
                    players: Vec::new(),
                },
                game: Some(GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    turn_player_id: Some(player_id),
                    turn_deadline_at: None,
                    soldiers: vec![graphwar_protocol::SoldierPosition {
                        player_id,
                        index: 0,
                        team: 1,
                        x: 100.0,
                        y: 225.0,
                        alive: false,
                        active: false,
                    }],
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                }),
            })),
        );
        assert!(model.soldiers.is_empty());
        assert!(model.pending_game.is_some());
        assert!(!model.authoritative_path.is_empty());
    }

    #[test]
    fn stale_game_started_preserves_newer_shot_animation() {
        let room_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(2),
            authoritative_path: vec![(100.0, 225.0), (120.0, 225.0)],
            preview_path: vec![(100.0, 225.0)],
            shot_hits: vec![HitView {
                player_id: "player".into(),
                index: 0,
            }],
            shot_explosion: Some(ExplosionView {
                x: 120.0,
                y: 225.0,
                radius: 12.0,
            }),
            pending_game: Some(GameSnapshot {
                room_id,
                revision: 2,
                mode: GameMode::Function,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
            }),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::GameStarted {
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Planning,
                    revision: 1,
                    mode: GameMode::Function,
                    players: Vec::new(),
                },
                game: GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                },
            })),
        );
        assert_eq!(model.room_revision, Some(2));
        assert!(!model.authoritative_path.is_empty());
        assert!(!model.preview_path.is_empty());
        assert!(!model.shot_hits.is_empty());
        assert!(model.shot_explosion.is_some());
        assert!(model.pending_game.is_some());
    }

    #[test]
    fn shot_keeps_pre_impact_board_until_animation_finishes() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            soldiers: vec![SoldierView {
                player_id: player_id.to_string(),
                index: 0,
                x: 100.0,
                y: 225.0,
                team: 1,
                alive: true,
                active: true,
            }],
            ..Model::default()
        };
        let snapshot = RoomSnapshot {
            id: room_id,
            name: "Calculus club".into(),
            visibility: RoomVisibility::Public,
            phase: Phase::Resolving,
            revision: 1,
            mode: GameMode::Function,
            players: Vec::new(),
        };
        let game = GameSnapshot {
            room_id,
            revision: 1,
            mode: GameMode::Function,
            turn_player_id: Some(player_id),
            turn_deadline_at: None,
            soldiers: vec![graphwar_protocol::SoldierPosition {
                player_id,
                index: 0,
                team: 1,
                x: 100.0,
                y: 225.0,
                alive: false,
                active: false,
            }],
            terrain: Vec::new(),
            terrain_cuts: Vec::new(),
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::ShotResolved {
                snapshot,
                shot: graphwar_protocol::ShotResolved {
                    path: vec![(100.0, 225.0), (120.0, 225.0)],
                    hits: Vec::new(),
                    explosion: None,
                    winner_team: None,
                    game,
                },
            })),
        );
        assert!(model.soldiers[0].alive);
        assert!(model.pending_game.is_some());
        apply_pending_game(&mut model);
        assert!(!model.soldiers[0].alive);
        assert!(model.pending_game.is_none());
    }
}
