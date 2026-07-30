//! Versioned JSON wire messages. Server owns every game-state transition.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 7;

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
    pub players: Vec<PlayerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainCircle {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
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
    pub turn_player_id: Option<Uuid>,
    pub turn_deadline_at: Option<i64>,
    pub soldiers: Vec<SoldierPosition>,
    pub terrain: Vec<TerrainCircle>,
    pub terrain_cuts: Vec<TerrainCircle>,
    #[serde(default)]
    pub shot_history: Vec<ShotHistoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotResolved {
    pub path: Vec<(f64, f64)>,
    pub hits: Vec<SoldierSnapshot>,
    pub explosion: Option<TerrainCircle>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    JoinRoom {
        room_id: Uuid,
        invite: Option<String>,
    },
    LeaveRoom,
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
    },
    Room {
        snapshot: RoomSnapshot,
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
        assert_eq!(json, r#"{"type":"hello","payload":{"version":7}}"#);
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
                password: None,
            }
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
    fn old_game_snapshot_defaults_to_empty_shot_history() {
        let json = r#"{"room_id":"00000000-0000-0000-0000-000000000001","revision":1,"mode":"function","turn_player_id":null,"turn_deadline_at":null,"soldiers":[],"terrain":[],"terrain_cuts":[]}"#;
        assert!(
            serde_json::from_str::<GameSnapshot>(json)
                .unwrap()
                .shot_history
                .is_empty()
        );
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
    fn fire_message_rejects_non_finite_angles_on_receive() {
        let json = serde_json::to_string(&ClientMessage::FireFunction {
            function: "x".into(),
            angle_deg: f64::NAN,
        })
        .unwrap();
        assert!(serde_json::from_str::<ClientMessage>(&json).is_err());
    }
}
