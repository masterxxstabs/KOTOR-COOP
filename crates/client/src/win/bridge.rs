//! `GameBridge` for the real Odyssey engine, driven entirely by offsets.toml.

use super::mem;
use crate::config::{BuildOffsets, PointerPath};
use crate::coop::{GameBridge, GlobalKind};
use kotor_protocol::*;

pub struct OdysseyBridge {
    position: Option<PointerPath>,
    facing: Option<PointerPath>,
    hp: Option<PointerPath>,
    max_hp: Option<PointerPath>,
    appearance: Option<PointerPath>,
    module_name: Option<PointerPath>,
    loading: Option<PointerPath>,
    globals_table: Option<PointerPath>,
    set_bool: Option<PointerPath>,
    set_num: Option<PointerPath>,
    get_bool: Option<PointerPath>,
    get_num: Option<PointerPath>,
}

/// Odyssey/Aurora engine string: `{ char* data; u32 length; }`.
#[repr(C)]
struct CExoString {
    data: *const u8,
    len: u32,
}

impl OdysseyBridge {
    pub fn new(o: BuildOffsets) -> Self {
        let p = |s: &str| PointerPath::parse(s);
        Self {
            position: p(&o.player_position),
            facing: p(&o.player_facing),
            hp: p(&o.player_hp),
            max_hp: p(&o.player_max_hp),
            appearance: p(&o.player_appearance),
            module_name: p(&o.module_name),
            loading: p(&o.loading_flag),
            globals_table: p(&o.functions.globals_table),
            set_bool: p(&o.functions.set_global_boolean),
            set_num: p(&o.functions.set_global_number),
            get_bool: p(&o.functions.get_global_boolean),
            get_num: p(&o.functions.get_global_number),
        }
    }

    pub fn capability_report(&self) -> Vec<String> {
        let f = |name: &str, ok: bool| {
            format!("  {name:<22} {}", if ok { "enabled" } else { "not mapped" })
        };
        vec![
            "engine features for this build:".into(),
            f("module detection", self.module_name.is_some()),
            f("position sync", self.position.is_some()),
            f(
                "plot global sync",
                self.globals_table.is_some() && self.get_num.is_some(),
            ),
            f("remote player puppets", false),
            f("party travel", false),
            f("world state replay", false),
        ]
    }

    fn read_at<T: Copy + Default>(path: &Option<PointerPath>) -> Option<T> {
        mem::read(mem::resolve(path.as_ref()?)?)
    }

    fn function(path: &Option<PointerPath>) -> Option<usize> {
        mem::resolve(path.as_ref()?).filter(|&a| a != 0)
    }

    fn globals_this(&self) -> Option<usize> {
        let p: u32 = Self::read_at(&self.globals_table)?;
        (p != 0).then_some(p as usize)
    }
}

impl GameBridge for OdysseyBridge {
    fn current_module(&mut self) -> Option<String> {
        if Self::read_at::<u8>(&self.loading).unwrap_or(0) != 0 {
            return None;
        }
        let name = mem::read_cstr(mem::resolve(self.module_name.as_ref()?)?, 32)?;
        let ok = !name.is_empty()
            && name.len() <= 16
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        ok.then(|| name.to_ascii_lowercase())
    }

    fn local_player(&mut self) -> Option<PlayerState> {
        let [x, y, z]: [f32; 3] = Self::read_at(&self.position)?;
        let position = Vec3::new(x, y, z);
        if !position.is_finite() {
            return None;
        }
        Some(PlayerState {
            module: String::new(),
            position,
            facing: Self::read_at(&self.facing).unwrap_or(0.0),
            animation: 0,
            hp: Self::read_at(&self.hp).unwrap_or(0),
            max_hp: Self::read_at(&self.max_hp).unwrap_or(0),
            appearance: Self::read_at(&self.appearance).unwrap_or(0),
            in_combat: false,
            in_dialog: false,
        })
    }

    fn read_global(&mut self, name: &str, kind: GlobalKind) -> Option<GlobalValue> {
        let this = self.globals_this()?;
        let f = Self::function(match kind {
            GlobalKind::Boolean => &self.get_bool,
            GlobalKind::Number => &self.get_num,
        })?;
        let cname = std::ffi::CString::new(name).ok()?;
        let s = CExoString {
            data: cname.as_ptr().cast(),
            len: name.len() as u32,
        };
        let mut out: i32 = 0;
        let found = unsafe {
            let f: unsafe extern "thiscall" fn(usize, *const CExoString, *mut i32) -> i32 =
                std::mem::transmute(f);
            f(this, &s, &mut out)
        };
        (found != 0).then_some(match kind {
            GlobalKind::Boolean => GlobalValue::Boolean(out != 0),
            GlobalKind::Number => GlobalValue::Number(out),
        })
    }

    fn write_global(&mut self, name: &str, value: &GlobalValue) -> bool {
        let (fpath, v) = match value {
            GlobalValue::Boolean(b) => (&self.set_bool, *b as i32),
            GlobalValue::Number(n) => (&self.set_num, *n),
            GlobalValue::String(_) => return false,
        };
        let (Some(this), Some(f)) = (self.globals_this(), Self::function(fpath)) else {
            return false;
        };
        let Ok(cname) = std::ffi::CString::new(name) else {
            return false;
        };
        let s = CExoString {
            data: cname.as_ptr().cast(),
            len: name.len() as u32,
        };
        unsafe {
            let f: unsafe extern "thiscall" fn(usize, *const CExoString, i32) =
                std::mem::transmute(f);
            f(this, &s, v);
        }
        true
    }

    // The following need engine calls nobody has mapped yet (creature spawn,
    // module load, object lookup by tag). They report "unsupported" so the
    // co-op layer falls back to overlay hints. See docs/REVERSE_ENGINEERING.md.
    fn update_puppet(&mut self, _id: PlayerId, _name: &str, _state: &PlayerState) -> bool {
        false
    }

    fn remove_puppet(&mut self, _id: PlayerId) {}

    fn travel(&mut self, _module: &str, _waypoint: &str) -> bool {
        false
    }

    fn apply_module_state(&mut self, _module: &str, _state: &ModuleState) -> bool {
        false
    }
}
