//! KOTOR Co-op client DLL.
//!
//! The launcher starts `swkotor.exe` suspended and injects `kotor_coop.dll`.
//! On load the DLL:
//!
//! 1. reads `kotor-coop/session.toml` (server, name, token) and
//!    `kotor-coop/offsets.toml` (memory layout for this game build),
//! 2. starts a network thread that connects to the dedicated server,
//! 3. hooks the game's OpenGL buffer swap so co-op logic and the overlay run
//!    once per frame on the game thread.
//!
//! Everything engine-specific sits behind [`coop::GameBridge`]; anything not
//! mapped in `offsets.toml` for the running build is switched off cleanly.

pub mod config;
pub mod coop;
pub mod net;

// KOTOR is 32-bit only, and the engine calls use x86 `thiscall`.
#[cfg(all(windows, target_arch = "x86"))]
mod win;
