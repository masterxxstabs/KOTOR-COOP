//! Persistent state: the shared world plus one account per player name.
//! Lives on disk as JSON so admins can inspect or hand-edit it.

use anyhow::Context;
use base64::Engine;
use kotor_protocol::WorldSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WORLD_FILE: &str = "world.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PersistentWorld {
    pub world: WorldSnapshot,
    pub accounts: BTreeMap<String, Account>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// SHA-256 of the player's launcher token; the token itself is never stored.
    pub token_hash: String,
    /// Base64 of the client-provided character blob.
    #[serde(default)]
    pub character: Option<String>,
    pub first_seen: u64,
    pub last_seen: u64,
}

impl Account {
    pub fn character_bytes(&self) -> Option<Vec<u8>> {
        let b64 = self.character.as_ref()?;
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    }

    pub fn set_character(&mut self, data: &[u8]) {
        self.character = Some(base64::engine::general_purpose::STANDARD.encode(data));
    }
}

/// Writes `json` as the world file without ever leaving a half-written file behind.
pub fn write_world_file(data_dir: &Path, json: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = PersistentWorld::path(data_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    if path.exists() {
        let _ = std::fs::copy(&path, path.with_extension("json.bak"));
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Outcome of checking a name/token pair against the stored accounts.
#[derive(Debug, PartialEq, Eq)]
pub enum AccountCheck {
    New,
    Returning,
    WrongToken,
}

impl PersistentWorld {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(WORLD_FILE)
    }

    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(data_dir);
        if !path.exists() {
            tracing::info!("no world at {}, starting a fresh campaign", path.display());
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Writes to a temp file and renames, so a crash mid-save never corrupts the world.
    /// The previous save is kept as `world.json.bak`.
    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        write_world_file(data_dir, &serde_json::to_vec_pretty(self)?)
    }

    pub fn check_account(&self, name: &str, token: &str) -> AccountCheck {
        match self.accounts.get(&name.to_lowercase()) {
            None => AccountCheck::New,
            Some(a) if a.token_hash == hash_token(token) => AccountCheck::Returning,
            Some(_) => AccountCheck::WrongToken,
        }
    }

    /// Creates or refreshes the account and returns it.
    pub fn touch_account(&mut self, name: &str, token: &str) -> &mut Account {
        let now = unix_now();
        let acct = self
            .accounts
            .entry(name.to_lowercase())
            .or_insert_with(|| Account {
                token_hash: hash_token(token),
                character: None,
                first_seen: now,
                last_seen: now,
            });
        acct.last_seen = now;
        acct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_binding_is_case_insensitive_and_token_checked() {
        let mut w = PersistentWorld::default();
        assert_eq!(w.check_account("Revan", "t1"), AccountCheck::New);
        w.touch_account("Revan", "t1");
        assert_eq!(w.check_account("revan", "t1"), AccountCheck::Returning);
        assert_eq!(w.check_account("REVAN", "other"), AccountCheck::WrongToken);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kotor-world-test-{}", std::process::id()));
        let mut w = PersistentWorld::default();
        w.world.current_module = Some("tar_m02aa".into());
        w.touch_account("Bastila", "tok").set_character(b"\x01\x02");
        w.save(&dir).unwrap();
        w.save(&dir).unwrap(); // second save exercises the .bak path
        let back = PersistentWorld::load(&dir).unwrap();
        assert_eq!(back.world.current_module.as_deref(), Some("tar_m02aa"));
        assert_eq!(
            back.accounts["bastila"].character_bytes().unwrap(),
            vec![1, 2]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
