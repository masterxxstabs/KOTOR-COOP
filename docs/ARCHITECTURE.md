# Architecture

```
 Player PC                                        Host (VPS / Docker / home PC)
┌──────────────────────────────────────┐         ┌──────────────────────────────┐
│ kotor-launcher.exe                    │  HTTP   │ kotor-master                  │
│  • detect game  • update/patch ──────────────▶ │  /api/servers  /updates/...   │
│  • server browser ─────────────────────────────▶│  ◀── heartbeat every 30 s ──┐ │
│  • writes kotor-coop/session.toml     │         └─────────────────────────────┼┘
│  • starts swkotor.exe suspended,      │                                       │
│    injects kotor_coop.dll             │         ┌─────────────────────────────┴┐
├──────────────────────────────────────┤  TCP    │ kotor-server (24/7)           │
│ swkotor.exe + kotor_coop.dll          │ 27700   │  Hub: players, leader, world │
│  net thread ◀───────────────────────────────────▶  tick loop (20 Hz)          │
│  SwapBuffers hook → Coop::on_frame    │         │  autosave → world.json       │
│    GameBridge (offsets.toml)          │         │  admin console               │
│    overlay + F8 chat                  │         └──────────────────────────────┘
└──────────────────────────────────────┘
```

## Authority model

Nobody runs KOTOR on the server; it can't, since the engine isn't headless.
Instead:

* **The server owns persistent state**: plot globals, per-module changes,
  the party's current module, player accounts and characters, and the world
  clock. Clients get a full `WorldSnapshot` on join and deltas afterwards.
  Duplicate events are dropped (`ModuleState::apply`), so two players
  killing the same NPC can't double-count.
* **Each client simulates its own game.** Other players are puppets driven
  by `PlayerState` updates. Combat against NPCs is simulated locally by
  each client; making one client authoritative per module is the planned
  next step once puppets exist.
* **The party leader drives travel.** When the leader enters a module, the
  server records it and tells everyone else (`Transition`). If the leader
  quits, the next player becomes leader.

That's why the server can host while no one is online. The world is just
data, and the next player to join loads it.

## Protocol

`crates/protocol`: length-prefixed (`u32` LE) [postcard](https://docs.rs/postcard)
frames over one TCP connection, `TCP_NODELAY`. Frames are capped at 4 MiB.
`PROTOCOL_VERSION` is checked in the handshake, so outdated clients are told
to update instead of desyncing.

Identity: the launcher creates a random token once. A server binds a name to
the first token that uses it (stored hashed), so nobody can take over
another player's name or character.

## Client DLL

* `coop.rs` holds all the co-op logic and is engine-agnostic: it is tested
  against a mock game and mock network.
* `GameBridge` is the only engine boundary. `win/bridge.rs` implements it
  from `offsets.toml`. Reads go through `ReadProcessMemory` on our own
  process, so a wrong offset yields "no data" instead of crashing the game.
* The frame hook patches the game's import of `SwapBuffers`/`wglSwapBuffers`,
  falling back to a hot-patch. These are public Windows exports, so no RE is
  needed.
* The network thread reconnects with backoff and stops retrying on a
  rejection (wrong version, name or password).

## Updater

The manifest lists every file with its SHA-256 and size. It is signed with
ed25519 (`manifest.json.sig`). Each file is verified before it replaces
anything. The first time the mod overwrites an original game file, a copy
goes to `kotor-coop/backup/`. Files dropped from the mod are removed or
restored, and uninstalling brings back the exact original game. Paths are
sanitised so nothing can be written outside the game folder.
