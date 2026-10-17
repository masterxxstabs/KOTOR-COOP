# Hosting a dedicated server

The server is a single binary with no game install required. It is the
authority for the shared campaign and is meant to run all the time. With no
one online it keeps accepting connections, ticks the world clock, autosaves,
and stays listed in the server browser.

## Run it

```sh
./kotor-server                       # creates server.toml with defaults, then starts
./kotor-server --config my.toml --port 27700 --data-dir /srv/kotor
```

Open **TCP 27700** in your firewall/router.

### Docker (recommended for 24/7)
```sh
docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml logs -f server
```
Data lives in the `kotor-data` volume; edit `/data/server.toml` there.

### systemd
See the header of `deploy/kotor-server.service`.

### Windows
Run `kotor-server.exe` (from CI artifacts or a cross build) in a console, or
register it as a service with a tool like NSSM.

## server.toml

| Key | Default | Meaning |
|---|---|---|
| `name`, `description`, `motd` | | Shown in the browser / on join |
| `bind`, `port` | `0.0.0.0`, `27700` | Listen address |
| `max_players` | `4` | |
| `password` | empty | Empty = public |
| `data_dir` | `server-data` | World save location |
| `autosave_secs` | `60` | Save interval (also saved on shutdown) |
| `world_time_scale` | `1.0` | In-game seconds per real second, online or not |
| `leader_only_transitions` | `true` | Only the party leader moves the party between areas |
| `tick_hz` | `20` | Position broadcast rate |
| `banned` | `[]` | Player names refused at join |
| `[master] enabled` | `false` | List on a server browser |
| `[master] url` | | Master server base URL |
| `[master] public_address` | empty | Hostname to advertise; empty = the IP the master sees |

## Saves

`data_dir/world.json` holds the campaign and player accounts, and
`world.json.bak` holds the previous save. Writes are atomic (temp file, then
rename), so a crash never leaves a half-written world. Player tokens are
stored only as SHA-256 hashes.

## Admin console

Type commands in the server terminal (disable with `--no-console`):

```
status | players | say <text> | kick <name> | ban <name>
module <area> [waypoint] | global <name> [value] | reset-module <area>
save | quit
```

SIGTERM/Ctrl-C tells players, saves, and exits.

## Running a server browser and update host

```sh
./kotor-master --port 27780 --updates-dir ./updates
```
- Servers with `[master] enabled = true` and `url = "http://your-host:27780"` appear in launchers whose *Server list URL* points there.
- Publish mod updates with `scripts/release.sh <version> <key> ./updates`. Generate the key once with `kotor-launcher keygen`, then give players the printed public key (launcher setting *Update public key*) so they only accept your signed updates.
