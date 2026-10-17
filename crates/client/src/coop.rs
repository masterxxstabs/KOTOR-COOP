//! Platform-independent co-op logic, run once per rendered frame on the game
//! thread. Talks to the game through [`GameBridge`] and to the server through
//! [`Link`], so it can be tested without either.

use kotor_protocol::*;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// What the DLL can do inside the engine. Every method may be unsupported on a
/// build whose offsets aren't mapped yet: getters return `None`, actions
/// return `false`, and the co-op layer degrades instead of crashing.
pub trait GameBridge {
    /// Current module resref, `None` in menus or while loading.
    fn current_module(&mut self) -> Option<String>;
    fn local_player(&mut self) -> Option<PlayerState>;
    fn read_global(&mut self, name: &str, kind: GlobalKind) -> Option<GlobalValue>;
    fn write_global(&mut self, name: &str, value: &GlobalValue) -> bool;
    /// Show/move another player's character. `false` = puppets unsupported.
    fn update_puppet(&mut self, id: PlayerId, name: &str, state: &PlayerState) -> bool;
    fn remove_puppet(&mut self, id: PlayerId);
    /// Load a module (party transition). `false` = the player must walk there.
    fn travel(&mut self, module: &str, waypoint: &str) -> bool;
    /// Apply saved changes (remove dead NPCs, open doors...) after a module loads.
    fn apply_module_state(&mut self, module: &str, state: &ModuleState) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalKind {
    Boolean,
    Number,
}

/// Events from the network thread.
#[derive(Debug)]
pub enum NetEvent {
    Connected(Welcome),
    Message(ServerMessage),
    /// Lost or failed connection; the network thread retries on its own.
    Disconnected(String),
}

pub trait Link {
    fn send(&mut self, msg: ClientMessage);
    fn poll(&mut self) -> Option<NetEvent>;
}

const STATE_INTERVAL: Duration = Duration::from_millis(50);
const GLOBAL_POLL_INTERVAL: Duration = Duration::from_millis(500);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CHAT_LINES: usize = 8;

#[derive(Debug, Clone)]
pub struct RemotePlayer {
    pub name: String,
    pub state: Option<PlayerState>,
    pub shown: bool,
}

pub struct Coop<B: GameBridge, L: Link> {
    pub bridge: B,
    pub link: L,
    pub my_id: Option<PlayerId>,
    pub server_name: String,
    pub leader: Option<PlayerId>,
    pub players: BTreeMap<PlayerId, RemotePlayer>,
    pub chat: VecDeque<String>,
    pub status: String,
    pub ping_ms: Option<u64>,
    world: WorldSnapshot,
    /// Last value of each global as agreed with the server; used to spot local changes.
    known_globals: BTreeMap<String, GlobalValue>,
    tracked: Vec<(String, GlobalKind)>,
    globals_pending: bool,
    local_module: Option<String>,
    pending_travel: Option<(String, String)>,
    travel_notice_for: Option<String>,
    last_state: Option<Instant>,
    last_global_poll: Option<Instant>,
    last_ping: Option<Instant>,
    epoch: Instant,
}

impl<B: GameBridge, L: Link> Coop<B, L> {
    pub fn new(bridge: B, link: L, tracked: Vec<(String, GlobalKind)>) -> Self {
        Self {
            bridge,
            link,
            my_id: None,
            server_name: String::new(),
            leader: None,
            players: BTreeMap::new(),
            chat: VecDeque::new(),
            status: "Connecting...".into(),
            ping_ms: None,
            world: WorldSnapshot::default(),
            known_globals: BTreeMap::new(),
            tracked,
            globals_pending: false,
            local_module: None,
            pending_travel: None,
            travel_notice_for: None,
            last_state: None,
            last_global_poll: None,
            last_ping: None,
            epoch: Instant::now(),
        }
    }

    pub fn is_leader(&self) -> bool {
        self.my_id.is_some() && self.my_id == self.leader
    }

    pub fn say(&mut self, text: &str) {
        let text = text.trim();
        if !text.is_empty() {
            self.link.send(ClientMessage::Chat {
                text: text.to_owned(),
            });
        }
    }

    fn log(&mut self, line: String) {
        if self.chat.len() == CHAT_LINES {
            self.chat.pop_front();
        }
        self.chat.push_back(line);
    }

    /// Called once per frame from the render hook.
    pub fn on_frame(&mut self, now: Instant) {
        while let Some(ev) = self.link.poll() {
            self.on_net(ev);
        }
        if self.my_id.is_none() {
            return;
        }

        let module = self.bridge.current_module();
        if module != self.local_module {
            self.local_module = module.clone();
            if let Some(m) = &module {
                self.on_module_loaded(m.clone());
            }
        }
        let Some(module) = module else { return };

        if self.globals_pending {
            self.globals_pending = false;
            let globals: Vec<_> = self
                .known_globals
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in globals {
                self.bridge.write_global(&k, &v);
            }
        }

        if let Some((target, wp)) = self.pending_travel.clone() {
            if target == module || self.bridge.travel(&target, &wp) {
                self.pending_travel = None;
            } else if self.travel_notice_for.as_deref() != Some(target.as_str()) {
                self.travel_notice_for = Some(target.clone());
                self.log(format!(
                    "* The party is in {target}. Travel there to regroup."
                ));
            }
        }

        if due(&mut self.last_state, now, STATE_INTERVAL) {
            if let Some(mut st) = self.bridge.local_player() {
                st.module = module.clone();
                self.link.send(ClientMessage::PlayerState(st));
            }
        }
        if due(&mut self.last_global_poll, now, GLOBAL_POLL_INTERVAL) {
            self.poll_globals();
        }
        if due(&mut self.last_ping, now, PING_INTERVAL) {
            let nonce = now.duration_since(self.epoch).as_millis() as u64;
            self.link.send(ClientMessage::Ping { nonce });
        }
    }

    fn on_module_loaded(&mut self, module: String) {
        self.link.send(ClientMessage::EnteredModule {
            module: module.clone(),
        });
        if let Some(state) = self.world.modules.get(&module).cloned() {
            self.bridge.apply_module_state(&module, &state);
        }
        // Puppets belong to a module; respawn the ones that are here.
        let ids: Vec<PlayerId> = self.players.keys().copied().collect();
        for id in ids {
            let p = self.players.get_mut(&id).unwrap();
            if p.shown {
                p.shown = false;
                self.bridge.remove_puppet(id);
            }
        }
    }

    fn poll_globals(&mut self) {
        let tracked = self.tracked.clone();
        for (name, kind) in tracked {
            let Some(value) = self.bridge.read_global(&name, kind) else {
                continue;
            };
            if self.known_globals.get(&name) != Some(&value) {
                self.known_globals.insert(name.clone(), value.clone());
                self.link.send(ClientMessage::SetGlobal { name, value });
            }
        }
    }

    fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Connected(w) => {
                self.my_id = Some(w.your_id);
                self.leader = w.leader;
                self.server_name = w.server_name.clone();
                self.status = format!("Connected to {}", w.server_name);
                self.players = w
                    .players
                    .into_iter()
                    .filter(|p| p.id != w.your_id)
                    .map(|p| {
                        (
                            p.id,
                            RemotePlayer {
                                name: p.name,
                                state: p.state,
                                shown: false,
                            },
                        )
                    })
                    .collect();
                self.known_globals = w.world.globals.clone();
                self.globals_pending = !self.known_globals.is_empty();
                if let Some(m) = &w.world.current_module {
                    self.pending_travel = Some((m.clone(), w.world.entry_waypoint.clone()));
                }
                // Force module state to be re-applied for the module we're already in.
                self.local_module = None;
                self.world = w.world;
                if !w.motd.is_empty() {
                    self.log(format!("* {}", w.motd));
                }
            }
            NetEvent::Disconnected(why) => {
                if self.my_id.is_some() {
                    self.log(format!("* Disconnected: {why}"));
                }
                self.status = format!("Reconnecting... ({why})");
                self.my_id = None;
                let ids: Vec<_> = self.players.keys().copied().collect();
                for id in ids {
                    self.bridge.remove_puppet(id);
                }
                self.players.clear();
            }
            NetEvent::Message(m) => self.on_message(m),
        }
    }

    fn on_message(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::Welcome(_) | ServerMessage::Rejected { .. } => {}
            ServerMessage::Pong { nonce } => {
                let now = self.epoch.elapsed().as_millis() as u64;
                self.ping_ms = Some(now.saturating_sub(nonce));
            }
            ServerMessage::PlayerJoined(p) => {
                self.players.insert(
                    p.id,
                    RemotePlayer {
                        name: p.name,
                        state: p.state,
                        shown: false,
                    },
                );
            }
            ServerMessage::PlayerLeft { id } => {
                if self.players.remove(&id).is_some_and(|p| p.shown) {
                    self.bridge.remove_puppet(id);
                }
            }
            ServerMessage::PlayerStates(states) => {
                for (id, st) in states {
                    let here = self.local_module.as_deref() == Some(st.module.as_str());
                    let Some(p) = self.players.get_mut(&id) else {
                        continue;
                    };
                    if here {
                        p.shown = self.bridge.update_puppet(id, &p.name, &st);
                    } else if p.shown {
                        p.shown = false;
                        self.bridge.remove_puppet(id);
                    }
                    p.state = Some(st);
                }
            }
            ServerMessage::LeaderChanged { id } => {
                self.leader = Some(id);
                let who = if Some(id) == self.my_id {
                    "You are".to_owned()
                } else {
                    format!(
                        "{} is",
                        self.players
                            .get(&id)
                            .map(|p| p.name.as_str())
                            .unwrap_or("?")
                    )
                };
                self.log(format!("* {who} now the party leader"));
            }
            ServerMessage::Transition { module, waypoint } => {
                self.world.current_module = Some(module.clone());
                self.pending_travel = Some((module, waypoint));
            }
            ServerMessage::GlobalChanged { name, value } => {
                self.known_globals.insert(name.clone(), value.clone());
                self.bridge.write_global(&name, &value);
            }
            ServerMessage::WorldEvent { event, .. } => {
                let module = event.module().to_owned();
                let st = self.world.modules.entry(module.clone()).or_default();
                st.apply(&event);
                if self.local_module.as_deref() == Some(module.as_str()) {
                    let st = st.clone();
                    self.bridge.apply_module_state(&module, &st);
                }
            }
            ServerMessage::Dialog { from, event } => {
                if let DialogEvent::Line { text, .. } = event {
                    let who = self
                        .players
                        .get(&from)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    self.log(format!("[{who}'s conversation] {text}"));
                }
            }
            ServerMessage::Chat { name, text, from } => {
                let line = if from.is_none() {
                    format!("* {text}")
                } else {
                    format!("<{name}> {text}")
                };
                self.log(line);
            }
            ServerMessage::CharacterData { .. } => {
                // Character import needs engine support (see docs); kept server-side for now.
            }
            ServerMessage::Shutdown { message } => {
                self.log(format!("* {message}"));
            }
        }
    }
}

fn due(last: &mut Option<Instant>, now: Instant, every: Duration) -> bool {
    if last.is_some_and(|t| now.duration_since(t) < every) {
        return false;
    }
    *last = Some(now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockGame {
        module: Option<String>,
        globals: BTreeMap<String, GlobalValue>,
        puppets: BTreeMap<PlayerId, PlayerState>,
        travelled: Vec<String>,
        can_travel: bool,
    }

    impl GameBridge for MockGame {
        fn current_module(&mut self) -> Option<String> {
            self.module.clone()
        }
        fn local_player(&mut self) -> Option<PlayerState> {
            Some(PlayerState {
                hp: 10,
                ..Default::default()
            })
        }
        fn read_global(&mut self, name: &str, _: GlobalKind) -> Option<GlobalValue> {
            self.globals.get(name).cloned()
        }
        fn write_global(&mut self, name: &str, value: &GlobalValue) -> bool {
            self.globals.insert(name.into(), value.clone());
            true
        }
        fn update_puppet(&mut self, id: PlayerId, _: &str, st: &PlayerState) -> bool {
            self.puppets.insert(id, st.clone());
            true
        }
        fn remove_puppet(&mut self, id: PlayerId) {
            self.puppets.remove(&id);
        }
        fn travel(&mut self, module: &str, _: &str) -> bool {
            if self.can_travel {
                self.travelled.push(module.into());
                self.module = Some(module.into());
            }
            self.can_travel
        }
        fn apply_module_state(&mut self, _: &str, _: &ModuleState) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct MockLink {
        inbox: VecDeque<NetEvent>,
        sent: Vec<ClientMessage>,
    }

    impl Link for MockLink {
        fn send(&mut self, msg: ClientMessage) {
            self.sent.push(msg);
        }
        fn poll(&mut self) -> Option<NetEvent> {
            self.inbox.pop_front()
        }
    }

    fn welcome(module: Option<&str>) -> Welcome {
        let mut world = WorldSnapshot {
            current_module: module.map(Into::into),
            ..Default::default()
        };
        world.globals.insert("PLOT".into(), GlobalValue::Number(3));
        Welcome {
            your_id: 2,
            server_name: "S".into(),
            motd: String::new(),
            leader: Some(1),
            players: vec![
                PlayerInfo {
                    id: 1,
                    name: "Bastila".into(),
                    state: None,
                },
                PlayerInfo {
                    id: 2,
                    name: "Me".into(),
                    state: None,
                },
            ],
            world,
        }
    }

    fn coop(game: MockGame) -> Coop<MockGame, MockLink> {
        Coop::new(
            game,
            MockLink::default(),
            vec![("PLOT".into(), GlobalKind::Number)],
        )
    }

    #[test]
    fn join_applies_globals_and_travels_to_party() {
        let mut c = coop(MockGame {
            module: Some("end_m01aa".into()),
            can_travel: true,
            ..Default::default()
        });
        c.link
            .inbox
            .push_back(NetEvent::Connected(welcome(Some("tar_m02aa"))));
        c.on_frame(Instant::now());
        assert_eq!(c.bridge.globals["PLOT"], GlobalValue::Number(3));
        assert_eq!(c.bridge.travelled, vec!["tar_m02aa".to_string()]);
        assert!(c
            .link
            .sent
            .iter()
            .any(|m| matches!(m, ClientMessage::PlayerState(_))));
        // The applied global must not be echoed back as a local change.
        assert!(!c
            .link
            .sent
            .iter()
            .any(|m| matches!(m, ClientMessage::SetGlobal { .. })));
    }

    #[test]
    fn local_global_change_is_sent_once() {
        let mut c = coop(MockGame {
            module: Some("m".into()),
            ..Default::default()
        });
        c.link.inbox.push_back(NetEvent::Connected(welcome(None)));
        let t0 = Instant::now();
        c.on_frame(t0);
        c.bridge
            .globals
            .insert("PLOT".into(), GlobalValue::Number(4));
        c.on_frame(t0 + Duration::from_secs(1));
        c.on_frame(t0 + Duration::from_secs(2));
        let sets: Vec<_> = c
            .link
            .sent
            .iter()
            .filter(|m| matches!(m, ClientMessage::SetGlobal { .. }))
            .collect();
        assert_eq!(sets.len(), 1);
    }

    #[test]
    fn puppets_follow_module_and_disconnect() {
        let mut c = coop(MockGame {
            module: Some("m1".into()),
            ..Default::default()
        });
        c.link.inbox.push_back(NetEvent::Connected(welcome(None)));
        c.on_frame(Instant::now());
        let st = |m: &str| PlayerState {
            module: m.into(),
            ..Default::default()
        };
        c.link
            .inbox
            .push_back(NetEvent::Message(ServerMessage::PlayerStates(vec![(
                1,
                st("m1"),
            )])));
        c.on_frame(Instant::now());
        assert!(c.bridge.puppets.contains_key(&1));
        c.link
            .inbox
            .push_back(NetEvent::Message(ServerMessage::PlayerStates(vec![(
                1,
                st("m2"),
            )])));
        c.on_frame(Instant::now());
        assert!(!c.bridge.puppets.contains_key(&1));
        c.link
            .inbox
            .push_back(NetEvent::Message(ServerMessage::PlayerStates(vec![(
                1,
                st("m1"),
            )])));
        c.link.inbox.push_back(NetEvent::Disconnected("x".into()));
        c.on_frame(Instant::now());
        assert!(c.bridge.puppets.is_empty());
        assert!(c.my_id.is_none());
    }

    #[test]
    fn unsupported_travel_tells_player_once() {
        let mut c = coop(MockGame {
            module: Some("m1".into()),
            ..Default::default()
        });
        c.link
            .inbox
            .push_back(NetEvent::Connected(welcome(Some("m2"))));
        c.on_frame(Instant::now());
        c.on_frame(Instant::now());
        assert_eq!(
            c.chat.iter().filter(|l| l.contains("Travel there")).count(),
            1
        );
    }
}
