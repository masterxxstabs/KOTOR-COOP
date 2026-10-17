//! Admin commands typed into the server's terminal.

use crate::SharedHub;
use kotor_protocol::GlobalValue;

pub const HELP: &str = "\
commands:
  status                    server, world and player summary
  players                   list connected players
  say <text>                broadcast a server message
  kick <name>               disconnect a player
  ban <name>                kick and ban a player (saved to config at runtime only)
  module <name> [waypoint]  move the party to a module
  global <name> [value]     show or set a plot global (true/false/number/text)
  reset-module <name>       forget dead NPCs/looted containers in a module
  save                      save the world now
  quit                      save and stop the server";

pub enum ConsoleResult {
    Output(String),
    Save,
    Quit,
}

pub fn run_command(hub: &SharedHub, line: &str) -> ConsoleResult {
    let line = line.trim();
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    let mut h = hub.lock().unwrap();
    let out = match cmd {
        "" => String::new(),
        "help" | "?" => HELP.into(),
        "save" => return ConsoleResult::Save,
        "quit" | "exit" | "stop" => return ConsoleResult::Quit,
        "status" => {
            let w = &h.data.world;
            format!(
                "{} | {}/{} players | module: {} | world time: {}h{:02}m | globals: {} | accounts: {}",
                h.config.name,
                h.player_count(),
                h.config.max_players,
                w.current_module.as_deref().unwrap_or("(new campaign)"),
                w.world_time_secs / 3600,
                (w.world_time_secs / 60) % 60,
                w.globals.len(),
                h.data.accounts.len(),
            )
        }
        "players" => {
            let lines = h.player_lines();
            if lines.is_empty() {
                "nobody online".into()
            } else {
                lines.join("\n")
            }
        }
        "say" if !rest.is_empty() => {
            h.system_chat(rest);
            format!("[Server] {rest}")
        }
        "kick" if !rest.is_empty() => {
            if h.kick(rest, "removed by admin") {
                format!("kicked {rest}")
            } else {
                format!("{rest} is not online")
            }
        }
        "ban" if !rest.is_empty() => {
            h.config.banned.push(rest.to_owned());
            h.kick(rest, "banned");
            format!("banned {rest} (add to config `banned` to make it permanent)")
        }
        "module" if !rest.is_empty() => {
            let (module, wp) = rest.split_once(' ').unwrap_or((rest, ""));
            h.transition(module.to_owned(), wp.trim().to_owned(), None);
            format!("party moved to {module}")
        }
        "global" if !rest.is_empty() => match rest.split_once(' ') {
            None => match h.data.world.globals.get(rest) {
                Some(v) => format!("{rest} = {v:?}"),
                None => format!("{rest} is unset"),
            },
            Some((name, value)) => {
                let value = parse_global(value.trim());
                h.data.world.globals.insert(name.to_owned(), value.clone());
                h.dirty = true;
                h.broadcast(&kotor_protocol::ServerMessage::GlobalChanged {
                    name: name.to_owned(),
                    value: value.clone(),
                });
                format!("{name} = {value:?}")
            }
        },
        "reset-module" if !rest.is_empty() => {
            if h.data.world.modules.remove(rest).is_some() {
                h.dirty = true;
                format!("{rest} reset; takes effect next time players load it")
            } else {
                format!("no saved state for {rest}")
            }
        }
        _ => format!("unknown command `{line}` (try `help`)"),
    };
    ConsoleResult::Output(out)
}

fn parse_global(v: &str) -> GlobalValue {
    match v {
        "true" => GlobalValue::Boolean(true),
        "false" => GlobalValue::Boolean(false),
        _ => v
            .parse()
            .map(GlobalValue::Number)
            .unwrap_or_else(|_| GlobalValue::String(v.into())),
    }
}
