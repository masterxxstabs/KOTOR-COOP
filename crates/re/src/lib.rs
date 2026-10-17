//! Reverse-engineering support for KOTOR Co-op: reads the game's file formats
//! and scans a running `swkotor.exe` to discover the memory layout the client
//! DLL needs (see docs/REVERSE_ENGINEERING.md).

pub mod erf;
pub mod gff;
pub mod probe;
pub mod save;
pub mod scan;

#[cfg(windows)]
pub mod process;
