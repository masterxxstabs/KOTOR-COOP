# KOTOR Co-op

A multiplayer co-op mod for *Star Wars: Knights of the Old Republic* (2003),
with a **persistent dedicated server** that keeps the campaign alive when
nobody is playing, and a **launcher** that keeps your game patched, lists
servers and starts the game with the mod loaded.

> You need your own copy of KOTOR (Steam or GOG, PC). This project ships no
> game files.

## What's in the box

| Component | Path | What it does |
|---|---|---|
| Dedicated server | `crates/server` | Runs 24/7, no game install needed. Keeps the shared campaign: plot globals, per-area changes (dead NPCs, looted containers, opened doors), the party's current area, each player's character, and a world clock that keeps ticking while the server is empty. Autosaves, admin console, Docker/systemd ready. |
| Master server | `crates/master` | Public server browser list (servers announce themselves) and HTTP host for mod updates. |
| Launcher | `crates/launcher` | GUI + CLI. Finds KOTOR (Steam/GOG), installs signed mod updates and game patches (with backup/restore of original files), browses/pings servers, direct connect with password, and starts the game with the co-op DLL injected. |
| Client DLL | `crates/client` | `kotor_coop.dll`, loaded into `swkotor.exe`. Connects to the server, syncs state each frame, draws an in-game overlay (party list, chat; F8 to talk). |
| Protocol | `crates/protocol` | Versioned wire format shared by all of the above. |
| Test bot | `crates/testclient` | Headless fake players for testing servers without the game. |

## Status: read this first

The networking, the persistent server, the server browser, the updater and
the launcher work and have tests (`cargo test --workspace`), including a
real-TCP test that restarts the server and checks the world persisted.

**Hooking into the game is only partly done.** KOTOR is a closed, 32-bit,
single-player engine. The DLL already does what needs no engine knowledge:
it injects, hooks the OpenGL frame, connects, draws the overlay and handles
chat. Gameplay sync needs the memory addresses of engine data and functions
in `swkotor.exe`. Those come from reverse-engineering, and nobody has mapped
them yet. They live in [`dist/kotor-coop/offsets.toml`](dist/kotor-coop/offsets.toml),
which ships through the updater. Filling them in turns features on without a
new DLL.

| Feature | State |
|---|---|
| Launcher, updates, patches, server browser | Working |
| Dedicated persistent server, accounts, autosave, admin console | Working |
| Injection, frame hook, overlay, chat | Implemented, not yet tested in-game |
| Position sync / area detection | Code done; needs pointer paths in offsets.toml |
| Plot/quest global sync | Code done; needs function addresses in offsets.toml |
| Seeing other players (puppet creatures) | Needs engine RE (spawn/move creature) |
| Party travel together | Server + client logic done; needs engine RE (module load). Until then the overlay tells players where the party went. |
| Replaying world changes (dead NPCs, opened doors) | Server stores them; needs engine RE to apply |
| Combat/dialog sync | Protocol messages exist; engine side not started |

See [docs/REVERSE_ENGINEERING.md](docs/REVERSE_ENGINEERING.md) for exactly
what to find and how.

## Quick start

### Players
1. Run `kotor-launcher.exe`. In **Settings**, pick a name. Check the KOTOR folder (it's auto-detected), then set the server list and update URLs your community uses.
2. **Mod & Patches → Install / Update**.
3. **Servers → Join**, or connect directly to `host:27700`.

To remove everything and restore your original game files, use **Mod &
Patches → Uninstall**.

### Hosting a server
```sh
cargo build --release -p kotor-server
./target/release/kotor-server            # writes server.toml on first run
```
Open TCP port **27700**. The server keeps running and saving with zero
players online. More in [docs/HOSTING.md](docs/HOSTING.md) (Docker, systemd,
server list, admin commands).

### Developers
```sh
cargo test --workspace
# Windows binaries (KOTOR is 32-bit): needs gcc-mingw-w64-i686
rustup target add i686-pc-windows-gnu
cargo build --release --target i686-pc-windows-gnu -p kotor-client -p kotor-launcher
# Try a server with fake players
cargo run -p kotor-server &
cargo run -p kotor-testclient -- 127.0.0.1 --bots 3
```
Architecture: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Legal
This is an unofficial fan project, not affiliated with Lucasfilm, Disney,
BioWare or Aspyr. It ships no game assets.
