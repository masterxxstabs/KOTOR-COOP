# Mapping swkotor.exe

Everything gameplay-related in the client reads its addresses from
`dist/kotor-coop/offsets.toml`. This is the work list to bring each feature
online, in priority order. **Never commit a guessed address.** A wrong
pointer path just reads nothing, but a wrong function address crashes the
game.

## 0. Identify your build

```
kotor-launcher detect
```
This prints the SHA-256 of `swkotor.exe`. Add it to a `[[build]]` entry's
`sha256` list. Steam and GOG 1.03 may or may not be byte-identical, so check
both.

## 1. Data: automatic with `kotor-launcher probe`

The launcher can find the player and area addresses by itself. It uses one
of your own saves as the answer key. The save records exactly where you're
standing, your HP and which area you're in. The probe finds those exact
values in the running game's memory, works back to stable pointer paths
(what Cheat Engine's pointer scan does), and drops every path that stops
matching when you play somewhere else.

1. Start KOTOR (from the launcher or normally) and load a game.
2. Stand still and **quicksave**.
3. Run `kotor-launcher probe`. Round 1 scans memory; expect up to a minute.
4. Go to a **different area** (or load an older save elsewhere), walk a
   bit, take some damage if you can, stand still, **quicksave**.
   Restarting the game between rounds makes the result more reliable.
5. Run `kotor-launcher probe` again. Repeat until it says **Mapped**.

The probe then writes `kotor-coop/offsets.local.toml` with
`module_name`, `player_position`, and, when found, `player_hp`,
`player_max_hp`, `player_appearance` and `player_facing`. The DLL prefers
that file over the shipped `offsets.toml`, and the updater never touches it.
Share it (it includes your exe hash) so the paths can go into the official
`offsets.toml` for everyone with the same build.

`kotor-launcher probe status` shows progress, and `probe reset` starts over.
The probe only reads the game's memory (`PROCESS_VM_READ`); it never writes.

The save format is from xoreos' KotOR loader: `savenfo.res` → `LASTMODULE`,
`SAVEGAME.sav` → nested `<module>.sav` → `Module.ifo` → `Mod_PlayerList[0]`
(`XPosition`/`YPosition`/`ZPosition`). HP, appearance and orientation are
read when present. `kotor-launcher dump-save` and `dump-gff <file>` show what
was read, which helps if a field turns out to be named differently.

Fields that never changed between rounds are written with an
"unverified" comment. HP is only proven once it differs between two saves.

### By hand (Cheat Engine)
Pointer paths use Cheat Engine syntax: `"swkotor.exe+0x123456, 0x4, 0x10"`,
optionally with a type suffix: `"... , 0x1A0 as i16"` (`i16`, `u16`, `i32`, `u8`).

| Key | Type | How to find it |
|---|---|---|
| `module_name` | char[] | Search for the text `end_m01aa` while on the Endar Spire; it should change to `end_m01ab` etc. |
| `loading_flag` | u8 | Changed-value scan across loading screens (the probe can't find this one) |
| `player_position` | f32 x3 | Walk around; unknown initial float, increased/decreased scans |
| `player_facing` | f32 | Usually near position in the creature object |
| `player_hp`, `player_max_hp` | int | Take damage and heal |
| `player_appearance` | u16 | Compare with the `appearance.2da` row of your character |

With `module_name` and `player_position` mapped, the overlay party list
shows real areas, and every client streams its position to the server.

## 2. Plot globals (Ghidra)

KOTOR's globals live in a global-variable table object. NWScript's
`SetGlobalNumber`/`GetGlobalBoolean` builtins call its methods. Find the
script command handlers via the strings `GetGlobalNumber`, `SetGlobalBoolean`
and so on, then follow them to the table methods. Expected shapes (`__thiscall`):

```
void SetValueBoolean(Table* this, CExoString* name, int value)
void SetValueNumber (Table* this, CExoString* name, int value)
int  GetValueBoolean(Table* this, CExoString* name, int* out)   // returns found
int  GetValueNumber (Table* this, CExoString* name, int* out)
struct CExoString { char* data; uint32 length; }
```
If the real signatures differ, change `win/bridge.rs` to match. Don't force
the data to fit. Then list the plot globals to sync under `tracked_globals`.
The game's `globalcat.2da` lists every global and its type.

## 3. Engine actions (Ghidra/x32dbg, needed for "real" co-op)

Each one becomes a `GameBridge` method that currently returns `false`:

* **Puppets**: create a creature from a template (or clone the PC's
  appearance), set its position/facing, play an animation, and destroy it.
  Mirror the code path of NWScript `CreateObject`, `JumpToLocation` and
  `DestroyObject`.
* **Travel**: the engine call behind NWScript `StartNewModule(module, waypoint)`.
* **World state replay**: look up an object by tag in the current area
  (`GetObjectByTag`), then destroy it, open it, or mark it looted.

An alternative to calling engine internals one by one is to find the
NWScript VM's "run script" entry point. Then the mod can ship small compiled
scripts (`.ncs` in `Override/`, delivered by the updater) and run them with
parameters. One RE target unlocks all of the above.

## Prior art worth reading

* [xoreos](https://xoreos.org): open-source reimplementation of BioWare's
  Aurora/Odyssey engines; good for formats (GFF, 2DA, ERF) and engine concepts.
* KotOR Script Extender (K1SE, on Nexus Mods): loads into the game as a
  `binkw32.dll` proxy. That's another injection option if a player can't use
  the launcher.
* Ghidra-based K1 save-format write-ups (e.g. the rakata docs).
