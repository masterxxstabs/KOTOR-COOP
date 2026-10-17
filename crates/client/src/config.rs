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
    /// Loads the shipped `offsets.toml` plus `offsets.local.toml` if present.
    /// The local file is written by `kotor-launcher probe` and never touched by
    /// the updater; its builds take precedence over the shipped ones.
    pub fn load(dir: &Path) -> Result<OffsetsFile, String> {
        let read = |name: &str| -> Result<Option<OffsetsFile>, String> {
            let path = dir.join(name);
            match std::fs::read_to_string(&path) {
                Ok(text) => Self::parse(&text)
                    .map(Some)
                    .map_err(|e| format!("{}: {e}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("{}: {e}", path.display())),
            }
        };
        let shipped = read("offsets.toml")?.unwrap_or_default();
        Ok(match read("offsets.local.toml")? {
            Some(mut local) => {
                local.build.extend(shipped.build);
                local.tracked_globals.extend(shipped.tracked_globals);
                local
            }
            None => shipped,
        })
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

/// Width/encoding of the value a pointer path points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    I16,
    U16,
    I32,
    U8,
}

/// Parsed form of a pointer path string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerPath {
    /// Module whose base address is added to `base` (None = absolute address).
    pub module: Option<String>,
    pub base: u32,
    /// Each entry: dereference the current address, then add the offset.
    pub offsets: Vec<u32>,
    /// From an optional `as i16` suffix; None = the field's default type.
    pub ty: Option<ValueType>,
}

impl PointerPath {
    /// Parses `"module.exe+0x1234, 0x10, 0x8"`, `"0x7A39FC, 0x4"` or either
    /// with a type suffix such as `"... , 0x1A0 as i16"`.
    pub fn parse(s: &str) -> Option<PointerPath> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (s, ty) = match s.rsplit_once(" as ") {
            Some((path, ty)) => (
                path.trim(),
                Some(match ty.trim() {
                    "i16" => ValueType::I16,
                    "u16" => ValueType::U16,
                    "i32" => ValueType::I32,
                    "u8" => ValueType::U8,
                    _ => return None,
                }),
            ),
            None => (s, None),
        };
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
            ty,
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
                offsets: vec![4, 0x90],
                ty: None,
            })
        );
        assert_eq!(
            PointerPath::parse("0x7A39FC"),
            Some(PointerPath {
                module: None,
                base: 0x7A39FC,
                offsets: vec![],
                ty: None,
            })
        );
        let typed = PointerPath::parse("swkotor.exe+0x100, 0xC, 0x1A0 as i16").unwrap();
        assert_eq!(typed.ty, Some(ValueType::I16));
        assert_eq!(typed.offsets, vec![0xC, 0x1A0]);
        assert_eq!(PointerPath::parse("0x10 as f64"), None);
        assert_eq!(PointerPath::parse(""), None);
        assert_eq!(PointerPath::parse("swkotor.exe+zz"), None);
    }

    #[test]
    fn local_offsets_take_precedence() {
        let dir = std::env::temp_dir().join(format!("kotor-offsets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("offsets.toml"),
            "[[build]]\nname = \"shipped\"\nsha256 = [\"AA\"]\n",
        )
        .unwrap();
        assert_eq!(
            OffsetsFile::load(&dir)
                .unwrap()
                .for_build("aa")
                .unwrap()
                .name,
            "shipped"
        );
        std::fs::write(
            dir.join("offsets.local.toml"),
            "[[build]]\nname = \"probed\"\nsha256 = [\"aa\"]\n",
        )
        .unwrap();
        assert_eq!(
            OffsetsFile::load(&dir)
                .unwrap()
                .for_build("AA")
                .unwrap()
                .name,
            "probed"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shipped_offsets_file_parses() {
        let text = include_str!("../../../dist/kotor-coop/offsets.toml");
        let f = OffsetsFile::parse(text).unwrap();
        assert!(!f.build.is_empty());
        assert!(f.for_build("not-a-hash").is_none());
    }
}
