//! The authoritative game hub. All connections funnel into one `Hub` guarded by
//! a mutex; methods never block or await, so holding the lock is always brief.

use crate::config::Config;
use crate::world::{AccountCheck, PersistentWorld};
use kotor_protocol::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

/// Encoded frames queued for one client's writer task.
pub type Outbox = mpsc::Sender<Arc<Vec<u8>>>;

/// How many frames may queue up for a client before it is considered stuck and dropped.
pub const OUTBOX_CAPACITY: usize = 1024;

const MAX_NAME_LEN: usize = 24;
const MAX_CHAT_LEN: usize = 256;
const MAX_GLOBAL_NAME_LEN: usize = 64;
const MAX_TAG_LEN: usize = 64;
const MAX_CHARACTER_BYTES: usize = 512 * 1024;

struct Session {
    name: String,
    tx: Outbox,
    kick: Arc<Notify>,
    state: Option<PlayerState>,
    state_dirty: bool,
}

pub struct Hub {
    pub config: Config,
    pub data: PersistentWorld,
    /// World changed since the last save.
    pub dirty: bool,
    sessions: BTreeMap<PlayerId, Session>,
    leader: Option<PlayerId>,
    next_id: PlayerId,
    time_carry: f64,
}

/// What the connection task should do after a message was handled.
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Disconnect,
}

pub struct Joined {
    pub id: PlayerId,
    pub kick: Arc<Notify>,
}

impl Hub {
    pub fn new(config: Config, data: PersistentWorld) -> Self {
        Self {
            config,
            data,
            dirty: false,
            sessions: BTreeMap::new(),
            leader: None,
            next_id: 1,
            time_carry: 0.0,
        }
    }

    pub fn player_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn current_module(&self) -> Option<&str> {
        self.data.world.current_module.as_deref()
    }

    /// Validates a handshake and registers the player.
    pub fn join(&mut self, hello: &Hello, tx: Outbox) -> Result<Joined, (RejectReason, String)> {
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err((
                RejectReason::VersionMismatch,
                format!(
                    "server speaks protocol v{PROTOCOL_VERSION}, you have v{}. Update through the launcher.",
                    hello.protocol_version
                ),
            ));
        }
        if !self.config.password.is_empty()
            && hello.password.as_deref() != Some(self.config.password.as_str())
        {
            return Err((RejectReason::BadPassword, "wrong server password".into()));
        }
        let name = hello.player_name.trim();
        if !valid_name(name) {
            return Err((
                RejectReason::InvalidName,
                format!("names must be 1-{MAX_NAME_LEN} letters, digits, spaces, '-' or '_'"),
            ));
        }
        if self
            .config
            .banned
            .iter()
            .any(|b| b.eq_ignore_ascii_case(name))
        {
            return Err((
                RejectReason::Banned,
                "you are banned from this server".into(),
            ));
        }
        if self
            .sessions
            .values()
            .any(|s| s.name.eq_ignore_ascii_case(name))
        {
            return Err((RejectReason::NameTaken, format!("{name} is already online")));
        }
        if hello.player_token.len() < 16 {
            return Err((RejectReason::ProtocolError, "missing player token".into()));
        }
        if self.data.check_account(name, &hello.player_token) == AccountCheck::WrongToken {
            return Err((
                RejectReason::NameTaken,
                format!("the name {name} belongs to another player on this server"),
            ));
        }
        if self.sessions.len() >= self.config.max_players {
            return Err((RejectReason::ServerFull, "server is full".into()));
        }

        let id = self.next_id;
        self.next_id += 1;
        let character = self
            .data
            .touch_account(name, &hello.player_token)
            .character_bytes();
        self.dirty = true;

        let kick = Arc::new(Notify::new());
        self.sessions.insert(
            id,
            Session {
                name: name.to_owned(),
                tx,
                kick: kick.clone(),
                state: None,
                state_dirty: false,
            },
        );
        if self.leader.is_none() {
            self.leader = Some(id);
        }
        tracing::info!(
            "{name} joined as #{id} (build {})",
            hello.game_build.as_deref().unwrap_or("unknown")
        );

        let welcome = ServerMessage::Welcome(Welcome {
            your_id: id,
            server_name: self.config.name.clone(),
            motd: self.config.motd.clone(),
            leader: self.leader,
            players: self.player_infos(),
            world: self.data.world.clone(),
        });
        self.send(id, &welcome);
        if let Some(data) = character {
            self.send(id, &ServerMessage::CharacterData { data });
        }
        let info = PlayerInfo {
            id,
            name: name.to_owned(),
            state: None,
        };
        self.broadcast_except(id, &ServerMessage::PlayerJoined(info));
        self.system_chat(&format!("{name} joined the party"));
        Ok(Joined { id, kick })
    }

    pub fn leave(&mut self, id: PlayerId) {
        let Some(session) = self.sessions.remove(&id) else {
            return;
        };
        tracing::info!("{} (#{id}) left", session.name);
        if let Some(acct) = self.data.accounts.get_mut(&session.name.to_lowercase()) {
            acct.last_seen = crate::world::unix_now();
            self.dirty = true;
        }
        self.broadcast(&ServerMessage::PlayerLeft { id });
        if self.leader == Some(id) {
            self.leader = self.sessions.keys().next().copied();
            if let Some(new) = self.leader {
                self.broadcast(&ServerMessage::LeaderChanged { id: new });
                let name = self.sessions[&new].name.clone();
                self.system_chat(&format!("{name} is now the party leader"));
            }
        }
        self.system_chat(&format!("{} left the party", session.name));
    }

    pub fn handle(&mut self, id: PlayerId, msg: ClientMessage) -> Flow {
        if !self.sessions.contains_key(&id) {
            return Flow::Disconnect;
        }
        match msg {
            ClientMessage::Hello(_) => return Flow::Disconnect,
            ClientMessage::Goodbye => return Flow::Disconnect,
            ClientMessage::Ping { nonce } => self.send(id, &ServerMessage::Pong { nonce }),
            ClientMessage::PlayerState(state) => {
                if !state.position.is_finite() || !state.facing.is_finite() {
                    return Flow::Continue;
                }
                let s = self.sessions.get_mut(&id).unwrap();
                s.state = Some(state);
                s.state_dirty = true;
            }
            ClientMessage::EnteredModule { module } => {
                if let Some(state) = self.sessions.get_mut(&id).unwrap().state.as_mut() {
                    state.module = module.clone();
                }
                if self.leader == Some(id) && self.current_module() != Some(module.as_str()) {
                    self.transition(module, String::new(), Some(id));
                }
            }
            ClientMessage::RequestTransition { module, waypoint } => {
                if self.config.leader_only_transitions && self.leader != Some(id) {
                    self.send(
                        id,
                        &system_msg("Only the party leader can travel to another area."),
                    );
                } else {
                    self.transition(module, waypoint, Some(id));
                }
            }
            ClientMessage::SetGlobal { name, value } => {
                if name.is_empty() || name.len() > MAX_GLOBAL_NAME_LEN {
                    return Flow::Continue;
                }
                if self.data.world.globals.get(&name) != Some(&value) {
                    self.data.world.globals.insert(name.clone(), value.clone());
                    self.dirty = true;
                    self.broadcast_except(id, &ServerMessage::GlobalChanged { name, value });
                }
            }
            ClientMessage::WorldEvent(event) => {
                if event.module().is_empty() || event_tag_len(&event) > MAX_TAG_LEN {
                    return Flow::Continue;
                }
                let module = event.module().to_owned();
                let changed = self
                    .data
                    .world
                    .modules
                    .entry(module)
                    .or_default()
                    .apply(&event);
                if changed {
                    self.dirty = true;
                    self.broadcast_except(id, &ServerMessage::WorldEvent { from: id, event });
                }
            }
            ClientMessage::Dialog(event) => {
                self.broadcast_except(id, &ServerMessage::Dialog { from: id, event });
            }
            ClientMessage::Chat { text } => {
                let text: String = text.trim().chars().take(MAX_CHAT_LEN).collect();
                if !text.is_empty() {
                    let name = self.sessions[&id].name.clone();
                    tracing::info!("<{name}> {text}");
                    self.broadcast(&ServerMessage::Chat {
                        from: Some(id),
                        name,
                        text,
                    });
                }
            }
            ClientMessage::SaveCharacter { data } => {
                if data.len() > MAX_CHARACTER_BYTES {
                    self.send(id, &system_msg("Character data too large; not saved."));
                } else {
                    let name = self.sessions[&id].name.to_lowercase();
                    if let Some(acct) = self.data.accounts.get_mut(&name) {
                        acct.set_character(&data);
                        self.dirty = true;
                    }
                }
            }
        }
        Flow::Continue
    }

    /// Moves the whole party to `module`. `by` is excluded from the broadcast
    /// because their game is already going there.
    pub fn transition(&mut self, module: ModuleName, waypoint: String, by: Option<PlayerId>) {
        tracing::info!("party transition to {module} {waypoint}");
        self.data.world.current_module = Some(module.clone());
        self.data.world.entry_waypoint = waypoint.clone();
        self.dirty = true;
        let msg = ServerMessage::Transition { module, waypoint };
        match by {
            Some(id) => self.broadcast_except(id, &msg),
            None => self.broadcast(&msg),
        }
    }

    /// Runs every server tick, with or without players online.
    pub fn tick(&mut self, dt_secs: f64) {
        self.time_carry += dt_secs * self.config.world_time_scale;
        let whole = self.time_carry.floor();
        if whole >= 1.0 {
            self.data.world.world_time_secs += whole as u64;
            self.time_carry -= whole;
        }

        // Each player only receives states of others in the same module.
        let updates: Vec<(PlayerId, PlayerState)> = self
            .sessions
            .iter_mut()
            .filter(|(_, s)| s.state_dirty)
            .filter_map(|(id, s)| {
                s.state_dirty = false;
                s.state.clone().map(|st| (*id, st))
            })
            .collect();
        if updates.is_empty() {
            return;
        }
        let receivers: Vec<(PlayerId, Option<String>)> = self
            .sessions
            .iter()
            .map(|(id, s)| (*id, s.state.as_ref().map(|st| st.module.clone())))
            .collect();
        for (rid, module) in receivers {
            let batch: Vec<_> = updates
                .iter()
                .filter(|(pid, st)| *pid != rid && module.as_deref().is_none_or(|m| m == st.module))
                .cloned()
                .collect();
            if !batch.is_empty() {
                self.send(rid, &ServerMessage::PlayerStates(batch));
            }
        }
    }

    pub fn kick(&mut self, name: &str, reason: &str) -> bool {
        let Some((&id, _)) = self
            .sessions
            .iter()
            .find(|(_, s)| s.name.eq_ignore_ascii_case(name))
        else {
            return false;
        };
        let kick = self.sessions[&id].kick.clone();
        self.send(
            id,
            &ServerMessage::Shutdown {
                message: format!("Kicked: {reason}"),
            },
        );
        kick.notify_one();
        self.leave(id);
        true
    }

    pub fn shutdown(&mut self, message: &str) {
        self.broadcast(&ServerMessage::Shutdown {
            message: message.into(),
        });
        for s in self.sessions.values() {
            s.kick.notify_one();
        }
    }

    pub fn system_chat(&mut self, text: &str) {
        self.broadcast(&system_msg(text));
    }

    pub fn player_lines(&self) -> Vec<String> {
        self.sessions
            .iter()
            .map(|(id, s)| {
                let lead = if self.leader == Some(*id) {
                    " [leader]"
                } else {
                    ""
                };
                let place = s
                    .state
                    .as_ref()
                    .map(|st| st.module.as_str())
                    .unwrap_or("loading");
                format!("#{id} {}{lead} - {place}", s.name)
            })
            .collect()
    }

    fn player_infos(&self) -> Vec<PlayerInfo> {
        self.sessions
            .iter()
            .map(|(id, s)| PlayerInfo {
                id: *id,
                name: s.name.clone(),
                state: s.state.clone(),
            })
            .collect()
    }

    fn send(&mut self, id: PlayerId, msg: &ServerMessage) {
        let Ok(frame) = encode_frame(msg) else { return };
        self.send_frame(id, Arc::new(frame));
    }

    fn send_frame(&mut self, id: PlayerId, frame: Arc<Vec<u8>>) {
        let Some(s) = self.sessions.get(&id) else {
            return;
        };
        if s.tx.try_send(frame).is_err() {
            tracing::warn!("{} is not keeping up (or gone); dropping", s.name);
            s.kick.notify_one();
            self.leave(id);
        }
    }

    pub fn broadcast(&mut self, msg: &ServerMessage) {
        self.broadcast_filtered(None, msg);
    }

    fn broadcast_except(&mut self, except: PlayerId, msg: &ServerMessage) {
        self.broadcast_filtered(Some(except), msg);
    }

    fn broadcast_filtered(&mut self, except: Option<PlayerId>, msg: &ServerMessage) {
        let Ok(frame) = encode_frame(msg) else { return };
        let frame = Arc::new(frame);
        let ids: Vec<PlayerId> = self
            .sessions
            .keys()
            .copied()
            .filter(|id| Some(*id) != except)
            .collect();
        for id in ids {
            self.send_frame(id, frame.clone());
        }
    }
}

fn system_msg(text: &str) -> ServerMessage {
    ServerMessage::Chat {
        from: None,
        name: "Server".into(),
        text: text.into(),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_')
}

fn event_tag_len(event: &WorldEvent) -> usize {
    match event {
        WorldEvent::CreatureDied { tag, .. }
        | WorldEvent::PlaceableOpened { tag, .. }
        | WorldEvent::PlaceableLooted { tag, .. }
        | WorldEvent::DoorOpened { tag, .. }
        | WorldEvent::DoorUnlocked { tag, .. }
        | WorldEvent::ItemPickedUp { tag, .. } => tag.len(),
        WorldEvent::LocalFlag { key, .. } => key.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(name: &str) -> Hello {
        Hello {
            protocol_version: PROTOCOL_VERSION,
            mod_version: "test".into(),
            player_name: name.into(),
            player_token: format!("{name}-token-0123456789"),
            password: None,
            game_build: None,
        }
    }

    fn drain(rx: &mut mpsc::Receiver<Arc<Vec<u8>>>) -> Vec<ServerMessage> {
        let mut out = Vec::new();
        while let Ok(f) = rx.try_recv() {
            out.push(decode_payload(&f[4..]).unwrap());
        }
        out
    }

    #[test]
    fn leader_transition_moves_party_and_persists() {
        let mut hub = Hub::new(Config::default(), PersistentWorld::default());
        let (tx1, mut rx1) = mpsc::channel(OUTBOX_CAPACITY);
        let (tx2, mut rx2) = mpsc::channel(OUTBOX_CAPACITY);
        let a = hub.join(&hello("Revan"), tx1).unwrap().id;
        let b = hub.join(&hello("Carth"), tx2).unwrap().id;
        drain(&mut rx1);
        drain(&mut rx2);

        // Non-leader is refused.
        hub.handle(
            b,
            ClientMessage::RequestTransition {
                module: "x".into(),
                waypoint: "".into(),
            },
        );
        assert_eq!(hub.current_module(), None);

        hub.handle(
            a,
            ClientMessage::RequestTransition {
                module: "tar_m02aa".into(),
                waypoint: "wp_start".into(),
            },
        );
        assert_eq!(hub.current_module(), Some("tar_m02aa"));
        assert!(drain(&mut rx2)
            .iter()
            .any(|m| matches!(m, ServerMessage::Transition { .. })));
        assert!(!drain(&mut rx1)
            .iter()
            .any(|m| matches!(m, ServerMessage::Transition { .. })));

        hub.leave(a);
        assert!(drain(&mut rx2).contains(&ServerMessage::LeaderChanged { id: b }));
    }

    #[test]
    fn rejects_bad_handshakes() {
        let cfg = Config {
            password: "hawk".into(),
            max_players: 1,
            ..Default::default()
        };
        let mut hub = Hub::new(cfg, PersistentWorld::default());
        // Keep receivers alive: a closed outbox makes the hub drop the player.
        let mut keep = Vec::new();
        let mut tx = || {
            let (tx, rx) = mpsc::channel(OUTBOX_CAPACITY);
            keep.push(rx);
            tx
        };

        let mut h = hello("Mission");
        assert_eq!(
            hub.join(&h, tx()).err().unwrap().0,
            RejectReason::BadPassword
        );
        h.password = Some("hawk".into());
        h.protocol_version = 0;
        assert_eq!(
            hub.join(&h, tx()).err().unwrap().0,
            RejectReason::VersionMismatch
        );
        h.protocol_version = PROTOCOL_VERSION;
        h.player_name = "<script>".into();
        assert_eq!(
            hub.join(&h, tx()).err().unwrap().0,
            RejectReason::InvalidName
        );
        h.player_name = "Mission".into();
        hub.join(&h, tx()).unwrap();
        assert_eq!(hub.join(&h, tx()).err().unwrap().0, RejectReason::NameTaken);
        let mut z = hello("Zaalbar");
        z.password = Some("hawk".into());
        assert_eq!(
            hub.join(&z, tx()).err().unwrap().0,
            RejectReason::ServerFull
        );
    }

    #[test]
    fn world_time_advances_without_players() {
        let mut hub = Hub::new(Config::default(), PersistentWorld::default());
        for _ in 0..40 {
            hub.tick(0.05);
        }
        assert_eq!(hub.data.world.world_time_secs, 2);
    }

    #[test]
    fn states_only_go_to_players_in_same_module() {
        let mut hub = Hub::new(Config::default(), PersistentWorld::default());
        let (t1, mut r1) = mpsc::channel(OUTBOX_CAPACITY);
        let (t2, mut r2) = mpsc::channel(OUTBOX_CAPACITY);
        let (t3, mut r3) = mpsc::channel(OUTBOX_CAPACITY);
        let a = hub.join(&hello("A"), t1).unwrap().id;
        let b = hub.join(&hello("B"), t2).unwrap().id;
        let c = hub.join(&hello("C"), t3).unwrap().id;
        let st = |m: &str| PlayerState {
            module: m.into(),
            ..Default::default()
        };
        hub.handle(a, ClientMessage::PlayerState(st("m1")));
        hub.handle(b, ClientMessage::PlayerState(st("m1")));
        hub.handle(c, ClientMessage::PlayerState(st("m2")));
        for r in [&mut r1, &mut r2, &mut r3] {
            drain(r);
        }
        hub.tick(0.05);
        let got = |r: &mut mpsc::Receiver<_>| -> Vec<PlayerId> {
            drain(r)
                .into_iter()
                .flat_map(|m| match m {
                    ServerMessage::PlayerStates(v) => v.into_iter().map(|(id, _)| id).collect(),
                    _ => vec![],
                })
                .collect()
        };
        assert_eq!(got(&mut r1), vec![b]);
        assert_eq!(got(&mut r2), vec![a]);
        assert!(got(&mut r3).is_empty());
    }
}
