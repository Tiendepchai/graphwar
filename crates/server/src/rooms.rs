use graphwar_protocol::{Phase, PlayerSnapshot, RoomSnapshot, RoomVisibility};
use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

pub type RoomRegistry = std::sync::Arc<tokio::sync::RwLock<Registry>>;

#[derive(Default)]
pub struct Registry {
    rooms: HashMap<Uuid, Room>,
}

struct Room {
    snapshot: RoomSnapshot,
    invite: Option<String>,
    members: HashMap<Uuid, bool>,
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
    #[error("action is invalid during the current phase")]
    WrongPhase,
}

impl RoomError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound => "not_found",
            Self::Private => "private",
            Self::NotMember => "not_member",
            Self::NotOwner => "not_owner",
            Self::WrongPhase => "wrong_phase",
        }
    }
}

impl Registry {
    pub fn create(
        &mut self,
        owner: Uuid,
        name: String,
        visibility: RoomVisibility,
    ) -> Result<RoomSnapshot, RoomError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(RoomError::Invalid("room name must be 1-64 characters"));
        }
        let id = Uuid::new_v4();
        let snapshot = RoomSnapshot {
            id,
            name: name.into(),
            visibility,
            phase: Phase::Lobby,
            revision: 0,
            players: vec![PlayerSnapshot {
                id: owner,
                display_name: "player".into(),
                owner: true,
                ready: false,
            }],
        };
        let mut members = HashMap::new();
        members.insert(owner, false);
        self.rooms.insert(
            id,
            Room {
                snapshot: snapshot.clone(),
                invite: (visibility == RoomVisibility::Private).then(|| Uuid::new_v4().to_string()),
                members,
            },
        );
        Ok(snapshot)
    }

    pub fn join(
        &mut self,
        player: Uuid,
        room_id: Uuid,
        invite: Option<&str>,
    ) -> Result<RoomSnapshot, RoomError> {
        let room = self.rooms.get_mut(&room_id).ok_or(RoomError::NotFound)?;
        if room.snapshot.visibility == RoomVisibility::Private && room.invite.as_deref() != invite {
            return Err(RoomError::Private);
        }
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = room.members.entry(player) {
            entry.insert(false);
            room.snapshot.players.push(PlayerSnapshot {
                id: player,
                display_name: "player".into(),
                owner: false,
                ready: false,
            });
            room.snapshot.revision += 1;
        }
        Ok(room.snapshot.clone())
    }

    pub fn leave(&mut self, player: Uuid) -> Result<(), RoomError> {
        let id = self
            .rooms
            .iter()
            .find_map(|(id, room)| room.members.contains_key(&player).then_some(*id))
            .ok_or(RoomError::NotMember)?;
        let room = self.rooms.get_mut(&id).unwrap();
        room.members.remove(&player);
        room.snapshot.players.retain(|p| p.id != player);
        room.snapshot.revision += 1;
        if room.snapshot.players.is_empty() {
            self.rooms.remove(&id);
        } else if !room.snapshot.players.iter().any(|p| p.owner) {
            room.snapshot.players[0].owner = true;
        }
        Ok(())
    }

    pub fn set_ready(&mut self, player: Uuid, ready: bool) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if room.snapshot.phase != Phase::Lobby {
            return Err(RoomError::WrongPhase);
        }
        *room.members.get_mut(&player).unwrap() = ready;
        room.snapshot
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .unwrap()
            .ready = ready;
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn start_game(&mut self, player: Uuid) -> Result<RoomSnapshot, RoomError> {
        let room = self.member_room_mut(player)?;
        if !room
            .snapshot
            .players
            .iter()
            .any(|p| p.id == player && p.owner)
        {
            return Err(RoomError::NotOwner);
        }
        if room.snapshot.phase != Phase::Lobby || room.snapshot.players.iter().any(|p| !p.ready) {
            return Err(RoomError::WrongPhase);
        }
        room.snapshot.phase = Phase::Planning;
        room.snapshot.revision += 1;
        Ok(room.snapshot.clone())
    }

    pub fn require_member(&self, player: Uuid) -> Result<(), RoomError> {
        self.rooms
            .values()
            .any(|r| r.members.contains_key(&player))
            .then_some(())
            .ok_or(RoomError::NotMember)
    }
    pub fn public_snapshots(&self) -> Vec<RoomSnapshot> {
        self.rooms
            .values()
            .filter(|r| r.snapshot.visibility == RoomVisibility::Public)
            .map(|r| r.snapshot.clone())
            .collect()
    }
    fn member_room_mut(&mut self, player: Uuid) -> Result<&mut Room, RoomError> {
        self.rooms
            .values_mut()
            .find(|r| r.members.contains_key(&player))
            .ok_or(RoomError::NotMember)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_only_start_after_all_ready() {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create(owner, "room".into(), RoomVisibility::Public)
            .unwrap();
        registry.join(guest, room.id, None).unwrap();
        assert!(matches!(
            registry.start_game(owner),
            Err(RoomError::WrongPhase)
        ));
        registry.set_ready(owner, true).unwrap();
        registry.set_ready(guest, true).unwrap();
        assert_eq!(registry.start_game(owner).unwrap().phase, Phase::Planning);
        assert!(matches!(
            registry.set_ready(guest, false),
            Err(RoomError::WrongPhase)
        ));
    }
}
