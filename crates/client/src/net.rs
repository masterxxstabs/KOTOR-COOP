//! Background network thread: connects (and reconnects) to the server and
//! hands events to the game thread through a channel. The game thread never
//! touches the socket for reading, so a slow server can't stall a frame.

use crate::config::Session;
use crate::coop::{Link, NetEvent};
use kotor_protocol::client::{connect, ClientError, Sender};
use kotor_protocol::*;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct NetLink {
    events: mpsc::Receiver<NetEvent>,
    sender: Arc<Mutex<Option<Sender>>>,
}

impl Link for NetLink {
    fn send(&mut self, msg: ClientMessage) {
        let sender = self.sender.lock().unwrap().clone();
        if let Some(s) = sender {
            // Errors surface on the reader side as a disconnect.
            let _ = s.send(&msg);
        }
    }

    fn poll(&mut self) -> Option<NetEvent> {
        self.events.try_recv().ok()
    }
}

impl NetLink {
    pub fn start(session: Session, game_build: Option<String>, log: fn(&str)) -> NetLink {
        let (tx, rx) = mpsc::channel();
        let sender = Arc::new(Mutex::new(None));
        let shared = sender.clone();
        std::thread::Builder::new()
            .name("kotor-coop-net".into())
            .spawn(move || run(session, game_build, tx, shared, log))
            .expect("spawn network thread");
        NetLink { events: rx, sender }
    }
}

fn run(
    session: Session,
    game_build: Option<String>,
    events: mpsc::Sender<NetEvent>,
    sender: Arc<Mutex<Option<Sender>>>,
    log: fn(&str),
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            mod_version: session
                .mod_version
                .clone()
                .unwrap_or_else(|| MOD_VERSION.into()),
            player_name: session.player_name.clone(),
            player_token: session.player_token.clone(),
            password: session.password.clone(),
            game_build: game_build.clone(),
        };
        log(&format!("connecting to {}", session.server));
        match connect(&session.server, hello) {
            Ok((welcome, tx, mut rx)) => {
                log(&format!(
                    "joined '{}' as #{}",
                    welcome.server_name, welcome.your_id
                ));
                backoff = Duration::from_secs(1);
                *sender.lock().unwrap() = Some(tx);
                if events.send(NetEvent::Connected(welcome)).is_err() {
                    return;
                }
                let reason = loop {
                    match rx.recv() {
                        Ok(msg) => {
                            if events.send(NetEvent::Message(msg)).is_err() {
                                return;
                            }
                        }
                        Err(e) => break e.to_string(),
                    }
                };
                *sender.lock().unwrap() = None;
                log(&format!("disconnected: {reason}"));
                let _ = events.send(NetEvent::Disconnected(reason));
            }
            Err(ClientError::Rejected { reason, message }) => {
                // Retrying won't help (wrong version/password/name): stop and tell the player.
                log(&format!("rejected ({reason:?}): {message}"));
                let _ = events.send(NetEvent::Disconnected(format!("Rejected: {message}")));
                return;
            }
            Err(e) => {
                log(&format!("connect failed: {e}"));
                let _ = events.send(NetEvent::Disconnected(e.to_string()));
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
