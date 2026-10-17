//! JSON types for the master server (server browser) HTTP API.
//!
//! * `POST /api/heartbeat` - a dedicated server announces itself ([`Heartbeat`]).
//! * `GET  /api/servers`   - the launcher fetches the list (`Vec<ServerListing>`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub name: String,
    pub description: String,
    /// Host to advertise. Empty = the master uses the heartbeat's source IP.
    #[serde(default)]
    pub host: String,
    pub port: u16,
    pub players: u32,
    pub max_players: u32,
    pub module: Option<String>,
    pub password_protected: bool,
    pub protocol_version: u32,
    pub mod_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerListing {
    /// `host:port` to connect to.
    pub address: String,
    pub name: String,
    pub description: String,
    pub players: u32,
    pub max_players: u32,
    pub module: Option<String>,
    pub password_protected: bool,
    pub protocol_version: u32,
    pub mod_version: String,
    /// Seconds since the master last heard from this server.
    pub last_seen_secs: u64,
}
