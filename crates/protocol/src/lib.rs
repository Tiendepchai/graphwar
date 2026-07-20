//! Versioned, JSON-compatible wire messages. Server remains authoritative for all mutations.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub id: Uuid,
    pub display_name: String,
    pub owner: bool,
    pub ready: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub id: Uuid,
    pub name: String,
    pub visibility: RoomVisibility,
    pub phase: Phase,
    pub revision: u64,
    pub players: Vec<PlayerSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        version: u16,
    },
    CreateRoom {
        name: String,
        visibility: RoomVisibility,
    },
    JoinRoom {
        room_id: Uuid,
        invite: Option<String>,
    },
    LeaveRoom,
    SetReady {
        ready: bool,
    },
    StartGame,
    Chat {
        text: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello { version: u16 },
    Error { code: String, message: String },
    Room { snapshot: RoomSnapshot },
    RoomList { rooms: Vec<RoomSnapshot> },
    Chat { player_id: Uuid, text: String },
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
        assert_eq!(json, r#"{"type":"hello","payload":{"version":1}}"#);
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
}
