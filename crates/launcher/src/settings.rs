use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Launcher preferences, stored per user (not in the game folder).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub player_name: String,
    /// Secret that proves this player owns their name on servers. Generated once.
    pub player_token: String,
    pub game_dir: Option<PathBuf>,
    pub master_url: String,
    /// Base URL of the update source (contains manifest.json).
    pub update_url: String,
    /// ed25519 public key (hex) that update manifests must be signed with.
    /// Empty = updates are not verified (development only).
    pub update_public_key: String,
    pub favorites: Vec<String>,
    pub last_server: String,
}

impl Default for Settings {
    fn default() -> Self {
        let token: [u8; 24] = rand::random();
        Self {
            player_name: String::new(),
            player_token: hex::encode(token),
            game_dir: None,
            master_url: format!("http://127.0.0.1:{}", kotor_protocol::DEFAULT_MASTER_PORT),
            update_url: format!(
                "http://127.0.0.1:{}/updates",
                kotor_protocol::DEFAULT_MASTER_PORT
            ),
            update_public_key: String::new(),
            favorites: Vec::new(),
            last_server: String::new(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("kotor-coop").join("launcher.toml")
    }

    pub fn load() -> Settings {
        let path = Self::path();
        let s: Settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        // Persist immediately so the generated token is never lost.
        if !path.exists() {
            let _ = s.save();
        }
        s
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
