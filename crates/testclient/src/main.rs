//! Headless bot: joins a server, walks in circles and prints what it sees.
//! Handy for testing a dedicated server or load without launching KOTOR.
//!
//! ```text
//! kotor-testclient 127.0.0.1:27700 --name Bot1 --bots 3
//! ```

use clap::Parser;
use kotor_protocol::client::connect;
use kotor_protocol::*;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Server address (host[:port]).
    server: String,
    #[arg(long, default_value = "Bot")]
    name: String,
    #[arg(long)]
    password: Option<String>,
    /// Number of bots to run in parallel.
    #[arg(long, default_value_t = 1)]
    bots: u32,
    /// Seconds to stay connected (0 = forever).
    #[arg(long, default_value_t = 0)]
    seconds: u64,
    #[arg(long, default_value = "end_m01aa")]
    module: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let handles: Vec<_> = (0..args.bots)
        .map(|i| {
            let name = if args.bots == 1 {
                args.name.clone()
            } else {
                format!("{}{}", args.name, i + 1)
            };
            let (server, password, module) = (
                args.server.clone(),
                args.password.clone(),
                args.module.clone(),
            );
            let secs = args.seconds;
            std::thread::spawn(move || {
                if let Err(e) = run_bot(&server, &name, password, &module, secs) {
                    eprintln!("[{name}] {e:#}");
                }
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn run_bot(
    server: &str,
    name: &str,
    password: Option<String>,
    module: &str,
    secs: u64,
) -> anyhow::Result<()> {
    let token = format!("bot-{}-{:016x}", name.to_lowercase(), rand::random::<u64>());
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        mod_version: MOD_VERSION.into(),
        player_name: name.into(),
        player_token: token,
        password,
        game_build: Some("testclient".into()),
    };
    let (welcome, tx, mut rx) = connect(server, hello)?;
    println!(
        "[{name}] joined '{}' as #{} - {} other player(s), module {:?}",
        welcome.server_name,
        welcome.your_id,
        welcome.players.len().saturating_sub(1),
        welcome.world.current_module
    );

    let tx2 = tx.clone();
    let name2 = name.to_owned();
    std::thread::spawn(move || loop {
        match rx.recv() {
            Ok(ServerMessage::Chat { name, text, .. }) => println!("[{name2}] <{name}> {text}"),
            Ok(ServerMessage::Transition { module, .. }) => {
                println!("[{name2}] party travelling to {module}")
            }
            Ok(ServerMessage::Shutdown { message }) => {
                println!("[{name2}] server: {message}");
                return;
            }
            Ok(_) => {}
            Err(e) => {
                println!("[{name2}] disconnected: {e}");
                return;
            }
        }
    });

    tx.send(&ClientMessage::EnteredModule {
        module: module.into(),
    })?;
    let start = Instant::now();
    let mut t = 0f32;
    while secs == 0 || start.elapsed() < Duration::from_secs(secs) {
        t += 0.05;
        let state = PlayerState {
            module: module.into(),
            position: Vec3::new(10.0 + 3.0 * t.cos(), 10.0 + 3.0 * t.sin(), 0.0),
            facing: t + std::f32::consts::FRAC_PI_2,
            animation: 0,
            hp: 30,
            max_hp: 30,
            appearance: 1,
            in_combat: false,
            in_dialog: false,
        };
        tx2.send(&ClientMessage::PlayerState(state))?;
        if ((t * 20.0) as u32).is_multiple_of(200) {
            tx2.send(&ClientMessage::Ping {
                nonce: start.elapsed().as_millis() as u64,
            })?;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tx.close();
    Ok(())
}
