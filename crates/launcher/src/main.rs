//! KOTOR Co-op launcher. Run without arguments for the GUI; subcommands give
//! the same features on the command line (and the release tooling).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod game;
mod gui;
mod probe;
mod servers;
mod settings;
mod update;

use clap::{Parser, Subcommand};
use settings::Settings;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "KOTOR Co-op launcher")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Find the game folder and print its swkotor.exe hash.
    Detect,
    /// Download and install the latest mod files and patches.
    Update {
        #[arg(long)]
        game: Option<PathBuf>,
    },
    /// Remove the mod and restore original game files.
    Uninstall {
        #[arg(long)]
        game: Option<PathBuf>,
    },
    /// List servers from the master server.
    Servers,
    /// Join a server: write the session and start the game.
    Play {
        /// host[:port]
        server: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        game: Option<PathBuf>,
    },
    /// Map your game's memory layout (run while KOTOR is running; see docs/REVERSE_ENGINEERING.md).
    Probe {
        #[command(subcommand)]
        action: Option<ProbeCmd>,
        #[arg(long)]
        game: Option<PathBuf>,
        /// Use this save folder instead of the newest one.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Print what a save folder says about the player (newest save by default).
    DumpSave {
        dir: Option<PathBuf>,
        #[arg(long)]
        game: Option<PathBuf>,
    },
    /// Print any GFF file (.res, .ifo, .utc, ...) as text.
    DumpGff { file: PathBuf },
    /// (Release tooling) Build an update folder from a release tree.
    MakeRelease {
        /// Folder laid out like the game folder (e.g. dist/).
        #[arg(long)]
        from: PathBuf,
        /// Output folder to upload (e.g. the master's updates dir).
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        version: String,
        /// File containing the hex ed25519 signing key.
        #[arg(long)]
        key: Option<PathBuf>,
    },
    /// (Release tooling) Generate an update signing keypair.
    Keygen {
        #[arg(long, default_value = "update-signing.key")]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProbeCmd {
    /// Show candidates found so far and the offsets they would produce.
    Status,
    /// Forget all probe data and start over.
    Reset,
}

fn game_dir(settings: &Settings, explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(g) = explicit.or_else(|| settings.game_dir.clone()) {
        anyhow::ensure!(
            game::is_game_dir(&g),
            "{} does not contain swkotor.exe and chitin.key",
            g.display()
        );
        return Ok(g);
    }
    game::detect()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("KOTOR not found; pass --game <folder>"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut settings = Settings::load();
    let Some(cmd) = cli.cmd else {
        return gui::run(settings);
    };
    match cmd {
        Cmd::Detect => {
            let found = game::detect();
            if found.is_empty() {
                println!(
                    "KOTOR not found automatically. Use --game <folder> or set it in the launcher."
                );
            }
            for g in found {
                let hash = update::sha256_file(&g.join(game::GAME_EXE))?;
                println!("{}\n  swkotor.exe sha256: {hash}", g.display());
            }
        }
        Cmd::Update { game } => {
            let g = game_dir(&settings, game)?;
            if settings.update_public_key.is_empty() {
                eprintln!("warning: no update_public_key configured; downloads are hash-checked but not signature-verified");
            }
            let src = update::source_for(&settings.update_url);
            let check = update::check(&*src, &g, Some(&settings.update_public_key))?;
            if check.needed.is_empty() && check.stale.is_empty() {
                println!("up to date ({})", check.manifest.version);
            } else {
                update::apply(&*src, &g, &check, &mut |l| println!("{l}"))?;
            }
        }
        Cmd::Uninstall { game } => {
            let g = game_dir(&settings, game)?;
            update::uninstall(&g, &mut |l| println!("{l}"))?;
        }
        Cmd::Servers => {
            for s in servers::fetch(&settings.master_url)? {
                println!(
                    "{:<24} {:>2}/{:<2} {:<12} {}{}",
                    s.address,
                    s.players,
                    s.max_players,
                    s.module.as_deref().unwrap_or("-"),
                    s.name,
                    if s.password_protected {
                        " [password]"
                    } else {
                        ""
                    }
                );
            }
        }
        Cmd::Play {
            server,
            name,
            password,
            game,
        } => {
            let g = game_dir(&settings, game)?;
            if let Some(n) = name {
                settings.player_name = n;
            }
            anyhow::ensure!(
                !settings.player_name.trim().is_empty(),
                "set a player name with --name"
            );
            settings.last_server = server.clone();
            settings.save()?;
            let installed = update::Installed::load(&g);
            game::write_session(
                &g,
                &game::Session {
                    server: &server,
                    player_name: settings.player_name.trim(),
                    player_token: &settings.player_token,
                    password: password.as_deref(),
                    mod_version: installed.version.as_deref(),
                },
            )?;
            game::launch(&g)?;
            println!("game started; joining {server}");
        }
        Cmd::Probe { action, game, save } => {
            let g = game_dir(&settings, game)?;
            match action {
                None => probe::run(&g, save)?,
                Some(ProbeCmd::Status) => probe::status(&g)?,
                Some(ProbeCmd::Reset) => probe::reset(&g)?,
            }
        }
        Cmd::DumpSave { dir, game } => {
            let g = if dir.is_none() {
                Some(game_dir(&settings, game)?)
            } else {
                None
            };
            probe::dump_save(g.as_deref(), dir)?;
        }
        Cmd::DumpGff { file } => probe::dump_gff(&file)?,
        Cmd::MakeRelease {
            from,
            out,
            version,
            key,
        } => {
            let key = key.map(std::fs::read_to_string).transpose()?;
            let m = update::build_release(&from, &out, &version, key.as_deref())?;
            println!(
                "release {} with {} files written to {}",
                m.version,
                m.files.len(),
                out.display()
            );
            if key.is_none() {
                println!("warning: release is unsigned");
            }
        }
        Cmd::Keygen { out } => {
            anyhow::ensure!(
                !out.exists(),
                "{} already exists; refusing to overwrite a signing key",
                out.display()
            );
            let (secret, public) = update::generate_keypair();
            std::fs::write(&out, secret)?;
            println!(
                "secret key written to {} (keep it private, never commit it)",
                out.display()
            );
            println!("public key (put in launcher settings update_public_key):\n{public}");
        }
    }
    Ok(())
}
