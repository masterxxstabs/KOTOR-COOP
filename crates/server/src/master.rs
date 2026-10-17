use crate::config::MasterConfig;
use crate::SharedHub;
use kotor_protocol::master::Heartbeat;
use std::time::Duration;
use tokio::sync::watch;

/// Announces the server to the master list periodically, players or not, so
/// an empty persistent world stays visible in the server browser.
pub async fn heartbeat_loop(
    hub: SharedHub,
    cfg: MasterConfig,
    port: u16,
    mut stop: watch::Receiver<bool>,
) {
    let url = format!("{}/api/heartbeat", cfg.url.trim_end_matches('/'));
    let mut interval = tokio::time::interval(Duration::from_secs(cfg.heartbeat_secs.max(5)));
    let mut failing = false;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.changed() => return,
        }
        let beat = {
            let h = hub.lock().unwrap();
            Heartbeat {
                name: h.config.name.clone(),
                description: h.config.description.clone(),
                host: cfg.public_address.clone(),
                port,
                players: h.player_count() as u32,
                max_players: h.config.max_players as u32,
                module: h.current_module().map(str::to_owned),
                password_protected: !h.config.password.is_empty(),
                protocol_version: kotor_protocol::PROTOCOL_VERSION,
                mod_version: kotor_protocol::MOD_VERSION.into(),
            }
        };
        let target = url.clone();
        let res = tokio::task::spawn_blocking(move || {
            ureq::post(&target)
                .send_json(&beat)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        match res {
            Ok(()) if failing => {
                tracing::info!("master server reachable again");
                failing = false;
            }
            Ok(()) => {}
            Err(e) if !failing => {
                tracing::warn!("master heartbeat to {url} failed: {e} (will keep retrying)");
                failing = true;
            }
            Err(_) => {}
        }
    }
}
