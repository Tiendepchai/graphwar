use graphwar_protocol::{
    GameMode, GameSnapshot, Phase, PlayerSnapshot, PracticeSetup, RoomKind, RoomSnapshot,
    RoomVisibility, ServerMessage, ShotMissReason, ShotOutcome,
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
    pub protected: bool,
    pub kind: RoomKind,
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
    pub sequence: u64,
    pub player_id: String,
    pub display_name: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShotHistoryView {
    pub sequence: u64,
    pub player_id: String,
    pub display_name: String,
    pub team: u8,
    pub function: String,
    pub angle_deg: f64,
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
    pub room_kind: Option<RoomKind>,
    pub practice_setup: Option<PracticeSetup>,
    pub game_mode: Option<GameMode>,
    pub rooms: Vec<RoomSummary>,
    pub players: Vec<PlayerSummary>,
    pub soldiers: Vec<SoldierView>,
    pub terrain: Vec<TerrainView>,
    pub authoritative_path: Vec<(f64, f64)>,
    pub preview_path: Vec<(f64, f64)>,
    pub shot_hits: Vec<HitView>,
    pub shot_explosion: Option<ExplosionView>,
    pub shot_status: Option<String>,
    pub winner_team: Option<u8>,
    pub shot_sequence: u64,
    pub(crate) last_shot_revision: Option<u64>,
    pub pending_game: Option<GameSnapshot>,
    pub draft_function: String,
    pub aim_angle_deg: f64,
    pub turn_player_id: Option<String>,
    pub turn_deadline_at: Option<i64>,
    pub chat: Vec<ChatView>,
    pub shot_history: Vec<ShotHistoryView>,
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
            ServerMessage::RoomCreated {
                snapshot,
                practice_setup,
                ..
            }
            | ServerMessage::Room {
                snapshot,
                practice_setup,
            } => {
                if apply_room(model, snapshot) {
                    model.practice_setup = practice_setup;
                }
            }
            ServerMessage::RoomList { rooms } => {
                model.rooms = rooms.iter().map(room_summary).collect();
            }
            ServerMessage::GameStarted { snapshot, game } => {
                if apply_room(model, snapshot) && apply_game(model, game) {
                    model.pending_game = None;
                    model.authoritative_path.clear();
                    model.preview_path.clear();
                    model.shot_hits.clear();
                    model.shot_explosion = None;
                    model.shot_status = None;
                    model.notices.clear();
                }
            }
            ServerMessage::TurnStarted { snapshot, game } => {
                if apply_room(model, snapshot) && apply_game(model, game) {
                    model.pending_game = None;
                    model.authoritative_path.clear();
                    model.preview_path.clear();
                    model.shot_hits.clear();
                    model.shot_explosion = None;
                    model.shot_status = None;
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
            ServerMessage::StateSync {
                snapshot,
                game,
                practice_setup,
                chat_history,
            } => {
                let room_id = snapshot.id.to_string();
                let switched_room = model.room_id.as_deref() != Some(room_id.as_str());
                let newer_state = switched_room
                    || model
                        .room_revision
                        .is_none_or(|revision| snapshot.revision > revision);
                if switched_room {
                    leave_room(model);
                }
                if apply_room(model, snapshot) {
                    model.practice_setup = practice_setup;
                    replace_chat_history(model, chat_history, newer_state);
                    if let Some(game) = game {
                        if model.pending_game.is_some() && !model.authoritative_path.is_empty() {
                            if game_matches_model(model, &game) {
                                model.shot_history = shot_history_views(&game);
                                model.pending_game = Some(game);
                            }
                        } else if apply_game(model, game) {
                            model.pending_game = None;
                            model.authoritative_path.clear();
                            model.shot_hits.clear();
                            model.shot_explosion = None;
                            if model.room_phase != Some(Phase::Finished) {
                                model.shot_status = None;
                            }
                        }
                    } else {
                        model.shot_history.clear();
                    }
                }
            }
            ServerMessage::LeftRoom => leave_room(model),
            ServerMessage::Chat { entry } => {
                if model.room_id.as_deref() == Some(entry.room_id.to_string().as_str()) {
                    upsert_chat(model, chat_view(entry));
                }
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
    if model
        .room_id
        .as_deref()
        .is_some_and(|current| current != room_id)
        || model.room_id.as_deref() == Some(room_id.as_str())
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
    model.room_kind = Some(snapshot.kind);
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
    if model.last_shot_revision == Some(shot.game.revision) {
        model.shot_history = shot_history_views(&shot.game);
        model.pending_game = Some(shot.game);
        return;
    }
    let winner_team = shot.winner_team;
    model.last_shot_revision = Some(shot.game.revision);
    model.shot_history = shot_history_views(&shot.game);
    model.authoritative_path = shot.path;
    model.preview_path.clear();
    let status = match shot.outcome {
        ShotOutcome::TerrainImpact { explosion, hits } => {
            let hit_count = hits.len();
            model.shot_hits = hits
                .into_iter()
                .map(|hit| HitView {
                    player_id: hit.player_id.to_string(),
                    index: hit.index,
                })
                .collect();
            model.shot_explosion = Some(ExplosionView {
                x: explosion.x,
                y: explosion.y,
                radius: explosion.radius,
            });
            if hit_count == 0 {
                "Terrain hit; no soldiers caught in the blast".into()
            } else {
                model.notices.push(format!("{hit_count} soldier(s) hit"));
                format!("Terrain hit; {hit_count} soldier(s) hit")
            }
        }
        ShotOutcome::Miss { reason, hits } => {
            let hit_count = hits.len();
            model.shot_hits = hits
                .into_iter()
                .map(|hit| HitView {
                    player_id: hit.player_id.to_string(),
                    index: hit.index,
                })
                .collect();
            model.shot_explosion = None;
            let miss = match reason {
                ShotMissReason::WorldExit => "trajectory left the battlefield",
                ShotMissReason::Numerical => "function became undefined",
                ShotMissReason::StepLimit => "simulation limit reached",
            };
            if hit_count == 0 {
                format!("Shot missed: {miss}")
            } else {
                model.notices.push(format!("{hit_count} soldier(s) hit"));
                format!("Shot hit {hit_count} soldier(s); {miss}")
            }
        }
        ShotOutcome::Forfeit => {
            model.shot_hits.clear();
            model.shot_explosion = None;
            "Player forfeited".into()
        }
    };
    model.shot_status = Some(status);
    model.shot_sequence = model.shot_sequence.wrapping_add(1);
    model.pending_game = Some(shot.game);
    if let Some(winner_team) = winner_team {
        model.winner_team = Some(winner_team);
        let notice = winner_status(Some(winner_team));
        if !model.notices.contains(&notice) {
            model.notices.push(notice);
        }
    }
    trim_notices(&mut model.notices);
}

fn winner_status(winner_team: Option<u8>) -> String {
    match winner_team {
        Some(1) => "Team 1 wins".into(),
        Some(2) => "Team 2 wins".into(),
        _ => "Draw".into(),
    }
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
    let winner_team = game.winner_team;
    model.room_revision = Some(game.revision);
    model.screen = Screen::Game;
    model.turn_player_id = game.turn_player_id.map(|id| id.to_string());
    model.turn_deadline_at = game.turn_deadline_at;
    model.shot_history = shot_history_views(&game);
    model.winner_team = winner_team;
    if model.room_phase == Some(Phase::Finished) {
        model.shot_status = Some(winner_status(winner_team));
    }
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

fn shot_history_views(game: &GameSnapshot) -> Vec<ShotHistoryView> {
    game.shot_history
        .iter()
        .map(|entry| ShotHistoryView {
            sequence: entry.sequence,
            player_id: entry.player_id.to_string(),
            display_name: entry.display_name.clone(),
            team: entry.team,
            function: entry.function.clone(),
            angle_deg: entry.angle_deg,
        })
        .collect()
}

fn chat_view(entry: graphwar_protocol::ChatEntry) -> ChatView {
    ChatView {
        sequence: entry.sequence,
        player_id: entry.player_id.to_string(),
        display_name: entry.display_name,
        text: entry.text,
    }
}

fn upsert_chat(model: &mut Model, entry: ChatView) {
    if let Some(existing) = model
        .chat
        .iter_mut()
        .find(|item| item.sequence == entry.sequence)
    {
        *existing = entry;
    } else {
        model.chat.push(entry);
    }
    model.chat.sort_by_key(|item| item.sequence);
    trim_chat(&mut model.chat);
}

fn replace_chat_history(
    model: &mut Model,
    entries: Vec<graphwar_protocol::ChatEntry>,
    authoritative: bool,
) {
    let incoming_sequence = entries.iter().map(|entry| entry.sequence).max();
    let current_sequence = model.chat.iter().map(|entry| entry.sequence).max();
    if !authoritative && incoming_sequence < current_sequence {
        return;
    }
    model.chat.clear();
    for entry in entries {
        upsert_chat(model, chat_view(entry));
    }
}

fn room_summary(room: &RoomSnapshot) -> RoomSummary {
    RoomSummary {
        id: room.id.to_string(),
        name: room.name.clone(),
        players: room.players.len().try_into().unwrap_or(u16::MAX),
        capacity: 10,
        protected: room.visibility == RoomVisibility::Private,
        kind: room.kind,
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
    model.room_kind = None;
    model.practice_setup = None;
    model.game_mode = None;
    model.players.clear();
    model.soldiers.clear();
    model.terrain.clear();
    model.authoritative_path.clear();
    model.preview_path.clear();
    model.shot_hits.clear();
    model.shot_explosion = None;
    model.shot_status = None;
    model.winner_team = None;
    model.shot_sequence = 0;
    model.last_shot_revision = None;
    model.pending_game = None;
    model.turn_player_id = None;
    model.turn_deadline_at = None;
    model.chat.clear();
    model.shot_history.clear();
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
    use graphwar_protocol::{ChatEntry, RoomVisibility, ServerMessage};
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
                practice_setup: None,
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Lobby,
                    revision: 0,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
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
    fn queued_room_snapshot_cannot_restore_or_replace_membership() {
        let current_room = Uuid::new_v4();
        let queued_room = Uuid::new_v4();
        let snapshot = |id| RoomSnapshot {
            id,
            name: "Queued".into(),
            visibility: RoomVisibility::Public,
            phase: Phase::Lobby,
            revision: 1,
            mode: GameMode::Function,
            kind: RoomKind::Standard,
            players: Vec::new(),
        };
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::Room {
                snapshot: snapshot(current_room),
                practice_setup: None,
            })),
        );
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::Room {
                snapshot: snapshot(queued_room),
                practice_setup: None,
            })),
        );
        assert_eq!(
            model.room_id.as_deref(),
            Some(current_room.to_string().as_str())
        );

        reduce(&mut model, Action::LeftRoom);
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::Room {
                snapshot: snapshot(queued_room),
                practice_setup: None,
            })),
        );
        assert_eq!(model.screen, Screen::Room);
        assert_eq!(
            model.room_id.as_deref(),
            Some(queued_room.to_string().as_str())
        );
    }

    #[test]
    fn practice_room_applies_setup_and_stale_snapshot_cannot_regress_it() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let newer = PracticeSetup {
            terrain: vec![graphwar_protocol::TerrainCircle {
                x: 100.0,
                y: 120.0,
                radius: 40.0,
            }],
            players: vec![graphwar_protocol::PracticePlayerPlacement {
                player_id,
                soldiers: vec![graphwar_protocol::SetupPoint { x: 30.0, y: 40.0 }],
            }],
        };
        let mut model = Model::default();
        for (revision, setup) in [(2, newer.clone()), (1, PracticeSetup::default())] {
            reduce(
                &mut model,
                Action::Message(Box::new(ServerMessage::Room {
                    snapshot: RoomSnapshot {
                        id: room_id,
                        name: "Practice".into(),
                        visibility: RoomVisibility::Public,
                        phase: Phase::Lobby,
                        revision,
                        mode: GameMode::Function,
                        kind: RoomKind::Practice,
                        players: vec![PlayerSnapshot {
                            id: player_id,
                            display_name: "Ada".into(),
                            owner: true,
                            ready: false,
                            team: 1,
                            soldiers: 1,
                            is_bot: false,
                        }],
                    },
                    practice_setup: Some(setup),
                })),
            );
        }
        assert_eq!(model.room_kind, Some(RoomKind::Practice));
        assert_eq!(model.practice_setup, Some(newer));
        reduce(&mut model, Action::LeftRoom);
        assert!(model.practice_setup.is_none());
    }

    #[test]
    fn room_list_marks_private_rooms_as_protected() {
        let room_id = Uuid::new_v4();
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::RoomList {
                rooms: vec![RoomSnapshot {
                    id: room_id,
                    name: "Protected".into(),
                    visibility: RoomVisibility::Private,
                    phase: Phase::Lobby,
                    revision: 0,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                }],
            })),
        );
        assert_eq!(
            model.rooms,
            [RoomSummary {
                id: room_id.to_string(),
                name: "Protected".into(),
                players: 0,
                capacity: 10,
                protected: true,
                kind: RoomKind::Standard,
            }]
        );
    }

    #[test]
    fn state_sync_restores_active_room() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                practice_setup: None,
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Private,
                    phase: Phase::Planning,
                    revision: 4,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
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
                chat_history: Vec::new(),
                game: Some(GameSnapshot {
                    room_id,
                    revision: 4,
                    mode: graphwar_protocol::GameMode::Function,
                    winner_team: None,
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
                    shot_history: Vec::new(),
                }),
            })),
        );
        assert_eq!(model.screen, Screen::Game);
        assert_eq!(model.soldiers.len(), 1);
        assert_eq!(model.terrain.len(), 1);
    }

    #[test]
    fn authoritative_state_sync_switches_rooms() {
        let stale_room = Uuid::new_v4();
        let current_room = Uuid::new_v4();
        let mut model = Model {
            screen: Screen::Room,
            room_id: Some(stale_room.to_string()),
            room_revision: Some(99),
            room_name: "Stale".into(),
            notices: vec!["keep account notice".into()],
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                snapshot: RoomSnapshot {
                    id: current_room,
                    name: "Current".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Lobby,
                    revision: 1,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                },
                game: None,
                practice_setup: None,
                chat_history: Vec::new(),
            })),
        );
        assert_eq!(
            model.room_id.as_deref(),
            Some(current_room.to_string().as_str())
        );
        assert_eq!(model.room_name, "Current");
        assert_eq!(model.screen, Screen::Room);
        assert_eq!(model.notices, ["keep account notice"]);
    }

    #[test]
    fn readiness_tracks_current_player_transitions() {
        let player_id = Uuid::new_v4();
        let mut model = Model {
            player_id: Some(player_id.to_string()),
            ..Model::default()
        };
        let room_id = Uuid::new_v4();
        for (revision, ready) in [(0, true), (1, false)] {
            reduce(
                &mut model,
                Action::Message(Box::new(ServerMessage::Room {
                    practice_setup: None,
                    snapshot: RoomSnapshot {
                        id: room_id,
                        name: "Calculus club".into(),
                        visibility: RoomVisibility::Public,
                        phase: Phase::Lobby,
                        revision,
                        mode: GameMode::Function,
                        kind: RoomKind::Standard,
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
                    practice_setup: None,
                    snapshot: RoomSnapshot {
                        id: room_id,
                        name: "Calculus club".into(),
                        visibility: RoomVisibility::Public,
                        phase: Phase::Lobby,
                        revision,
                        mode: GameMode::Function,
                        kind: RoomKind::Standard,
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
                winner_team: None,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
                shot_history: Vec::new(),
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

    fn active_game_message(room_id: Uuid, revision: u64, started: bool) -> ServerMessage {
        let snapshot = RoomSnapshot {
            id: room_id,
            name: "Calculus club".into(),
            visibility: RoomVisibility::Public,
            phase: Phase::Planning,
            revision,
            mode: GameMode::Function,
            kind: RoomKind::Standard,
            players: Vec::new(),
        };
        let game = GameSnapshot {
            room_id,
            revision,
            mode: GameMode::Function,
            winner_team: None,
            turn_player_id: None,
            turn_deadline_at: None,
            soldiers: Vec::new(),
            terrain: Vec::new(),
            terrain_cuts: Vec::new(),
            shot_history: Vec::new(),
        };
        if started {
            ServerMessage::GameStarted { snapshot, game }
        } else {
            ServerMessage::TurnStarted { snapshot, game }
        }
    }

    #[test]
    fn game_started_clears_previous_match_notices() {
        let room_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            notices: vec!["Team 1 wins".into()],
            shot_status: Some("old outcome".into()),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(active_game_message(room_id, 1, true))),
        );
        assert!(model.notices.is_empty());
        assert!(model.shot_status.is_none());
    }

    #[test]
    fn turn_started_preserves_current_match_notices() {
        let room_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            notices: vec!["1 soldier(s) hit".into()],
            shot_status: Some("old outcome".into()),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(active_game_message(room_id, 1, false))),
        );
        assert_eq!(model.notices, ["1 soldier(s) hit"]);
        assert!(model.shot_status.is_none());
    }

    #[test]
    fn chat_reducer_sorts_deduplicates_and_rejects_wrong_room() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            ..Model::default()
        };
        let entry = |sequence, text: &str, room_id| ChatEntry {
            room_id,
            sequence,
            player_id,
            display_name: "Ada".into(),
            text: text.into(),
        };
        for item in [
            entry(3, "after", room_id),
            entry(1, "before", room_id),
            entry(3, "after", room_id),
        ] {
            reduce(
                &mut model,
                Action::Message(Box::new(ServerMessage::Chat { entry: item })),
            );
        }
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::Chat {
                entry: entry(2, "wrong", Uuid::new_v4()),
            })),
        );
        assert_eq!(
            model
                .chat
                .iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(model.chat[0].text, "before");
        assert_eq!(model.chat[1].text, "after");
    }

    #[test]
    fn stale_state_sync_does_not_regress_chat_history() {
        let room_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(1),
            chat: vec![ChatView {
                sequence: 5,
                player_id: "current".into(),
                display_name: "Current".into(),
                text: "new".into(),
            }],
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                practice_setup: None,
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Room".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Lobby,
                    revision: 1,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                },
                game: None,
                chat_history: vec![ChatEntry {
                    room_id,
                    sequence: 4,
                    player_id: Uuid::new_v4(),
                    display_name: "Old".into(),
                    text: "old".into(),
                }],
            })),
        );
        assert_eq!(model.chat[0].sequence, 5);
    }

    #[test]
    fn state_sync_replaces_chat_history_and_preserves_captured_name() {
        let room_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            chat: vec![ChatView {
                sequence: 99,
                player_id: "old".into(),
                display_name: "Old".into(),
                text: "stale".into(),
            }],
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                practice_setup: None,
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Room".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Lobby,
                    revision: 1,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                },
                game: None,
                chat_history: vec![ChatEntry {
                    room_id,
                    sequence: 4,
                    player_id: Uuid::new_v4(),
                    display_name: "Departed Ada".into(),
                    text: "retained".into(),
                }],
            })),
        );
        assert_eq!(model.chat.len(), 1);
        assert_eq!(model.chat[0].display_name, "Departed Ada");
        assert_eq!(model.chat[0].sequence, 4);
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
                winner_team: None,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
                shot_history: Vec::new(),
            }),
            ..Model::default()
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::StateSync {
                practice_setup: None,
                snapshot: RoomSnapshot {
                    id: room_id,
                    name: "Calculus club".into(),
                    visibility: RoomVisibility::Public,
                    phase: Phase::Resolving,
                    revision: 1,
                    mode: GameMode::Function,
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                },
                chat_history: Vec::new(),
                game: Some(GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    winner_team: None,
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
                    shot_history: Vec::new(),
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
                winner_team: None,
                turn_player_id: None,
                turn_deadline_at: None,
                soldiers: Vec::new(),
                terrain: Vec::new(),
                terrain_cuts: Vec::new(),
                shot_history: Vec::new(),
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
                    kind: RoomKind::Standard,
                    players: Vec::new(),
                },
                game: GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    winner_team: None,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                    shot_history: Vec::new(),
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
    fn shot_history_updates_before_pending_board_applies() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            ..Model::default()
        };
        let snapshot = RoomSnapshot {
            id: room_id,
            name: "Calculus club".into(),
            visibility: RoomVisibility::Public,
            phase: Phase::Resolving,
            revision: 1,
            mode: GameMode::Function,
            kind: RoomKind::Standard,
            players: Vec::new(),
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::ShotResolved {
                snapshot,
                shot: graphwar_protocol::ShotResolved {
                    path: vec![(100.0, 225.0), (120.0, 225.0)],
                    outcome: ShotOutcome::Miss {
                        reason: ShotMissReason::WorldExit,
                        hits: Vec::new(),
                    },
                    winner_team: None,
                    game: GameSnapshot {
                        room_id,
                        revision: 1,
                        mode: GameMode::Function,
                        winner_team: None,
                        turn_player_id: None,
                        turn_deadline_at: None,
                        soldiers: Vec::new(),
                        terrain: Vec::new(),
                        terrain_cuts: Vec::new(),
                        shot_history: vec![graphwar_protocol::ShotHistoryEntry {
                            sequence: 1,
                            player_id,
                            display_name: "Ada".into(),
                            team: 1,
                            function: "sin(x)".into(),
                            angle_deg: 0.0,
                        }],
                    },
                },
            })),
        );
        assert!(model.pending_game.is_some());
        assert_eq!(model.shot_history[0].function, "sin(x)");
        assert_eq!(
            model.shot_status.as_deref(),
            Some("Shot missed: trajectory left the battlefield")
        );
        assert!(model.shot_hits.is_empty());
        assert!(model.shot_explosion.is_none());
    }

    #[test]
    fn miss_populates_projectile_hits_without_explosion() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            ..Model::default()
        };
        apply_shot(
            &mut model,
            graphwar_protocol::ShotResolved {
                path: vec![(100.0, 225.0), (120.0, 225.0)],
                outcome: ShotOutcome::Miss {
                    reason: ShotMissReason::WorldExit,
                    hits: vec![graphwar_protocol::SoldierSnapshot {
                        player_id,
                        index: 0,
                        team: 2,
                        alive: false,
                    }],
                },
                winner_team: None,
                game: GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    winner_team: None,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                    shot_history: Vec::new(),
                },
            },
        );
        assert_eq!(model.shot_hits.len(), 1);
        assert!(model.shot_explosion.is_none());
        assert_eq!(
            model.shot_status.as_deref(),
            Some("Shot hit 1 soldier(s); trajectory left the battlefield")
        );
    }

    #[test]
    fn terrain_impact_populates_terminal_effects() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            ..Model::default()
        };
        apply_shot(
            &mut model,
            graphwar_protocol::ShotResolved {
                path: vec![(100.0, 225.0), (120.0, 225.0)],
                outcome: ShotOutcome::TerrainImpact {
                    explosion: graphwar_protocol::TerrainCircle {
                        x: 120.0,
                        y: 225.0,
                        radius: 12.0,
                    },
                    hits: vec![graphwar_protocol::SoldierSnapshot {
                        player_id,
                        index: 0,
                        team: 2,
                        alive: false,
                    }],
                },
                winner_team: None,
                game: GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    winner_team: None,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                    shot_history: Vec::new(),
                },
            },
        );
        assert_eq!(model.shot_hits.len(), 1);
        assert!(model.shot_explosion.is_some());
        assert_eq!(
            model.shot_status.as_deref(),
            Some("Terrain hit; 1 soldier(s) hit")
        );
    }

    #[test]
    fn duplicate_shot_does_not_advance_animation_token() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let message = ServerMessage::ShotResolved {
            snapshot: RoomSnapshot {
                id: room_id,
                name: "Calculus club".into(),
                visibility: RoomVisibility::Public,
                phase: Phase::Resolving,
                revision: 1,
                mode: GameMode::Function,
                kind: RoomKind::Standard,
                players: Vec::new(),
            },
            shot: graphwar_protocol::ShotResolved {
                path: vec![(100.0, 225.0)],
                outcome: ShotOutcome::Miss {
                    reason: ShotMissReason::WorldExit,
                    hits: Vec::new(),
                },
                winner_team: None,
                game: GameSnapshot {
                    room_id,
                    revision: 1,
                    mode: GameMode::Function,
                    winner_team: None,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                    shot_history: vec![graphwar_protocol::ShotHistoryEntry {
                        sequence: 1,
                        player_id,
                        display_name: "Ada".into(),
                        team: 1,
                        function: "sin(x)".into(),
                        angle_deg: 0.0,
                    }],
                },
            },
        };
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(0),
            ..Model::default()
        };
        reduce(&mut model, Action::Message(Box::new(message.clone())));
        let sequence = model.shot_sequence;
        reduce(&mut model, Action::Message(Box::new(message)));
        assert_eq!(model.shot_sequence, sequence);
    }

    #[test]
    fn forfeit_after_prior_shot_applies_finished_state() {
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let history = graphwar_protocol::ShotHistoryEntry {
            sequence: 1,
            player_id,
            display_name: "Ada".into(),
            team: 1,
            function: "x".into(),
            angle_deg: 0.0,
        };
        let mut model = Model {
            room_id: Some(room_id.to_string()),
            room_revision: Some(1),
            shot_history: vec![ShotHistoryView {
                sequence: 1,
                player_id: player_id.to_string(),
                display_name: "Ada".into(),
                team: 1,
                function: "x".into(),
                angle_deg: 0.0,
            }],
            ..Model::default()
        };
        apply_shot(
            &mut model,
            graphwar_protocol::ShotResolved {
                path: Vec::new(),
                outcome: ShotOutcome::Forfeit,
                winner_team: Some(1),
                game: GameSnapshot {
                    room_id,
                    revision: 2,
                    mode: GameMode::Function,
                    winner_team: None,
                    turn_player_id: None,
                    turn_deadline_at: None,
                    soldiers: Vec::new(),
                    terrain: Vec::new(),
                    terrain_cuts: Vec::new(),
                    shot_history: vec![history],
                },
            },
        );
        assert_eq!(model.shot_status.as_deref(), Some("Player forfeited"));
        assert_eq!(model.shot_sequence, 1);
        assert!(model.notices.iter().any(|notice| notice == "Team 1 wins"));
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
            kind: RoomKind::Standard,
            players: Vec::new(),
        };
        let game = GameSnapshot {
            room_id,
            revision: 1,
            mode: GameMode::Function,
            winner_team: None,
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
            shot_history: Vec::new(),
        };
        reduce(
            &mut model,
            Action::Message(Box::new(ServerMessage::ShotResolved {
                snapshot,
                shot: graphwar_protocol::ShotResolved {
                    path: vec![(100.0, 225.0), (120.0, 225.0)],
                    outcome: ShotOutcome::Miss {
                        reason: ShotMissReason::WorldExit,
                        hits: Vec::new(),
                    },
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

    #[test]
    fn every_miss_reason_has_visible_status_without_effects() {
        let room_id = Uuid::new_v4();
        for (reason, expected) in [
            (
                ShotMissReason::WorldExit,
                "Shot missed: trajectory left the battlefield",
            ),
            (
                ShotMissReason::Numerical,
                "Shot missed: function became undefined",
            ),
            (
                ShotMissReason::StepLimit,
                "Shot missed: simulation limit reached",
            ),
        ] {
            let mut model = Model {
                room_id: Some(room_id.to_string()),
                room_revision: Some(0),
                shot_hits: vec![HitView {
                    player_id: "stale".into(),
                    index: 0,
                }],
                shot_explosion: Some(ExplosionView {
                    x: 1.0,
                    y: 2.0,
                    radius: 3.0,
                }),
                ..Model::default()
            };
            apply_shot(
                &mut model,
                graphwar_protocol::ShotResolved {
                    path: vec![(100.0, 225.0)],
                    outcome: ShotOutcome::Miss {
                        reason,
                        hits: Vec::new(),
                    },
                    winner_team: None,
                    game: GameSnapshot {
                        room_id,
                        revision: 1,
                        mode: GameMode::Function,
                        winner_team: None,
                        turn_player_id: None,
                        turn_deadline_at: None,
                        soldiers: Vec::new(),
                        terrain: Vec::new(),
                        terrain_cuts: Vec::new(),
                        shot_history: Vec::new(),
                    },
                },
            );
            assert_eq!(model.shot_status.as_deref(), Some(expected));
            assert!(model.shot_hits.is_empty());
            assert!(model.shot_explosion.is_none());
        }
    }

    #[test]
    fn finished_state_sync_restores_winner_without_duplicate_notice() {
        let room_id = Uuid::new_v4();
        let game = GameSnapshot {
            room_id,
            revision: 3,
            mode: GameMode::Function,
            winner_team: Some(2),
            turn_player_id: None,
            turn_deadline_at: None,
            soldiers: Vec::new(),
            terrain: Vec::new(),
            terrain_cuts: Vec::new(),
            shot_history: Vec::new(),
        };
        let message = ServerMessage::StateSync {
            snapshot: RoomSnapshot {
                id: room_id,
                name: "Finished".into(),
                visibility: RoomVisibility::Public,
                phase: Phase::Finished,
                revision: 3,
                mode: GameMode::Function,
                kind: RoomKind::Standard,
                players: Vec::new(),
            },
            game: Some(game),
            practice_setup: None,
            chat_history: Vec::new(),
        };
        let mut model = Model::default();
        reduce(&mut model, Action::Message(Box::new(message.clone())));
        reduce(&mut model, Action::Message(Box::new(message)));
        assert_eq!(model.winner_team, Some(2));
        assert_eq!(model.shot_status.as_deref(), Some("Team 2 wins"));
        assert!(model.notices.is_empty());
    }
}
