//! Guided offset discovery. Each round uses a fresh save as ground truth:
//!
//! * Round 1: find every address holding the save's module name / position
//!   (and HP etc. near the position), then every static pointer path to them.
//! * Later rounds (after changing area and saving again): drop every
//!   candidate that no longer reads the save's values.
//!
//! Candidates are pointer paths relative to the exe, so rounds can span game
//! restarts, which is what removes paths that only worked by luck.

use crate::save::PlayerSnapshot;
use crate::scan::{self, Chain, Memory, PointerScanParams, Scalar};
use serde::{Deserialize, Serialize};

/// How far from the position HP/appearance/facing may sit in the same object.
pub const NEAR_RADIUS: usize = 0x2000;
const POS_EPS: f32 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NearField {
    PlayerHp,
    PlayerMaxHp,
    PlayerAppearance,
    PlayerFacing,
}

impl NearField {
    pub const ALL: [NearField; 4] = [
        NearField::PlayerHp,
        NearField::PlayerMaxHp,
        NearField::PlayerAppearance,
        NearField::PlayerFacing,
    ];

    pub fn key(self) -> &'static str {
        match self {
            NearField::PlayerHp => "player_hp",
            NearField::PlayerMaxHp => "player_max_hp",
            NearField::PlayerAppearance => "player_appearance",
            NearField::PlayerFacing => "player_facing",
        }
    }

    fn expected(self, s: &PlayerSnapshot) -> Option<f64> {
        match self {
            NearField::PlayerHp => s.hp.map(|v| v as f64),
            NearField::PlayerMaxHp => s.max_hp.map(|v| v as f64),
            NearField::PlayerAppearance => s.appearance.map(|v| v as f64),
            NearField::PlayerFacing => s.facing.map(|v| v as f64),
        }
    }

    fn scalars(self) -> &'static [Scalar] {
        match self {
            NearField::PlayerFacing => &[Scalar::F32],
            NearField::PlayerAppearance => &[Scalar::U16],
            // Narrowest first: reading 16 bits of a 32-bit HP field is still
            // correct for any real HP, while the reverse picks up neighbours.
            _ => &[Scalar::I16, Scalar::I32],
        }
    }

    fn eps(self) -> f64 {
        if self == NearField::PlayerFacing {
            0.01
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NearCandidate {
    pub field: NearField,
    /// Byte offset from the player position.
    pub delta: i32,
    pub scalar: Scalar,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProbeState {
    pub exe_sha256: String,
    pub rounds: u32,
    pub module_name: Vec<Chain>,
    pub position: Vec<Chain>,
    pub near: Vec<NearCandidate>,
    /// Values seen in each round; pruning only proves something when they change.
    pub seen_modules: Vec<String>,
    pub seen_positions: Vec<[f32; 3]>,
    pub seen_near: Vec<(NearField, f64)>,
}

#[derive(Debug, Default)]
pub struct RoundReport {
    pub round: u32,
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

fn read_cstr(mem: &dyn Memory, addr: usize, max: usize) -> Option<String> {
    let mut buf = vec![0u8; max];
    // Strings near the end of a region: shrink until the read succeeds.
    let mut len = max;
    while len > 0 && !mem.read(addr, &mut buf[..len]) {
        len /= 2;
    }
    let end = buf[..len].iter().position(|&b| b == 0)?;
    String::from_utf8(buf[..end].to_vec()).ok()
}

fn position_matches(mem: &dyn Memory, addr: usize, want: [f32; 3]) -> bool {
    (0..3).all(|i| {
        Scalar::F32
            .read(mem, addr + i * 4)
            .is_some_and(|v| (v as f32 - want[i]).abs() <= POS_EPS)
    })
}

impl ProbeState {
    pub fn new(exe_sha256: &str) -> Self {
        Self {
            exe_sha256: exe_sha256.to_owned(),
            ..Default::default()
        }
    }

    pub fn round(
        &mut self,
        mem: &dyn Memory,
        snap: &PlayerSnapshot,
        params: PointerScanParams,
    ) -> RoundReport {
        let mut rep = RoundReport {
            round: self.rounds + 1,
            ..Default::default()
        };
        let first = self.rounds == 0;

        // --- module name -------------------------------------------------
        if first {
            let mut needle = snap.module.as_bytes().to_vec();
            needle.push(0);
            let mut hits = scan::find_bytes(mem, &needle, 5000);
            let upper = snap.module.to_ascii_uppercase();
            if upper != snap.module {
                let mut n2 = upper.into_bytes();
                n2.push(0);
                hits.extend(scan::find_bytes(mem, &n2, 5000));
            }
            rep.lines.push(format!(
                "module name '{}': {} copies in memory",
                snap.module,
                hits.len()
            ));
            self.module_name = scan::pointer_scan(mem, &hits, params);
        } else {
            self.module_name.retain(|c| {
                c.resolve(mem)
                    .and_then(|a| read_cstr(mem, a, 32))
                    .is_some_and(|s| s.eq_ignore_ascii_case(&snap.module))
            });
        }
        rep.lines.push(format!(
            "module_name: {} candidate path(s)",
            self.module_name.len()
        ));

        // --- position ----------------------------------------------------
        let pos_targets: Vec<usize>;
        if first {
            let hits = scan::find_f32x3(mem, snap.position, POS_EPS, 5000);
            rep.lines.push(format!(
                "position {:?}: {} copies in memory",
                snap.position,
                hits.len()
            ));
            self.position = scan::pointer_scan(mem, &hits, params);
            pos_targets = self
                .position
                .iter()
                .filter_map(|c| c.resolve(mem))
                .collect();
        } else {
            self.position.retain(|c| {
                c.resolve(mem)
                    .is_some_and(|a| position_matches(mem, a, snap.position))
            });
            pos_targets = self
                .position
                .iter()
                .filter_map(|c| c.resolve(mem))
                .collect();
        }
        rep.lines.push(format!(
            "player_position: {} candidate path(s)",
            self.position.len()
        ));

        // --- fields near the position ------------------------------------
        let mut targets = pos_targets;
        targets.sort_unstable();
        targets.dedup();
        for field in NearField::ALL {
            let Some(want) = field.expected(snap) else {
                if first {
                    rep.warnings
                        .push(format!("{} not in this save; skipped", field.key()));
                }
                continue;
            };
            if first {
                for &ty in field.scalars() {
                    for delta in scan::find_near(mem, &targets, NEAR_RADIUS, ty, want, field.eps())
                    {
                        // First matching type wins (see NearField::scalars).
                        if !self
                            .near
                            .iter()
                            .any(|n| n.field == field && n.delta == delta)
                        {
                            self.near.push(NearCandidate {
                                field,
                                delta,
                                scalar: ty,
                            });
                        }
                    }
                }
            } else {
                self.near.retain(|n| {
                    n.field != field
                        || targets.iter().any(|&p| {
                            n.scalar
                                .read(mem, (p as i64 + n.delta as i64) as usize)
                                .is_some_and(|v| (v - want).abs() <= field.eps())
                        })
                });
            }
            let count = self.near.iter().filter(|n| n.field == field).count();
            rep.lines
                .push(format!("{}: {count} candidate offset(s)", field.key()));
            self.seen_near.push((field, want));
        }

        // --- did this round actually prove anything? ----------------------
        if !first {
            if self.seen_modules.iter().all(|m| m == &snap.module) {
                rep.warnings.push("Same area as every previous round, so module_name was not really tested. Change area before the next save.".into());
            }
            if self.seen_positions.iter().all(|p| p == &snap.position) {
                rep.warnings
                    .push("Same position as before; walk somewhere else before saving.".into());
            }
        }
        self.seen_modules.push(snap.module.clone());
        self.seen_positions.push(snap.position);
        self.rounds += 1;
        rep
    }

    /// True once module and position were checked against at least two different values.
    pub fn confident(&self) -> bool {
        let distinct_modules = {
            let mut m = self.seen_modules.clone();
            m.sort();
            m.dedup();
            m.len()
        };
        let distinct_pos = {
            let mut p: Vec<String> = self
                .seen_positions
                .iter()
                .map(|p| format!("{p:?}"))
                .collect();
            p.sort();
            p.dedup();
            p.len()
        };
        distinct_modules >= 2
            && distinct_pos >= 2
            && !self.module_name.is_empty()
            && !self.position.is_empty()
    }

    /// Whether a near field's value changed across rounds (otherwise its
    /// candidates were never really tested and may be a coincidence).
    pub fn near_tested(&self, field: NearField) -> bool {
        let vals: Vec<f64> = self
            .seen_near
            .iter()
            .filter(|(f, _)| *f == field)
            .map(|(_, v)| *v)
            .collect();
        vals.windows(2).any(|w| w[0] != w[1])
    }

    pub fn best_module_name(&self) -> Option<&Chain> {
        self.module_name
            .iter()
            .min_by_key(|c| (c.cost(), c.base_rva))
    }

    pub fn best_position(&self) -> Option<&Chain> {
        self.position.iter().min_by_key(|c| (c.cost(), c.base_rva))
    }

    pub fn best_near(&self, field: NearField) -> Option<&NearCandidate> {
        self.near
            .iter()
            .filter(|n| n.field == field)
            .min_by_key(|n| (n.delta.unsigned_abs(), n.delta))
    }

    /// TOML for a `[[build]]` entry with everything found so far.
    pub fn to_offsets_toml(&self, module: &str, name: &str) -> String {
        let mut out = format!(
            "[[build]]\nname = {name:?}\nsha256 = [{:?}]\n",
            self.exe_sha256
        );
        if let Some(c) = self.best_module_name() {
            out += &format!("module_name = {:?}\n", c.to_path(module));
        }
        if let Some(pos) = self.best_position() {
            out += &format!("player_position = {:?}\n", pos.to_path(module));
            for field in NearField::ALL {
                if let Some(n) = self.best_near(field) {
                    let path = pos.shifted(n.delta).to_path(module);
                    let typed = if n.scalar == Scalar::I32 || n.scalar == Scalar::F32 {
                        path
                    } else {
                        format!("{path} as {}", n.scalar.name())
                    };
                    let note = if self.near_tested(field) {
                        ""
                    } else {
                        "  # value never changed between rounds: unverified"
                    };
                    out += &format!("{} = {typed:?}{note}\n", field.key());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::fake::FakeMemory;

    /// Builds a fake KOTOR: exe+0x100 -> app object; app+0x8 -> module object
    /// whose +0x20 points to the module name string; app+0xC -> player
    /// creature with position at +0x90 and HP (i16) at +0x1A0.
    fn fake_game(module: &str, pos: [f32; 3], hp: i16, shift: usize) -> FakeMemory {
        let mut m = FakeMemory::new(0x400000, 0x1000);
        // Each object in its own allocation, placed differently per "session".
        let (app, modobj, pc, name) = (
            0x1000_0000 + shift,
            0x2000_0000 + shift * 3,
            0x3000_0000 + shift * 5,
            0x4000_0000 + shift * 7,
        );
        for base in [app, modobj, pc, name, 0x5000_0000] {
            m.add(base, vec![0; 0x2000]);
        }
        m.write_u32(0x400100, app as u32);
        m.write_u32(app + 0x8, modobj as u32);
        m.write_u32(app + 0xC, pc as u32);
        m.write_u32(modobj + 0x20, name as u32);
        let mut s = module.as_bytes().to_vec();
        s.push(0);
        m.write(name, &s);
        let p: Vec<u8> = pos.iter().flat_map(|f| f.to_le_bytes()).collect();
        m.write(pc + 0x90, &p);
        m.write(pc + 0x1A0, &hp.to_le_bytes());
        // Decoys: stale copies with no static path.
        m.write(0x5000_0000, &s);
        m.write(0x5000_0100, &p);
        m
    }

    fn snap(module: &str, pos: [f32; 3], hp: i64) -> PlayerSnapshot {
        PlayerSnapshot {
            module: module.into(),
            position: pos,
            facing: None,
            hp: Some(hp),
            max_hp: None,
            appearance: None,
            save_name: None,
        }
    }

    #[test]
    fn two_rounds_converge_on_the_real_paths() {
        let mut st = ProbeState::new("abc");
        let params = PointerScanParams::default();

        let s1 = snap("end_m01aa", [10.0, 20.0, 0.5], 30);
        let r1 = st.round(&fake_game("end_m01aa", s1.position, 30, 0), &s1, params);
        assert!(r1.warnings.iter().any(|w| w.contains("player_max_hp")));
        assert!(!st.confident());

        // "Restart" the game: objects at new heap addresses, new area, moved, took damage.
        let s2 = snap("end_m01ab", [-4.0, 7.5, 1.0], 22);
        let r2 = st.round(
            &fake_game("end_m01ab", s2.position, 22, 0x10000),
            &s2,
            params,
        );
        assert!(r2.warnings.is_empty(), "{:?}", r2.warnings);
        assert!(st.confident());

        assert_eq!(
            st.best_module_name().unwrap().to_path("swkotor.exe"),
            "swkotor.exe+0x100, 0x8, 0x20, 0x0"
        );
        assert_eq!(
            st.best_position().unwrap().to_path("swkotor.exe"),
            "swkotor.exe+0x100, 0xC, 0x90"
        );
        let hp = st.best_near(NearField::PlayerHp).unwrap();
        assert_eq!((hp.delta, hp.scalar), (0x110, Scalar::I16));
        assert!(st.near_tested(NearField::PlayerHp));

        let toml = st.to_offsets_toml("swkotor.exe", "test");
        assert!(
            toml.contains("player_hp = \"swkotor.exe+0x100, 0xC, 0x1A0 as i16\""),
            "{toml}"
        );
    }
}
