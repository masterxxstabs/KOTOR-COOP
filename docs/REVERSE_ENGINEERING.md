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

## 1. Data (Cheat Engine is enough)

Pointer paths use Cheat Engine syntax: `"swkotor.exe+0x123456, 0x4, 0x10"`.
For each value, find the address, run *Pointer scan*, and keep a path that
survives restarting the game *and* loading another area.

| Key | Type | How to find it |
|---|---|---|
| `module_name` | char[] | Search for the text `end_m01aa` while on the Endar Spire; it should change to `end_m01ab` etc. |
| `loading_flag` | u8 | Changed-value scan across loading screens |
| `player_position` | f32 x3 | Walk around; unknown initial float, increased/decreased scans |
| `player_facing` | f32 | Usually near position in the creature object |
| `player_hp`, `player_max_hp` | i32 | Take damage and heal |
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
