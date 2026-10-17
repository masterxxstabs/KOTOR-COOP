//! KOTOR Co-op dedicated server.
//!
//! The server is the single source of truth for the shared campaign: plot
//! globals, per-module changes (dead NPCs, looted containers, opened doors),
//! the party's current module and every player's saved character. It runs
//! headless, keeps ticking and accepting connections when nobody is online,
//! autosaves to disk and (optionally) advertises itself on the master list.

pub mod config;
pub mod console;
pub mod hub;
pub mod master;
pub mod net;
pub mod world;

use config::Config;
use hub::Hub;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub type SharedHub = Arc<Mutex<Hub>>;

/// A running server. Dropping it does not stop it; call [`Server::shutdown`].
pub struct Server {
    pub hub: SharedHub,
    pub local_addr: SocketAddr,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    data_dir: PathBuf,
}

impl Server {
    pub async fn start(config: Config) -> anyhow::Result<Server> {
        let data_dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&data_dir)?;
        let data = world::PersistentWorld::load(&data_dir)?;

        let listener = TcpListener::bind((config.bind.as_str(), config.port)).await?;
        let local_addr = listener.local_addr()?;
        tracing::info!("'{}' listening on {local_addr}", config.name);

        let tick = Duration::from_secs_f64(1.0 / config.tick_hz.max(1) as f64);
        let autosave = Duration::from_secs(config.autosave_secs.max(5));
        let master_cfg = config.master.clone();
        let hub: SharedHub = Arc::new(Mutex::new(Hub::new(config, data)));
        let (stop, stop_rx) = watch::channel(false);

        let mut tasks = vec![
            tokio::spawn(net::accept_loop(listener, hub.clone(), stop_rx.clone())),
            tokio::spawn(tick_loop(hub.clone(), tick, stop_rx.clone())),
            tokio::spawn(autosave_loop(
                hub.clone(),
                data_dir.clone(),
                autosave,
                stop_rx.clone(),
            )),
        ];
        if master_cfg.enabled {
            tasks.push(tokio::spawn(master::heartbeat_loop(
                hub.clone(),
                master_cfg,
                local_addr.port(),
                stop_rx,
            )));
        }
        Ok(Server {
            hub,
            local_addr,
            stop,
            tasks,
            data_dir,
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        save_hub(&self.hub, &self.data_dir)
    }

    /// Tells players, stops all tasks and writes a final save.
    pub async fn shutdown(self, message: &str) -> anyhow::Result<()> {
        self.hub.lock().unwrap().shutdown(message);
        let _ = self.stop.send(true);
        // Give writer tasks a moment to flush the shutdown notice.
        tokio::time::sleep(Duration::from_millis(200)).await;
        for t in &self.tasks {
            t.abort();
        }
        self.save()?;
        tracing::info!("world saved, server stopped");
        Ok(())
    }
}

fn save_hub(hub: &SharedHub, data_dir: &std::path::Path) -> anyhow::Result<()> {
    // Serialize under the lock, write outside it.
    let json = {
        let mut h = hub.lock().unwrap();
        h.dirty = false;
        serde_json::to_vec_pretty(&h.data)?
    };
    world::write_world_file(data_dir, &json)
}

async fn tick_loop(hub: SharedHub, every: Duration, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.changed() => return,
        }
        let now = tokio::time::Instant::now();
        hub.lock().unwrap().tick((now - last).as_secs_f64());
        last = now;
    }
}

async fn autosave_loop(
    hub: SharedHub,
    data_dir: PathBuf,
    every: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(every);
    interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.changed() => return,
        }
        // world_time keeps moving while empty, so save at least every interval
        // even without gameplay changes; the file is small.
        let hub2 = hub.clone();
        let dir = data_dir.clone();
        match tokio::task::spawn_blocking(move || save_hub(&hub2, &dir)).await {
            Ok(Ok(())) => tracing::debug!("autosaved"),
            Ok(Err(e)) => tracing::error!("autosave failed: {e:#}"),
            Err(e) => tracing::error!("autosave task panicked: {e}"),
        }
    }
}
