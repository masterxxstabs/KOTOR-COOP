mod bridge;
mod hook;
mod input;
mod mem;
mod overlay;

use crate::config::{OffsetsFile, Session};
use crate::coop::{Coop, GlobalKind};
use crate::net::NetLink;
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HMODULE, TRUE};
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

type Runtime = Coop<bridge::OdysseyBridge, NetLink>;

static DLL_DIR: OnceLock<PathBuf> = OnceLock::new();
static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);
static OVERLAY: Mutex<Option<overlay::Overlay>> = Mutex::new(None);

pub(crate) fn log(msg: &str) {
    let Some(dir) = DLL_DIR.get() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("client.log"))
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

fn module_path(module: HMODULE) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// Exported so the launcher (or a debugger) can confirm which build is loaded.
#[no_mangle]
pub extern "C" fn kotor_coop_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe { DisableThreadLibraryCalls(module) };
        // Never do real work under the loader lock.
        let module = module as usize;
        std::thread::spawn(move || {
            if std::panic::catch_unwind(|| init(module as HMODULE)).is_err() {
                log("initialisation panicked; co-op disabled");
            }
        });
    }
    TRUE
}

fn init(module: HMODULE) {
    let Some(dir) = module_path(module).and_then(|p| p.parent().map(PathBuf::from)) else {
        return;
    };
    let _ = DLL_DIR.set(dir.clone());
    log(&format!("KOTOR Co-op {} loaded", env!("CARGO_PKG_VERSION")));

    let session = match Session::load(&dir) {
        Ok(s) => s,
        Err(e) => {
            log(&format!(
                "no session ({e}); start the game from the KOTOR Co-op launcher"
            ));
            return;
        }
    };

    let exe = module_path(std::ptr::null_mut());
    let build = exe
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| hex::encode(Sha256::digest(b)));
    log(&format!(
        "game: {:?} sha256={}",
        exe,
        build.as_deref().unwrap_or("?")
    ));

    let offsets = OffsetsFile::load(&dir).unwrap_or_else(|e| {
        log(&format!("offsets: {e}"));
        OffsetsFile::default()
    });
    let layout = build.as_deref().and_then(|h| offsets.for_build(h)).cloned();
    match &layout {
        Some(l) => log(&format!("using offsets for '{}'", l.name)),
        None => log("this swkotor.exe build is not mapped in offsets.toml; running in network/overlay-only mode"),
    }
    let tracked = offsets
        .tracked_globals
        .iter()
        .map(|g| {
            (
                g.name.clone(),
                if g.kind == "boolean" {
                    GlobalKind::Boolean
                } else {
                    GlobalKind::Number
                },
            )
        })
        .collect();

    let bridge = bridge::OdysseyBridge::new(layout.unwrap_or_default());
    for line in bridge.capability_report() {
        log(&line);
    }
    let link = NetLink::start(session, build, log);
    *RUNTIME.lock().unwrap() = Some(Coop::new(bridge, link, tracked));

    match hook::install_swap_hook(on_swap) {
        Ok(how) => log(&format!("frame hook installed via {how}")),
        Err(e) => log(&format!("could not hook rendering ({e}); co-op cannot run")),
    }
}

/// Runs on the game's render thread right before each buffer swap.
fn on_swap(hdc: HDC) {
    let res = std::panic::catch_unwind(|| {
        input::ensure_installed(hdc);
        let mut rt = RUNTIME.lock().unwrap();
        let Some(coop) = rt.as_mut() else { return };
        for line in input::take_submitted() {
            coop.say(&line);
        }
        coop.on_frame(Instant::now());

        let mut ov = OVERLAY.lock().unwrap();
        let ov = ov.get_or_insert_with(overlay::Overlay::default);
        ov.draw(hdc, coop, input::current_text().as_deref());
    });
    if res.is_err() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| log("panic in frame hook (suppressed further reports)"));
    }
}
