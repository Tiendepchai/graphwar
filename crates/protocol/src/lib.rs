//! Versioned JSON wire messages. Server owns every game-state transition.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 9;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    Public,
    Private,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Lobby,
    Planning,
    Resolving,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    Function,
    FirstOrder,
    SecondOrder,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    #[default]
    Standard,
    Practice,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub id: Uuid,
    pub display_name: String,
    pub owner: bool,
    pub ready: bool,
    pub team: u8,
    pub soldiers: u8,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub id: Uuid,
    pub name: String,
    pub visibility: RoomVisibility,
    pub phase: Phase,
    pub revision: u64,
    pub mode: GameMode,
    #[serde(default)]
    pub kind: RoomKind,
    pub players: Vec<PlayerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainCircle {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetupPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PracticePlayerPlacement {
    pub player_id: Uuid,
    pub soldiers: Vec<SetupPoint>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PracticeSetup {
    pub terrain: Vec<TerrainCircle>,
    pub players: Vec<PracticePlayerPlacement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SoldierSnapshot {
    pub player_id: Uuid,
    pub index: usize,
    pub team: u8,
    pub alive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoldierPosition {
    pub player_id: Uuid,
    pub index: usize,
    pub team: u8,
    pub x: f64,
    pub y: f64,
    pub alive: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub room_id: Uuid,
    pub sequence: u64,
    pub player_id: Uuid,
    pub display_name: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotHistoryEntry {
    #[serde(default)]
    pub sequence: u64,
    pub player_id: Uuid,
    pub display_name: String,
    pub team: u8,
    pub function: String,
    pub angle_deg: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameSnapshot {
    pub room_id: Uuid,
    pub revision: u64,
    pub mode: GameMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_team: Option<u8>,
    pub turn_player_id: Option<Uuid>,
    pub turn_deadline_at: Option<i64>,
    pub soldiers: Vec<SoldierPosition>,
    pub terrain: Vec<TerrainCircle>,
    pub terrain_cuts: Vec<TerrainCircle>,
    #[serde(default)]
    pub shot_history: Vec<ShotHistoryEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotMissReason {
    WorldExit,
    Numerical,
    StepLimit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ShotOutcome {
    TerrainImpact {
        explosion: TerrainCircle,
        hits: Vec<SoldierSnapshot>,
    },
    Miss {
        reason: ShotMissReason,
    },
    Forfeit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotResolved {
    pub path: Vec<(f64, f64)>,
    pub outcome: ShotOutcome,
    pub winner_team: Option<u8>,
    pub game: GameSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        version: u16,
    },
    ListRooms,
    CreateRoom {
        name: String,
        visibility: RoomVisibility,
        #[serde(default)]
        kind: RoomKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    JoinRoom {
        room_id: Uuid,
        invite: Option<String>,
    },
    LeaveRoom,
    ReturnToLobby,
    SetReady {
        ready: bool,
    },
    SetMode {
        mode: GameMode,
    },
    SetTeam {
        player_id: Uuid,
        team: u8,
    },
    SetSoldiers {
        player_id: Uuid,
        soldiers: u8,
    },
    SetPracticeSetup {
        base_revision: u64,
        setup: PracticeSetup,
    },
    AddBot {
        level: u8,
    },
    RemoveBot {
        player_id: Uuid,
    },
    KickPlayer {
        player_id: Uuid,
    },
    StartGame,
    FireFunction {
        function: String,
        angle_deg: f64,
    },
    Chat {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello {
        version: u16,
    },
    Error {
        code: String,
        message: String,
    },
    SessionExpired,
    RoomCreated {
        snapshot: RoomSnapshot,
        invite: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        practice_setup: Option<PracticeSetup>,
    },
    Room {
        snapshot: RoomSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        practice_setup: Option<PracticeSetup>,
    },
    RoomList {
        rooms: Vec<RoomSnapshot>,
    },
    GameStarted {
        snapshot: RoomSnapshot,
        game: GameSnapshot,
    },
    TurnStarted {
        snapshot: RoomSnapshot,
        game: GameSnapshot,
    },
    ShotResolved {
        snapshot: RoomSnapshot,
        shot: ShotResolved,
    },
    GameFinished {
        snapshot: RoomSnapshot,
        shot: ShotResolved,
    },
    StateSync {
        snapshot: RoomSnapshot,
        game: Option<GameSnapshot>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        practice_setup: Option<PracticeSetup>,
        chat_history: Vec<ChatEntry>,
    },
    LeftRoom,
    Chat {
        entry: ChatEntry,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEnvelope<T> {
    pub version: u16,
    pub sequence: u64,
    pub state: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_and_versioned() {
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"hello","payload":{"version":9}}"#);
        let snap = SnapshotEnvelope {
            version: PROTOCOL_VERSION,
            sequence: 3,
            state: Phase::Lobby,
        };
        assert_eq!(
            serde_json::from_str::<SnapshotEnvelope<Phase>>(&serde_json::to_string(&snap).unwrap())
                .unwrap(),
            snap
        );
    }

    #[test]
    fn create_room_password_is_optional_and_round_trips() {
        let message = ClientMessage::CreateRoom {
            name: "Protected".into(),
            visibility: RoomVisibility::Private,
            kind: RoomKind::Practice,
            password: Some("room secret".into()),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            message
        );
        assert_eq!(
            serde_json::from_str::<ClientMessage>(
                r#"{"type":"create_room","payload":{"name":"Public","visibility":"public"}}"#,
            )
            .unwrap(),
            ClientMessage::CreateRoom {
                name: "Public".into(),
                visibility: RoomVisibility::Public,
                kind: RoomKind::Standard,
                password: None,
            }
        );
    }

    #[test]
    fn room_kind_and_practice_setup_round_trip() {
        let player_id = Uuid::new_v4();
        let setup = PracticeSetup {
            terrain: vec![TerrainCircle {
                x: 100.0,
                y: 120.0,
                radius: 40.0,
            }],
            players: vec![PracticePlayerPlacement {
                player_id,
                soldiers: vec![SetupPoint { x: 30.0, y: 40.0 }],
            }],
        };
        let message = ClientMessage::SetPracticeSetup {
            base_revision: 4,
            setup: setup.clone(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            message
        );
        assert!(!json.contains("alive"));
        assert!(!json.contains("active"));
        assert!(!json.contains("team"));
        assert!(!json.contains("terrain_cuts"));

        let room = ServerMessage::Room {
            snapshot: RoomSnapshot {
                id: Uuid::new_v4(),
                name: "Practice".into(),
                visibility: RoomVisibility::Public,
                phase: Phase::Lobby,
                revision: 4,
                mode: GameMode::Function,
                kind: RoomKind::Practice,
                players: Vec::new(),
            },
            practice_setup: Some(setup),
        };
        let json = serde_json::to_string(&room).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&json).unwrap(), room);
    }

    #[test]
    fn maximum_valid_practice_room_fits_inbound_and_outbound_limits() {
        let room_id = Uuid::new_v4();
        let players = (0..10)
            .map(|index| PlayerSnapshot {
                id: Uuid::new_v4(),
                display_name: "x".repeat(32),
                owner: index == 0,
                ready: false,
                team: if index % 2 == 0 { 1 } else { 2 },
                soldiers: 4,
                is_bot: index != 0,
            })
            .collect::<Vec<_>>();
        let setup = PracticeSetup {
            terrain: (0..64)
                .map(|index| TerrainCircle {
                    x: 70.0 + f64::from(index % 8) * 80.0,
                    y: 70.0 + f64::from(index / 8) * 40.0,
                    radius: 20.0,
                })
                .collect(),
            players: players
                .iter()
                .enumerate()
                .map(|(player_index, player)| PracticePlayerPlacement {
                    player_id: player.id,
                    soldiers: (0..4)
                        .map(|soldier_index| SetupPoint {
                            x: 10.0 + (player_index * 4 + soldier_index) as f64 * 20.0,
                            y: 430.0,
                        })
                        .collect(),
                })
                .collect(),
        };
        let snapshot = RoomSnapshot {
            id: room_id,
            name: "x".repeat(64),
            visibility: RoomVisibility::Public,
            phase: Phase::Lobby,
            revision: u64::MAX - 1,
            mode: GameMode::SecondOrder,
            kind: RoomKind::Practice,
            players,
        };
        let inbound = serde_json::to_vec(&ClientMessage::SetPracticeSetup {
            base_revision: u64::MAX - 1,
            setup: setup.clone(),
        })
        .unwrap();
        let outbound = serde_json::to_vec(&ServerMessage::Room {
            snapshot,
            practice_setup: Some(setup),
        })
        .unwrap();
        assert!(
            inbound.len() <= 8 * 1024,
            "{} byte practice input",
            inbound.len()
        );
        assert!(
            outbound.len() <= 1024 * 1024,
            "{} byte practice room",
            outbound.len()
        );
    }

    #[test]
    fn old_room_snapshot_defaults_to_standard() {
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Old","visibility":"public","phase":"lobby","revision":0,"mode":"function","players":[]}"#;
        assert_eq!(
            serde_json::from_str::<RoomSnapshot>(json).unwrap().kind,
            RoomKind::Standard
        );
    }

    #[test]
    fn session_expiry_message_round_trips() {
        let json = serde_json::to_string(&ServerMessage::SessionExpired).unwrap();
        assert_eq!(json, r#"{"type":"session_expired"}"#);
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            ServerMessage::SessionExpired
        );
    }

    #[test]
    fn bot_snapshot_and_commands_round_trip() {
        let bot = Uuid::new_v4();
        let snapshot = PlayerSnapshot {
            id: bot,
            display_name: "Bot 1".into(),
            owner: false,
            ready: true,
            team: 2,
            soldiers: 2,
            is_bot: true,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<PlayerSnapshot>(&json).unwrap(),
            snapshot
        );
        for message in [
            ClientMessage::AddBot { level: 4 },
            ClientMessage::RemoveBot { player_id: bot },
            ClientMessage::KickPlayer { player_id: bot },
            ClientMessage::ReturnToLobby,
        ] {
            let json = serde_json::to_string(&message).unwrap();
            assert_eq!(
                serde_json::from_str::<ClientMessage>(&json).unwrap(),
                message
            );
        }
    }

    #[test]
    fn old_player_snapshot_defaults_to_human() {
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","display_name":"Ada","owner":true,"ready":false,"team":1,"soldiers":2}"#;
        assert!(!serde_json::from_str::<PlayerSnapshot>(json).unwrap().is_bot);
    }

    #[test]
    fn old_game_snapshot_defaults_optional_fields() {
        let json = r#"{"room_id":"00000000-0000-0000-0000-000000000001","revision":1,"mode":"function","turn_player_id":null,"turn_deadline_at":null,"soldiers":[],"terrain":[],"terrain_cuts":[]}"#;
        let snapshot = serde_json::from_str::<GameSnapshot>(json).unwrap();
        assert_eq!(snapshot.winner_team, None);
        assert!(snapshot.shot_history.is_empty());
    }

    #[test]
    fn return_to_lobby_has_exact_json() {
        let json = serde_json::to_string(&ClientMessage::ReturnToLobby).unwrap();
        assert_eq!(json, r#"{"type":"return_to_lobby"}"#);
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            ClientMessage::ReturnToLobby
        );
    }

    #[test]
    fn finished_snapshot_winner_round_trips() {
        let snapshot = GameSnapshot {
            room_id: Uuid::new_v4(),
            revision: 4,
            mode: GameMode::Function,
            winner_team: Some(2),
            turn_player_id: None,
            turn_deadline_at: None,
            soldiers: Vec::new(),
            terrain: Vec::new(),
            terrain_cuts: Vec::new(),
            shot_history: Vec::new(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<GameSnapshot>(&json).unwrap(),
            snapshot
        );
        assert!(json.contains("\"winner_team\":2"));
    }

    #[test]
    fn old_shot_history_defaults_to_zero_sequence() {
        let json = r#"{"player_id":"00000000-0000-0000-0000-000000000001","display_name":"Ada","team":1,"function":"x","angle_deg":0.0}"#;
        assert_eq!(
            serde_json::from_str::<ShotHistoryEntry>(json)
                .unwrap()
                .sequence,
            0
        );
    }

    #[test]
    fn chat_message_round_trips_authoritative_entry() {
        let entry = ChatEntry {
            room_id: Uuid::new_v4(),
            sequence: 7,
            player_id: Uuid::new_v4(),
            display_name: "Ada".into(),
            text: "hello".into(),
        };
        let message = ServerMessage::Chat {
            entry: entry.clone(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            message
        );
        assert!(json.contains("\"sequence\":7"));
    }

    #[test]
    fn shot_outcomes_are_tagged_and_round_trip() {
        let outcomes = [
            ShotOutcome::TerrainImpact {
                explosion: TerrainCircle {
                    x: 1.0,
                    y: 2.0,
                    radius: 3.0,
                },
                hits: Vec::new(),
            },
            ShotOutcome::Miss {
                reason: ShotMissReason::WorldExit,
            },
            ShotOutcome::Miss {
                reason: ShotMissReason::Numerical,
            },
            ShotOutcome::Miss {
                reason: ShotMissReason::StepLimit,
            },
            ShotOutcome::Forfeit,
        ];
        for outcome in outcomes {
            let json = serde_json::to_string(&outcome).unwrap();
            assert_eq!(serde_json::from_str::<ShotOutcome>(&json).unwrap(), outcome);
        }
    }

    #[test]
    fn fire_message_rejects_non_finite_angles_on_receive() {
        let json = serde_json::to_string(&ClientMessage::FireFunction {
            function: "x".into(),
            angle_deg: f64::NAN,
        })
        .unwrap();
        assert!(serde_json::from_str::<ClientMessage>(&json).is_err());
    }
}
