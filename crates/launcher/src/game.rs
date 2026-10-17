//! Finding the game, writing the session file and starting swkotor.exe with
//! the co-op DLL injected.

use crate::update::COOP_DIR;
use std::path::{Path, PathBuf};

pub const GAME_EXE: &str = "swkotor.exe";
pub const CLIENT_DLL: &str = "kotor_coop.dll";

pub fn is_game_dir(p: &Path) -> bool {
    p.join(GAME_EXE).is_file() && p.join("chitin.key").is_file()
}

/// Library folders listed in Steam's libraryfolders.vdf (`"path"  "D:\\Games"` lines).
fn steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![steam_root.to_path_buf()];
    if let Ok(text) =
        std::fs::read_to_string(steam_root.join("steamapps").join("libraryfolders.vdf"))
    {
        for line in text.lines() {
            let parts: Vec<&str> = line.split('"').collect();
            if parts.len() >= 4 && parts[1] == "path" {
                libs.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
            }
        }
    }
    libs
}

/// Best-effort search of Steam, GOG and common install locations.
pub fn detect() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut steam_roots: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    {
        if let Some(p) = win::reg_string(true, r"Software\Valve\Steam", "SteamPath") {
            steam_roots.push(PathBuf::from(p));
        }
        // GOG.com game id of Knights of the Old Republic.
        for key in [
            r"SOFTWARE\WOW6432Node\GOG.com\Games\1207666283",
            r"SOFTWARE\GOG.com\Games\1207666283",
        ] {
            if let Some(p) = win::reg_string(false, key, "path") {
                candidates.push(PathBuf::from(p));
            }
        }
        steam_roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        candidates.push(PathBuf::from(r"C:\GOG Games\Star Wars - KotOR"));
        candidates.push(PathBuf::from(r"C:\Program Files (x86)\LucasArts\SWKotOR"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        steam_roots.push(home.join(".steam/steam"));
        steam_roots.push(home.join(".local/share/Steam"));
    }

    for root in steam_roots {
        for lib in steam_libraries(&root) {
            candidates.push(lib.join("steamapps").join("common").join("swkotor"));
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if is_game_dir(&c) && !found.contains(&c) {
            found.push(c);
        }
    }
    found
}

pub struct Session<'a> {
    pub server: &'a str,
    pub player_name: &'a str,
    pub player_token: &'a str,
    pub password: Option<&'a str>,
    pub mod_version: Option<&'a str>,
}

/// Writes `kotor-coop/session.toml`, which the DLL reads on startup.
pub fn write_session(game: &Path, s: &Session) -> anyhow::Result<()> {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        server: &'a str,
        player_name: &'a str,
        player_token: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mod_version: Option<&'a str>,
    }
    let out = Out {
        server: s.server,
        player_name: s.player_name,
        player_token: s.player_token,
        password: s.password,
        mod_version: s.mod_version,
    };
    let dir = game.join(COOP_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("session.toml"), toml::to_string(&out)?)?;
    Ok(())
}

/// Starts the game with the co-op DLL loaded. Windows only (also works under Wine
/// when the launcher itself runs in Wine).
pub fn launch(game: &Path) -> anyhow::Result<()> {
    let dll = game.join(COOP_DIR).join(CLIENT_DLL);
    anyhow::ensure!(
        dll.is_file(),
        "{} is missing; run the update first",
        dll.display()
    );
    #[cfg(windows)]
    {
        win::launch_injected(&game.join(GAME_EXE), game, &dll)
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("starting the game is only supported on Windows (run the Windows launcher, e.g. under Wine/Proton)")
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
    };
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::System::Threading::*;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn reg_string(current_user: bool, key: &str, value: &str) -> Option<String> {
        let root = if current_user {
            HKEY_CURRENT_USER
        } else {
            HKEY_LOCAL_MACHINE
        };
        let (k, v) = (wide(key.as_ref()), wide(value.as_ref()));
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                root,
                k.as_ptr(),
                v.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if rc != 0 {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// CreateProcess(suspended) -> LoadLibraryW(dll) via a remote thread -> resume.
    /// The launcher is built as 32-bit like the game, so kernel32's
    /// LoadLibraryW address is the same in both processes.
    pub fn launch_injected(exe: &Path, cwd: &Path, dll: &Path) -> anyhow::Result<()> {
        unsafe {
            let exe_w = wide(exe.as_os_str());
            let cwd_w = wide(cwd.as_os_str());
            let mut cmd = wide(format!("\"{}\"", exe.display()).as_ref());
            let mut si: STARTUPINFOW = std::mem::zeroed();
            si.cb = size_of::<STARTUPINFOW>() as u32;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                exe_w.as_ptr(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED,
                std::ptr::null(),
                cwd_w.as_ptr(),
                &si,
                &mut pi,
            );
            anyhow::ensure!(
                ok != 0,
                "could not start {}: {}",
                exe.display(),
                std::io::Error::last_os_error()
            );

            let result = (|| -> anyhow::Result<()> {
                let path = wide(dll.as_os_str());
                let bytes = path.len() * 2;
                let remote = VirtualAllocEx(
                    pi.hProcess,
                    std::ptr::null(),
                    bytes,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                );
                anyhow::ensure!(
                    !remote.is_null(),
                    "VirtualAllocEx failed: {}",
                    std::io::Error::last_os_error()
                );
                let mut written = 0;
                let ok = WriteProcessMemory(
                    pi.hProcess,
                    remote,
                    path.as_ptr().cast(),
                    bytes,
                    &mut written,
                );
                anyhow::ensure!(
                    ok != 0,
                    "WriteProcessMemory failed: {}",
                    std::io::Error::last_os_error()
                );

                let k32 = GetModuleHandleA(c"kernel32.dll".as_ptr().cast());
                let load = GetProcAddress(k32, c"LoadLibraryW".as_ptr().cast())
                    .ok_or_else(|| anyhow::anyhow!("LoadLibraryW not found"))?;
                let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "system" fn(*mut c_void) -> u32,
                >(load));
                let thread = CreateRemoteThread(
                    pi.hProcess,
                    std::ptr::null(),
                    0,
                    start,
                    remote,
                    0,
                    std::ptr::null_mut(),
                );
                anyhow::ensure!(
                    !thread.is_null(),
                    "CreateRemoteThread failed: {}",
                    std::io::Error::last_os_error()
                );
                let waited = WaitForSingleObject(thread, 15_000);
                let mut module = 0u32;
                GetExitCodeThread(thread, &mut module);
                CloseHandle(thread);
                VirtualFreeEx(pi.hProcess, remote, 0, MEM_RELEASE);
                anyhow::ensure!(waited == WAIT_OBJECT_0, "timed out loading the co-op DLL");
                anyhow::ensure!(
                    module != 0,
                    "the game refused to load {} (wrong architecture or missing file?)",
                    dll.display()
                );
                Ok(())
            })();

            if result.is_err() {
                TerminateProcess(pi.hProcess, 1);
            } else {
                ResumeThread(pi.hThread);
            }
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            result
        }
    }
}
