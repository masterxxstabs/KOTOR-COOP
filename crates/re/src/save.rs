//! Reads what a KOTOR save says about the player right now.
//!
//! Save folder layout (per xoreos' KotOR loader):
//! * `savenfo.res`  GFF; `LASTMODULE` = module the save was made in
//! * `SAVEGAME.sav` ERF; contains a nested ERF named after that module (type
//!   SAV), which contains `Module` (type IFO), whose `Mod_PlayerList[0]` is
//!   the player creature with `XPosition`/`YPosition`/`ZPosition`.
//!
//! The remaining labels (`XOrientation`, `CurrentHitPoints`, `MaxHitPoints`,
//! `Appearance_Type`) are standard creature fields but are treated as
//! optional, so an unexpected layout degrades instead of failing.

use crate::erf::{Erf, TYPE_IFO, TYPE_SAV};
use crate::gff;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Gff(&'static str, gff::GffError),
    #[error("{0}: {1}")]
    Erf(&'static str, crate::erf::ErfError),
    #[error("save is missing {0}")]
    Missing(String),
    #[error("no saves found in {0}")]
    NoSaves(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub module: String,
    pub position: [f32; 3],
    /// Facing in radians from the X/Y orientation vector, if present.
    pub facing: Option<f32>,
    pub hp: Option<i64>,
    pub max_hp: Option<i64>,
    pub appearance: Option<i64>,
    pub save_name: Option<String>,
}

fn read(path: PathBuf) -> Result<Vec<u8>, SaveError> {
    std::fs::read(&path).map_err(|e| SaveError::Io(path, e))
}

pub fn read_player(save_dir: &Path) -> Result<PlayerSnapshot, SaveError> {
    let nfo = gff::parse(&read(save_dir.join("savenfo.res"))?)
        .map_err(|e| SaveError::Gff("savenfo.res", e))?;
    let module = nfo
        .root
        .string("LASTMODULE")
        .ok_or_else(|| SaveError::Missing("savenfo LASTMODULE".into()))?
        .to_ascii_lowercase();

    let sav_bytes = read(save_dir.join("SAVEGAME.sav"))?;
    let sav = Erf::parse(&sav_bytes).map_err(|e| SaveError::Erf("SAVEGAME.sav", e))?;
    let nested_bytes = sav
        .get(&module, TYPE_SAV)
        .ok_or_else(|| SaveError::Missing(format!("{module}.sav inside SAVEGAME.sav")))?;
    let nested = Erf::parse(nested_bytes).map_err(|e| SaveError::Erf("module save", e))?;
    let ifo_bytes = nested
        .get("Module", TYPE_IFO)
        .ok_or_else(|| SaveError::Missing("Module.ifo".into()))?;
    let ifo = gff::parse(ifo_bytes).map_err(|e| SaveError::Gff("Module.ifo", e))?;
    let pc = ifo
        .root
        .list("Mod_PlayerList")
        .and_then(|l| l.first())
        .ok_or_else(|| SaveError::Missing("Mod_PlayerList".into()))?;

    let pos = |l: &str| pc.float(l).ok_or_else(|| SaveError::Missing(l.into()));
    let facing = match (pc.float("XOrientation"), pc.float("YOrientation")) {
        (Some(x), Some(y)) if x != 0.0 || y != 0.0 => Some(y.atan2(x)),
        _ => None,
    };
    Ok(PlayerSnapshot {
        module,
        position: [pos("XPosition")?, pos("YPosition")?, pos("ZPosition")?],
        facing,
        hp: pc.int("CurrentHitPoints"),
        max_hp: pc.int("MaxHitPoints"),
        appearance: pc.int("Appearance_Type"),
        save_name: nfo.root.string("SAVEGAMENAME").map(str::to_owned),
    })
}

/// The most recently written save folder under `<game>/saves`.
pub fn latest_save(game_dir: &Path) -> Result<PathBuf, SaveError> {
    let saves = game_dir.join("saves");
    let rd = std::fs::read_dir(&saves).map_err(|e| SaveError::Io(saves.clone(), e))?;
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("SAVEGAME.sav").is_file())
        .max_by_key(|p| {
            std::fs::metadata(p.join("SAVEGAME.sav"))
                .and_then(|m| m.modified())
                .ok()
        })
        .ok_or(SaveError::NoSaves(saves))
}

/// Names of global variables listed in a save's GLOBALVARS.res, grouped by
/// list label (e.g. a `Cat...` list of structs with a `Name` field). The
/// exact labels aren't confirmed, so this reports whatever lists it finds.
pub fn global_names(save_dir: &Path) -> Result<Vec<(String, Vec<String>)>, SaveError> {
    let g = gff::parse(&read(save_dir.join("GLOBALVARS.res"))?)
        .map_err(|e| SaveError::Gff("GLOBALVARS.res", e))?;
    Ok(g.root
        .fields
        .iter()
        .filter_map(|(label, v)| match v {
            gff::Value::List(items) => {
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|s| s.string("Name").map(str::to_owned))
                    .collect();
                (!names.is_empty()).then(|| (label.clone(), names))
            }
            _ => None,
        })
        .collect())
}

/// Builds a minimal save folder. Used by tests here and in the launcher.
pub fn write_fixture(dir: &Path, snap: &PlayerSnapshot) -> std::io::Result<()> {
    use crate::erf;
    use gff::{Struct, Value};
    std::fs::create_dir_all(dir)?;
    let mut nfo = Struct::default();
    nfo.fields
        .insert("LASTMODULE".into(), Value::ResRef(snap.module.clone()));
    nfo.fields
        .insert("SAVEGAMENAME".into(), Value::String("fixture".into()));
    std::fs::write(dir.join("savenfo.res"), gff::write("NFO", &nfo))?;

    let mut pc = Struct::default();
    for (l, v) in ["XPosition", "YPosition", "ZPosition"]
        .iter()
        .zip(snap.position)
    {
        pc.fields.insert((*l).into(), Value::Float(v));
    }
    if let Some(f) = snap.facing {
        pc.fields
            .insert("XOrientation".into(), Value::Float(f.cos()));
        pc.fields
            .insert("YOrientation".into(), Value::Float(f.sin()));
    }
    if let Some(hp) = snap.hp {
        pc.fields
            .insert("CurrentHitPoints".into(), Value::Short(hp as i16));
    }
    if let Some(hp) = snap.max_hp {
        pc.fields
            .insert("MaxHitPoints".into(), Value::Short(hp as i16));
    }
    if let Some(a) = snap.appearance {
        pc.fields
            .insert("Appearance_Type".into(), Value::Word(a as u16));
    }
    let mut ifo = Struct::default();
    ifo.fields
        .insert("Mod_PlayerList".into(), Value::List(vec![pc]));
    let nested = erf::write("SAV", &[("Module", TYPE_IFO, gff::write("IFO", &ifo))]);
    let sav = erf::write("MOD", &[(&snap.module, TYPE_SAV, nested)]);
    std::fs::write(dir.join("SAVEGAME.sav"), sav)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_player_from_fixture_save() {
        let game = std::env::temp_dir().join(format!("kotor-save-test-{}", std::process::id()));
        let snap = PlayerSnapshot {
            module: "tar_m02aa".into(),
            position: [101.5, 42.25, 0.5],
            facing: Some(1.0),
            hp: Some(54),
            max_hp: Some(60),
            appearance: Some(91),
            save_name: Some("fixture".into()),
        };
        write_fixture(&game.join("saves/000001 - Game1"), &snap).unwrap();
        let dir = latest_save(&game).unwrap();
        let got = read_player(&dir).unwrap();
        assert_eq!(got.module, snap.module);
        assert_eq!(got.position, snap.position);
        assert!((got.facing.unwrap() - 1.0).abs() < 1e-5);
        assert_eq!(
            (got.hp, got.max_hp, got.appearance),
            (Some(54), Some(60), Some(91))
        );
        std::fs::remove_dir_all(game).unwrap();
    }
}
