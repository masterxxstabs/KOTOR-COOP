//! `kotor-launcher probe`: maps the running game's memory using your own
//! saves as ground truth, then writes `kotor-coop/offsets.local.toml`.

use crate::update::{sha256_file, COOP_DIR};
use anyhow::Context;
use kotor_re::probe::ProbeState;
use kotor_re::save;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const STATE_FILE: &str = "probe.json";
pub const LOCAL_OFFSETS: &str = "offsets.local.toml";
const EXE: &str = "swkotor.exe";

fn state_path(game: &Path) -> PathBuf {
    game.join(COOP_DIR).join(STATE_FILE)
}

fn load_state(game: &Path) -> anyhow::Result<Option<ProbeState>> {
    match std::fs::read(state_path(game)) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).context("probe.json is corrupt; run `probe reset`")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_state(game: &Path, st: &ProbeState) -> anyhow::Result<()> {
    std::fs::create_dir_all(game.join(COOP_DIR))?;
    std::fs::write(state_path(game), serde_json::to_vec(st)?)?;
    Ok(())
}

pub fn reset(game: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(state_path(game)) {
        Ok(()) => println!("probe data cleared"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("nothing to reset"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn status(game: &Path) -> anyhow::Result<()> {
    let Some(st) = load_state(game)? else {
        println!("No probe in progress. Start with `kotor-launcher probe`.");
        return Ok(());
    };
    println!(
        "{} round(s) | module_name: {} | player_position: {} | nearby fields: {} | {}",
        st.rounds,
        st.module_name.len(),
        st.position.len(),
        st.near.len(),
        if st.confident() {
            "ready"
        } else {
            "needs more rounds"
        }
    );
    println!("\n{}", st.to_offsets_toml(EXE, "preview"));
    Ok(())
}

pub fn run(game: &Path, save_dir: Option<PathBuf>) -> anyhow::Result<()> {
    run_with(game, save_dir, scan_game)
}

type Scanner = fn(
    &Path,
    &mut ProbeState,
    &save::PlayerSnapshot,
) -> anyhow::Result<kotor_re::probe::RoundReport>;

fn run_with(game: &Path, save_dir: Option<PathBuf>, scan: Scanner) -> anyhow::Result<()> {
    let exe_hash = sha256_file(&game.join(EXE))?;
    let mut st = match load_state(game)? {
        Some(st) if st.exe_sha256 != exe_hash => {
            anyhow::bail!("probe data was collected for a different swkotor.exe; run `kotor-launcher probe reset`")
        }
        Some(st) => st,
        None => ProbeState::new(&exe_hash),
    };

    let save_dir = match save_dir {
        Some(d) => d,
        None => save::latest_save(game)?,
    };
    let snap = save::read_player(&save_dir)
        .with_context(|| format!("reading save {}", save_dir.display()))?;
    let age = std::fs::metadata(save_dir.join("SAVEGAME.sav"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default();
    println!(
        "Save: {} ({} min old)\n  area {} | position {:?} | hp {:?}/{:?} | appearance {:?}",
        save_dir.display(),
        age.as_secs() / 60,
        snap.module,
        snap.position,
        snap.hp,
        snap.max_hp,
        snap.appearance
    );
    if age > Duration::from_secs(10 * 60) {
        println!("  warning: this save is old. Quicksave right now, standing still, so it matches the game's memory.");
    }

    let started = Instant::now();
    let report = scan(game, &mut st, &snap)?;
    for l in &report.lines {
        println!("  {l}");
    }
    for w in &report.warnings {
        println!("  note: {w}");
    }
    println!(
        "  (round {} took {:.1}s)",
        report.round,
        started.elapsed().as_secs_f32()
    );
    save_state(game, &st)?;

    if st.module_name.is_empty() || st.position.is_empty() {
        println!(
            "\nNo candidates left for {}. Usually the save didn't match the game's state \
             (moved after saving, or loaded a different save). Run `kotor-launcher probe reset` and start over.",
            if st.module_name.is_empty() { "module_name" } else { "player_position" }
        );
        return Ok(());
    }
    if !st.confident() {
        println!(
            "\nNext: travel to a DIFFERENT area (or load an older save in another area), walk a few steps, \
             stand still, quicksave, then run `kotor-launcher probe` again. Restarting the game in between \
             makes the result even more reliable. Taking some damage lets HP be verified too."
        );
        return Ok(());
    }

    let toml = st.to_offsets_toml(EXE, &format!("probed locally ({} rounds)", st.rounds));
    let path = game.join(COOP_DIR).join(LOCAL_OFFSETS);
    std::fs::write(
        &path,
        format!(
            "# Written by `kotor-launcher probe`. Share it to help map official offsets.\n\n{toml}"
        ),
    )?;
    println!("\nMapped. Wrote {}:\n\n{toml}", path.display());
    println!(
        "Restart KOTOR from the launcher: the overlay will now show your area, and your position syncs to the server.\n\
         You can keep running `probe` after more area changes to narrow it down further."
    );
    Ok(())
}

#[cfg(windows)]
fn scan_game(
    game: &Path,
    st: &mut ProbeState,
    snap: &save::PlayerSnapshot,
) -> anyhow::Result<kotor_re::probe::RoundReport> {
    let mem = kotor_re::process::ProcessMemory::attach(EXE).map_err(|e| anyhow::anyhow!(e))?;
    let running_hash = sha256_file(&mem.exe_path).unwrap_or_default();
    if running_hash != st.exe_sha256 {
        anyhow::bail!(
            "the running game ({}) is not the swkotor.exe in {}; probe the install you play with",
            mem.exe_path.display(),
            game.display()
        );
    }
    println!(
        "  scanning {} MB of game memory (pid {})...",
        mem.total_bytes() >> 20,
        mem.pid
    );
    Ok(st.round(&mem, snap, kotor_re::scan::PointerScanParams::default()))
}

#[cfg(not(windows))]
fn scan_game(
    _: &Path,
    _: &mut ProbeState,
    _: &save::PlayerSnapshot,
) -> anyhow::Result<kotor_re::probe::RoundReport> {
    anyhow::bail!("probing reads the running game's memory, so it only works on Windows (or the Windows launcher under Wine)")
}

pub fn dump_save(game: Option<&Path>, dir: Option<PathBuf>) -> anyhow::Result<()> {
    let dir = match (dir, game) {
        (Some(d), _) => d,
        (None, Some(g)) => save::latest_save(g)?,
        (None, None) => anyhow::bail!("pass a save folder or --game"),
    };
    println!("{}", dir.display());
    println!("{:#?}", save::read_player(&dir)?);
    match save::global_names(&dir) {
        Ok(lists) => {
            for (label, names) in lists {
                println!(
                    "{label}: {} globals (e.g. {})",
                    names.len(),
                    names.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }
        Err(e) => println!("globals: {e}"),
    }
    Ok(())
}

pub fn dump_gff(file: &Path) -> anyhow::Result<()> {
    let g = kotor_re::gff::parse(&std::fs::read(file)?)?;
    let mut out = format!("{} file\n", g.file_type);
    kotor_re::gff::dump(&g.root, 0, &mut out);
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kotor_re::save::PlayerSnapshot;
    use kotor_re::scan::fake::FakeMemory;
    use kotor_re::scan::PointerScanParams;
    use std::cell::RefCell;

    thread_local! {
        static GAME: RefCell<Option<FakeMemory>> = const { RefCell::new(None) };
    }

    /// Minimal fake game: exe+0x40 -> app; app+0x4 -> creature (pos +0x90, hp +0x120);
    /// app+0x8 -> CExoString { char* } -> module name.
    fn fake(module: &str, pos: [f32; 3], hp: i32, shift: usize) -> FakeMemory {
        let mut m = FakeMemory::new(0x400000, 0x1000);
        let (app, pc, s, name) = (
            0x1000_0000 + shift,
            0x2000_0000 + 3 * shift,
            0x3000_0000 + 5 * shift,
            0x4000_0000 + 7 * shift,
        );
        for b in [app, pc, s, name] {
            m.add(b, vec![0; 0x1000]);
        }
        m.write_u32(0x400040, app as u32);
        m.write_u32(app + 4, pc as u32);
        m.write_u32(app + 8, s as u32);
        m.write_u32(s, name as u32);
        m.write(name, format!("{module}\0").as_bytes());
        m.write(
            pc + 0x90,
            &pos.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>(),
        );
        m.write(pc + 0x120, &hp.to_le_bytes());
        m
    }

    fn fake_scan(
        _: &Path,
        st: &mut ProbeState,
        snap: &PlayerSnapshot,
    ) -> anyhow::Result<kotor_re::probe::RoundReport> {
        GAME.with(|g| {
            Ok(st.round(
                g.borrow().as_ref().unwrap(),
                snap,
                PointerScanParams::default(),
            ))
        })
    }

    #[test]
    fn probe_rounds_write_offsets_the_client_can_load() {
        let game = std::env::temp_dir().join(format!("kotor-probe-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&game);
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(EXE), b"fake exe").unwrap();
        let exe_hash = sha256_file(&game.join(EXE)).unwrap();

        let rounds = [
            ("end_m01aa", [5.0f32, 6.0, 0.0], 40, 0usize),
            ("tar_m02aa", [-90.5, 12.0, 3.0], 31, 0x20000),
        ];
        for (i, (module, pos, hp, shift)) in rounds.into_iter().enumerate() {
            let snap = PlayerSnapshot {
                module: module.into(),
                position: pos,
                facing: None,
                hp: Some(hp as i64),
                max_hp: None,
                appearance: None,
                save_name: None,
            };
            save::write_fixture(&game.join(format!("saves/00000{i} - Game")), &snap).unwrap();
            // Make sure the newest-save pick is deterministic on coarse clocks.
            std::thread::sleep(Duration::from_millis(20));
            GAME.with(|g| *g.borrow_mut() = Some(fake(module, pos, hp, shift)));
            let dir = game.join(format!("saves/00000{i} - Game"));
            run_with(&game, Some(dir), fake_scan).unwrap();
        }

        let local = std::fs::read_to_string(game.join(COOP_DIR).join(LOCAL_OFFSETS)).unwrap();
        assert!(
            local.contains("module_name = \"swkotor.exe+0x40, 0x8, 0x0, 0x0\""),
            "{local}"
        );
        assert!(
            local.contains("player_position = \"swkotor.exe+0x40, 0x4, 0x90\""),
            "{local}"
        );
        // Narrowest matching type wins; 16 bits of a 32-bit HP still reads correctly.
        assert!(
            local.contains("player_hp = \"swkotor.exe+0x40, 0x4, 0x120 as i16\"\n"),
            "{local}"
        );

        // What the DLL will load from the game folder.
        let offsets = kotor_coop::config::OffsetsFile::load(&game.join(COOP_DIR)).unwrap();
        let build = offsets
            .for_build(&exe_hash)
            .expect("probed build is selected");
        for p in [&build.module_name, &build.player_position, &build.player_hp] {
            assert!(kotor_coop::config::PointerPath::parse(p).is_some(), "{p}");
        }
        std::fs::remove_dir_all(game).unwrap();
    }
}
