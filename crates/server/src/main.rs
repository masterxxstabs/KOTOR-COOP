use clap::Parser;
use kotor_server::config::Config;
use kotor_server::console::{run_command, ConsoleResult};
use kotor_server::Server;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(version, about = "KOTOR Co-op persistent dedicated server")]
struct Args {
    /// Config file; created with defaults if missing.
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// Override the listen port.
    #[arg(short, long)]
    port: Option<u16>,
    /// Override the data directory.
    #[arg(long)]
    data_dir: Option<String>,
    /// Don't read admin commands from stdin (for services/containers).
    #[arg(long)]
    no_console: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let mut config = Config::load_or_create(&args.config)?;
    if let Some(p) = args.port {
        config.port = p;
    }
    if let Some(d) = args.data_dir {
        config.data_dir = d;
    }

    let server = Server::start(config).await?;
    tracing::info!(
        "server is up; it keeps running with no players online. Type `help` for commands."
    );

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<String>(16);
    if !args.no_console {
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            // On EOF (detached/no tty) just stop reading; the server keeps going.
            while let Ok(Some(line)) = lines.next_line().await {
                if cmd_tx.send(line).await.is_err() {
                    break;
                }
            }
        });
    }

    loop {
        tokio::select! {
            _ = shutdown_signal() => break,
            Some(line) = cmd_rx.recv() => match run_command(&server.hub, &line) {
                ConsoleResult::Output(s) if !s.is_empty() => println!("{s}"),
                ConsoleResult::Output(_) => {}
                ConsoleResult::Save => match server.save() {
                    Ok(()) => println!("saved"),
                    Err(e) => println!("save failed: {e:#}"),
                },
                ConsoleResult::Quit => break,
            },
        }
    }
    let result = server
        .shutdown("The server is shutting down. Your progress has been saved.")
        .await;
    // Exit explicitly: the runtime would otherwise wait forever on the
    // blocking thread that reads stdin.
    if let Err(e) = result {
        eprintln!("shutdown error: {e:#}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
