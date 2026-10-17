//! Crash-proof memory access inside our own process. ReadProcessMemory on the
//! current process fails cleanly on unmapped pages instead of faulting, so a
//! wrong offset produces "no data", never a crashed game.

use crate::config::PointerPath;
use std::ffi::c_void;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub fn read_bytes(addr: usize, out: &mut [u8]) -> bool {
    if addr < 0x10000 {
        return false;
    }
    let mut n = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            addr as *const c_void,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut n,
        )
    };
    ok != 0 && n == out.len()
}

pub fn read<T: Copy + Default>(addr: usize) -> Option<T> {
    let mut v = T::default();
    let buf =
        unsafe { std::slice::from_raw_parts_mut((&mut v as *mut T).cast::<u8>(), size_of::<T>()) };
    read_bytes(addr, buf).then_some(v)
}

pub fn module_base(name: &str) -> Option<usize> {
    let c = std::ffi::CString::new(name).ok()?;
    let h = unsafe { GetModuleHandleA(c.as_ptr().cast()) };
    (!h.is_null()).then_some(h as usize)
}

/// Resolves a pointer path to the final address (not dereferenced).
pub fn resolve(path: &PointerPath) -> Option<usize> {
    let mut addr = match &path.module {
        Some(m) => module_base(m)?.wrapping_add(path.base as usize),
        None => path.base as usize,
    };
    for off in &path.offsets {
        let ptr: u32 = read(addr)?;
        addr = (ptr as usize).wrapping_add(*off as usize);
    }
    Some(addr)
}

/// Reads a NUL-terminated ASCII string of at most `max` bytes.
pub fn read_cstr(addr: usize, max: usize) -> Option<String> {
    let mut buf = vec![0u8; max];
    if !read_bytes(addr, &mut buf) {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    let s = std::str::from_utf8(&buf[..end]).ok()?;
    Some(s.to_owned())
}
