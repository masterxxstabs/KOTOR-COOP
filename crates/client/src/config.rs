//! Files the launcher places next to the DLL in `<game>/kotor-coop/`:
//!
//! * `session.toml` - which server to join and as whom (rewritten on every launch)
//! * `offsets.toml` - per-game-build memory layout, shipped through the updater

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub server: String,
    pub player_name: String,
    pub player_token: String,
    #[serde(default)]
    pub password: Option<String>,
    /// Mod package version installed by the launcher.
    #[serde(default)]
    pub mod_version: Option<String>,
}

impl Session {
    pub fn load(dir: &Path) -> Result<Session, String> {
        let path = dir.join("session.toml");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Memory layout for one build of swkotor.exe, matched by SHA-256.
///
/// Pointer paths use Cheat Engine notation so they can be found and shared
/// without touching code: `"swkotor.exe+0x3A39FC, 0x4, 0x90"` means read the
/// pointer at module base + 0x3A39FC, add 0x4, read a pointer, then add 0x90
/// to get the value's address. Empty string = not known yet; the feature is
/// switched off for that build.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BuildOffsets {
    pub name: String,
    pub sha256: Vec<String>,
    /// f32 x3, world position of the controlled character.
    pub player_position: String,
    /// f32, facing in radians.
    pub player_facing: String,
    pub player_hp: String,
    pub player_max_hp: String,
    /// u16 row in appearance.2da.
    pub player_appearance: String,
    /// NUL-terminated ASCII resref of the loaded module, e.g. `end_m01aa`.
    pub module_name: String,
    /// u8/bool, non-zero while a loading screen is up.
    pub loading_flag: String,
    pub functions: Functions,
}

/// Engine functions (absolute addresses, `__thiscall`). Each needs its `this`
/// object as a pointer path too. See docs/REVERSE_ENGINEERING.md for the
/// expected signatures. Zero/empty = unknown.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Functions {
    /// Pointer path to the global variable table (CSWSGlobalVariableTable*).
    pub globals_table: String,
    /// `void SetValueBoolean(this, CExoString* name, int value)`
    pub set_global_boolean: String,
    /// `void SetValueNumber(this, CExoString* name, int value)`
    pub set_global_number: String,
    /// `int GetValueBoolean(this, CExoString* name, int* out)`
    pub get_global_boolean: String,
    /// `int GetValueNumber(this, CExoString* name, int* out)`
    pub get_global_number: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct OffsetsFile {
    #[serde(default)]
    pub build: Vec<BuildOffsets>,
    /// Plot globals to keep in sync by polling (in addition to ones the server sends).
    #[serde(default)]
    pub tracked_globals: Vec<TrackedGlobal>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackedGlobal {
    pub name: String,
    /// "boolean" or "number"
    pub kind: String,
}

impl OffsetsFile {
    pub fn load(dir: &Path) -> Result<OffsetsFile, String> {
        let path = dir.join("offsets.toml");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<OffsetsFile, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn for_build(&self, sha256: &str) -> Option<&BuildOffsets> {
        self.build
            .iter()
            .find(|b| b.sha256.iter().any(|h| h.eq_ignore_ascii_case(sha256)))
    }
}

/// Parsed form of a pointer path string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerPath {
    /// Module whose base address is added to `base` (None = absolute address).
    pub module: Option<String>,
    pub base: u32,
    /// Each entry: dereference the current address, then add the offset.
    pub offsets: Vec<u32>,
}

impl PointerPath {
    /// Parses `"module.exe+0x1234, 0x10, 0x8"` or `"0x7A39FC, 0x4"`.
    pub fn parse(s: &str) -> Option<PointerPath> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut parts = s.split(',').map(str::trim);
        let head = parts.next()?;
        let (module, base) = match head.split_once('+') {
            Some((m, off)) => (Some(m.trim().to_owned()), parse_num(off)?),
            None => (None, parse_num(head)?),
        };
        let offsets = parts.map(parse_num).collect::<Option<Vec<_>>>()?;
        Some(PointerPath {
            module,
            base,
            offsets,
        })
    }
}

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pointer_paths() {
        assert_eq!(
            PointerPath::parse("swkotor.exe+0x3A39FC, 0x4, 0x90"),
            Some(PointerPath {
                module: Some("swkotor.exe".into()),
                base: 0x3A39FC,
                offsets: vec![4, 0x90]
            })
        );
        assert_eq!(
            PointerPath::parse("0x7A39FC"),
            Some(PointerPath {
                module: None,
                base: 0x7A39FC,
                offsets: vec![]
            })
        );
        assert_eq!(PointerPath::parse(""), None);
        assert_eq!(PointerPath::parse("swkotor.exe+zz"), None);
    }

    #[test]
    fn shipped_offsets_file_parses() {
        let text = include_str!("../../../dist/kotor-coop/offsets.toml");
        let f = OffsetsFile::parse(text).unwrap();
        assert!(!f.build.is_empty());
        assert!(f.for_build("not-a-hash").is_none());
    }
}
