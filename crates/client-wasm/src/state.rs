use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RoomSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub players: u16,
    #[serde(default = "default_room_capacity")]
    pub capacity: u16,
}

const fn default_room_capacity() -> u16 {
    10
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PlayerSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub team: u8,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SoldierView {
    pub x: f64,
    pub y: f64,
    pub team: u8,
    #[serde(default = "full_health")]
    pub health: u16,
    #[serde(default)]
    pub active: bool,
}

const fn full_health() -> u16 {
    100
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        player_id: String,
        name: String,
    },
    LobbySnapshot {
        rooms: Vec<RoomSummary>,
    },
    RoomSnapshot {
        room_id: String,
        room_name: String,
        players: Vec<PlayerSummary>,
    },
    GameStarted,
    GameSnapshot {
        soldiers: Vec<SoldierView>,
    },
    Chat {
        sender: String,
        body: String,
    },
    Error {
        message: String,
    },
    Pong,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    Login { name: String },
    CreateRoom { name: String },
    JoinRoom { room_id: String },
    LeaveRoom,
    SetReady { ready: bool },
    Fire { function: String, angle_deg: f64 },
    Chat { body: String },
    Ping,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub screen: Screen,
    pub connection: Connection,
    pub player_id: Option<String>,
    pub player_name: String,
    pub room_id: Option<String>,
    pub room_name: String,
    pub rooms: Vec<RoomSummary>,
    pub players: Vec<PlayerSummary>,
    pub soldiers: Vec<SoldierView>,
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Connecting,
    Connected,
    Disconnected { attempt: u32 },
    GiveUp,
    Message(ServerMessage),
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
        Action::LeftRoom => {
            model.screen = Screen::Lobby;
            model.room_id = None;
            model.room_name.clear();
            model.players.clear();
            model.soldiers.clear();
        }
        Action::Message(message) => match message {
            ServerMessage::Welcome { player_id, name } => {
                model.player_id = Some(player_id);
                model.player_name = name;
                model.screen = Screen::Lobby;
            }
            ServerMessage::LobbySnapshot { rooms } => model.rooms = rooms,
            ServerMessage::RoomSnapshot {
                room_id,
                room_name,
                players,
            } => {
                model.room_id = Some(room_id);
                model.room_name = room_name;
                model.players = players;
                model.screen = Screen::Room;
            }
            ServerMessage::GameStarted => model.screen = Screen::Game,
            ServerMessage::GameSnapshot { soldiers } => model.soldiers = soldiers,
            ServerMessage::Chat { sender, body } => {
                model.notices.push(format!("{sender}: {body}"));
                trim_notices(&mut model.notices);
            }
            ServerMessage::Error { message } => {
                model.notices.push(message);
                trim_notices(&mut model.notices);
            }
            ServerMessage::Pong => {}
        },
    }
}

fn trim_notices(notices: &mut Vec<String>) {
    const MAX_NOTICES: usize = 40;
    let excess = notices.len().saturating_sub(MAX_NOTICES);
    notices.drain(..excess);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_advances_to_lobby() {
        let mut model = Model::default();
        reduce(
            &mut model,
            Action::Message(ServerMessage::Welcome {
                player_id: "p1".into(),
                name: "Ada".into(),
            }),
        );
        assert_eq!(model.screen, Screen::Lobby);
        assert_eq!(model.player_name, "Ada");
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
                ready: true,
                team: 1,
            }],
            soldiers: vec![SoldierView {
                x: 2.0,
                y: 3.0,
                team: 1,
                health: 100,
                active: true,
            }],
            ..Model::default()
        };
        reduce(&mut model, Action::LeftRoom);
        assert_eq!(model.screen, Screen::Lobby);
        assert!(model.room_id.is_none());
        assert!(model.players.is_empty());
        assert!(model.soldiers.is_empty());
    }
}
