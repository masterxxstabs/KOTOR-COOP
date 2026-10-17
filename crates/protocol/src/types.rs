use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Server-assigned id for a connected player. Not stable across reconnects.
pub type PlayerId = u32;

/// Module resource name, e.g. `end_m01aa` (Endar Spire) or `tar_m02aa` (Taris).
pub type ModuleName = String;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn distance(self, other: Vec3) -> f32 {
        let (dx, dy, dz) = (self.x - other.x, self.y - other.y, self.z - other.z);
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

/// Snapshot of one player's character, streamed to the other players so they
/// can drive a "puppet" creature in their own game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub module: ModuleName,
    pub position: Vec3,
    /// Facing in radians, as stored by the engine.
    pub facing: f32,
    /// Engine animation id currently playing (0 = idle/unknown).
    pub animation: u16,
    pub hp: i32,
    pub max_hp: i32,
    /// Row in appearance.2da so puppets use the right model.
    pub appearance: u16,
    pub in_combat: bool,
    pub in_dialog: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub state: Option<PlayerState>,
}

/// Value of a KOTOR global variable. The engine has booleans, numbers
/// (signed bytes in K1 but we keep i32 for headroom), strings and locations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GlobalValue {
    Boolean(bool),
    Number(i32),
    String(String),
}

/// Persistent changes to a module that must look the same for everyone and
/// survive while nobody is online.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorldEvent {
    CreatureDied {
        module: ModuleName,
        tag: String,
    },
    PlaceableOpened {
        module: ModuleName,
        tag: String,
    },
    PlaceableLooted {
        module: ModuleName,
        tag: String,
    },
    DoorOpened {
        module: ModuleName,
        tag: String,
    },
    DoorUnlocked {
        module: ModuleName,
        tag: String,
    },
    ItemPickedUp {
        module: ModuleName,
        tag: String,
    },
    /// Escape hatch for scripted one-off flags inside a module.
    LocalFlag {
        module: ModuleName,
        key: String,
        value: i32,
    },
}

impl WorldEvent {
    pub fn module(&self) -> &str {
        match self {
            WorldEvent::CreatureDied { module, .. }
            | WorldEvent::PlaceableOpened { module, .. }
            | WorldEvent::PlaceableLooted { module, .. }
            | WorldEvent::DoorOpened { module, .. }
            | WorldEvent::DoorUnlocked { module, .. }
            | WorldEvent::ItemPickedUp { module, .. }
            | WorldEvent::LocalFlag { module, .. } => module,
        }
    }
}

/// Conversation sync. The player who starts a dialog owns it; the others see
/// the same lines and may vote on replies (server relays, owner decides).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DialogEvent {
    Started { dialog: String, speaker_tag: String },
    Line { entry_index: u32, text: String },
    Replies { options: Vec<(u32, String)> },
    Vote { reply_index: u32 },
    Chosen { reply_index: u32 },
    Ended,
}

/// Per-module persistent state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModuleState {
    pub dead_creatures: Vec<String>,
    pub opened_placeables: Vec<String>,
    pub looted_placeables: Vec<String>,
    pub opened_doors: Vec<String>,
    pub unlocked_doors: Vec<String>,
    pub picked_up_items: Vec<String>,
    pub flags: BTreeMap<String, i32>,
}

impl ModuleState {
    /// Applies an event. Returns `false` when it changed nothing (duplicate).
    pub fn apply(&mut self, event: &WorldEvent) -> bool {
        fn add(list: &mut Vec<String>, tag: &str) -> bool {
            if list.iter().any(|t| t == tag) {
                false
            } else {
                list.push(tag.to_owned());
                true
            }
        }
        match event {
            WorldEvent::CreatureDied { tag, .. } => add(&mut self.dead_creatures, tag),
            WorldEvent::PlaceableOpened { tag, .. } => add(&mut self.opened_placeables, tag),
            WorldEvent::PlaceableLooted { tag, .. } => add(&mut self.looted_placeables, tag),
            WorldEvent::DoorOpened { tag, .. } => add(&mut self.opened_doors, tag),
            WorldEvent::DoorUnlocked { tag, .. } => add(&mut self.unlocked_doors, tag),
            WorldEvent::ItemPickedUp { tag, .. } => add(&mut self.picked_up_items, tag),
            WorldEvent::LocalFlag { key, value, .. } => {
                self.flags.insert(key.clone(), *value) != Some(*value)
            }
        }
    }
}

/// Everything a joining client needs to bring its game in line with the
/// persistent world.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    /// Module the party is currently in (None on a fresh world: start a new game).
    pub current_module: Option<ModuleName>,
    pub entry_waypoint: String,
    pub globals: BTreeMap<String, GlobalValue>,
    pub modules: BTreeMap<ModuleName, ModuleState>,
    /// In-game seconds elapsed; keeps ticking while the server is empty.
    pub world_time_secs: u64,
}
