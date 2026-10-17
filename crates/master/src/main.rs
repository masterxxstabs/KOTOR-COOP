//! Master server: keeps the public server list and hosts mod updates.
//!
//! * `POST /api/heartbeat`  dedicated servers announce themselves every ~30 s
//! * `GET  /api/servers`    launchers fetch the server browser list
//! * `GET  /updates/...`    static files: `manifest.json` plus the files it lists

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use kotor_protocol::master::{Heartbeat, ServerListing};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower_http::services::ServeDir;

/// A server that misses this many seconds of heartbeats disappears from the list.
const EXPIRY: Duration = Duration::from_secs(90);
const MAX_SERVERS: usize = 2000;
const MAX_SERVERS_PER_IP: usize = 8;

#[derive(Parser)]
#[command(
    version,
    about = "KOTOR Co-op master server (server list + update host)"
)]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    #[arg(short, long, default_value_t = kotor_protocol::DEFAULT_MASTER_PORT)]
    port: u16,
    /// Directory served at /updates (put manifest.json and the release files here).
    #[arg(long, default_value = "updates")]
    updates_dir: PathBuf,
}

struct Entry {
    listing: ServerListing,
    source_ip: std::net::IpAddr,
    seen: Instant,
}

#[derive(Clone, Default)]
struct AppState {
    servers: Arc<Mutex<HashMap<String, Entry>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.updates_dir)?;

    let app = router(AppState::default(), args.updates_dir.clone());
    let listener = tokio::net::TcpListener::bind((args.bind.as_str(), args.port)).await?;
    tracing::info!(
        "master listening on {} (updates from {})",
        listener.local_addr()?,
        args.updates_dir.display()
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn router(state: AppState, updates_dir: PathBuf) -> Router {
    Router::new()
        .route("/api/heartbeat", post(heartbeat))
        .route("/api/servers", get(list))
        .route("/health", get(|| async { "ok" }))
        .nest_service("/updates", ServeDir::new(updates_dir))
        .with_state(state)
}

fn clip(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

async fn heartbeat(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(hb): Json<Heartbeat>,
) -> StatusCode {
    if hb.port == 0 || hb.name.trim().is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let host = if hb.host.trim().is_empty() {
        peer.ip().to_string()
    } else {
        clip(hb.host.trim(), 253)
    };
    let address = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", hb.port) // bare IPv6
    } else {
        format!("{host}:{}", hb.port)
    };

    let mut servers = state.servers.lock().unwrap();
    servers.retain(|_, e| e.seen.elapsed() < EXPIRY);
    if !servers.contains_key(&address) {
        let from_ip = servers
            .values()
            .filter(|e| e.source_ip == peer.ip())
            .count();
        if servers.len() >= MAX_SERVERS || from_ip >= MAX_SERVERS_PER_IP {
            return StatusCode::TOO_MANY_REQUESTS;
        }
        tracing::info!("new server {address} '{}'", hb.name);
    }
    let listing = ServerListing {
        address: address.clone(),
        name: clip(&hb.name, 64),
        description: clip(&hb.description, 256),
        players: hb.players.min(hb.max_players),
        max_players: hb.max_players.min(64),
        module: hb.module.map(|m| clip(&m, 32)),
        password_protected: hb.password_protected,
        protocol_version: hb.protocol_version,
        mod_version: clip(&hb.mod_version, 32),
        last_seen_secs: 0,
    };
    servers.insert(
        address,
        Entry {
            listing,
            source_ip: peer.ip(),
            seen: Instant::now(),
        },
    );
    StatusCode::NO_CONTENT
}

async fn list(State(state): State<AppState>) -> Json<Vec<ServerListing>> {
    let mut servers = state.servers.lock().unwrap();
    servers.retain(|_, e| e.seen.elapsed() < EXPIRY);
    let mut out: Vec<ServerListing> = servers
        .values()
        .map(|e| ServerListing {
            last_seen_secs: e.seen.elapsed().as_secs(),
            ..e.listing.clone()
        })
        .collect();
    out.sort_by(|a, b| b.players.cmp(&a.players).then_with(|| a.name.cmp(&b.name)));
    Json(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_then_list() {
        let dir = std::env::temp_dir().join(format!("kotor-master-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        let app = router(AppState::default(), dir.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });

        let list = tokio::task::spawn_blocking(move || {
            let hb = Heartbeat {
                name: "Ebon Hawk".into(),
                description: "test".into(),
                host: String::new(),
                port: 27700,
                players: 1,
                max_players: 4,
                module: Some("ebo_m12aa".into()),
                password_protected: false,
                protocol_version: kotor_protocol::PROTOCOL_VERSION,
                mod_version: "0.1.0".into(),
            };
            ureq::post(&format!("{base}/api/heartbeat"))
                .send_json(&hb)
                .unwrap();
            let manifest = ureq::get(&format!("{base}/updates/manifest.json"))
                .call()
                .unwrap()
                .into_body()
                .read_to_string()
                .unwrap();
            assert_eq!(manifest, "{}");
            ureq::get(&format!("{base}/api/servers"))
                .call()
                .unwrap()
                .into_body()
                .read_json::<Vec<ServerListing>>()
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].address, "127.0.0.1:27700");
        assert_eq!(list[0].module.as_deref(), Some("ebo_m12aa"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
