use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Name shown in the launcher's server browser.
    pub name: String,
    pub description: String,
    pub motd: String,
    pub bind: String,
    pub port: u16,
    pub max_players: usize,
    /// Leave empty for a public server.
    pub password: String,
    /// Where world state and player characters are stored.
    pub data_dir: String,
    /// Seconds between automatic saves (only written when something changed).
    pub autosave_secs: u64,
    /// In-game seconds that pass per real second, even when nobody is online.
    pub world_time_scale: f64,
    /// Only the party leader may trigger module transitions.
    pub leader_only_transitions: bool,
    /// Player state broadcast rate.
    pub tick_hz: u32,
    pub master: MasterConfig,
    /// Player names that may not join.
    pub banned: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MasterConfig {
    /// List this server on the public server browser.
    pub enabled: bool,
    pub url: String,
    pub heartbeat_secs: u64,
    /// Address advertised to players. Empty = let the master use the address
    /// it sees the heartbeat coming from.
    pub public_address: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "KOTOR Co-op Server".into(),
            description: "A persistent Knights of the Old Republic co-op world".into(),
            motd: "Welcome to the Ebon Hawk. Be kind to your crew.".into(),
            bind: "0.0.0.0".into(),
            port: kotor_protocol::DEFAULT_PORT,
            max_players: 4,
            password: String::new(),
            data_dir: "server-data".into(),
            autosave_secs: 60,
            world_time_scale: 1.0,
            leader_only_transitions: true,
            tick_hz: 20,
            master: MasterConfig::default(),
            banned: Vec::new(),
        }
    }
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://127.0.0.1:27780".into(),
            heartbeat_secs: 30,
            public_address: String::new(),
        }
    }
}

impl Config {
    /// Loads the config, writing a commented default file if none exists yet.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            let body = toml::to_string_pretty(&cfg)?;
            std::fs::write(
                path,
                format!("# KOTOR Co-op dedicated server configuration\n\n{body}"),
            )
            .with_context(|| format!("writing default config to {}", path.display()))?;
            tracing::info!("wrote default config to {}", path.display());
            return Ok(cfg);
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}
