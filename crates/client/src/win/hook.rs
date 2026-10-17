//! Frame hook. KOTOR renders with OpenGL, so every frame ends in
//! `SwapBuffers` (gdi32) or `wglSwapBuffers` (opengl32). Both are public
//! exports, which means this hook needs no reverse-engineered addresses:
//!
//! 1. Prefer patching swkotor.exe's import table (IAT) entry.
//! 2. Otherwise hot-patch `wglSwapBuffers` (Windows functions start with a
//!    2-byte `mov edi, edi` preceded by 5 bytes of padding for exactly this).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::core::BOOL;
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};

type SwapFn = unsafe extern "system" fn(HDC) -> BOOL;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static CALLBACK: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn swap_detour(hdc: HDC) -> BOOL {
    let cb = CALLBACK.load(Ordering::Acquire);
    if cb != 0 {
        let cb: fn(HDC) = std::mem::transmute(cb);
        cb(hdc);
    }
    let orig: SwapFn = std::mem::transmute(ORIGINAL.load(Ordering::Acquire));
    orig(hdc)
}

fn proc_address(dll: &[u8], name: &[u8]) -> Option<usize> {
    unsafe {
        let mut h = GetModuleHandleA(dll.as_ptr());
        if h.is_null() {
            h = LoadLibraryA(dll.as_ptr());
        }
        if h.is_null() {
            return None;
        }
        GetProcAddress(h, name.as_ptr()).map(|f| f as usize)
    }
}

unsafe fn write_protected(addr: usize, bytes: &[u8]) -> bool {
    let mut old = 0u32;
    if VirtualProtect(
        addr as *const c_void,
        bytes.len(),
        PAGE_EXECUTE_READWRITE,
        &mut old,
    ) == 0
    {
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
    VirtualProtect(addr as *const c_void, bytes.len(), old, &mut old);
    true
}

/// Finds IAT slots in the main executable that hold `target` and redirects them.
unsafe fn patch_iat(target: usize, replacement: usize) -> bool {
    let base = GetModuleHandleA(std::ptr::null()) as usize;
    let rd = |off: usize| super::mem::read::<u32>(base + off).map(|v| v as usize);
    let Some(e_lfanew) = rd(0x3C) else {
        return false;
    };
    // PE32: signature(4) + file header(20) + optional header; data dir #1 = imports at +104.
    let opt = e_lfanew + 4 + 20;
    let (Some(magic), Some(import_rva)) = (super::mem::read::<u16>(base + opt), rd(opt + 104))
    else {
        return false;
    };
    if magic != 0x10B || import_rva == 0 {
        return false;
    }
    let mut patched = false;
    let mut desc = base + import_rva;
    // IMAGE_IMPORT_DESCRIPTOR is 20 bytes; FirstThunk (the IAT) is at +16.
    while let Some(first_thunk) = super::mem::read::<u32>(desc + 16).filter(|&v| v != 0) {
        let mut slot = base + first_thunk as usize;
        while let Some(value) = super::mem::read::<u32>(slot).filter(|&v| v != 0) {
            if value as usize == target
                && write_protected(slot, &(replacement as u32).to_le_bytes())
            {
                patched = true;
            }
            slot += 4;
        }
        desc += 20;
    }
    patched
}

unsafe fn hotpatch(func: usize, replacement: usize) -> Option<usize> {
    let mut head = [0u8; 7];
    if !super::mem::read_bytes(func - 5, &mut head) {
        return None;
    }
    let padding_ok = head[..5].iter().all(|&b| b == 0x90 || b == 0xCC);
    if !padding_ok || head[5..] != [0x8B, 0xFF] {
        return None;
    }
    let rel = (replacement as isize - func as isize) as i32; // jmp rel32 from func-5 ends at func
    let mut jmp = [0xE9u8, 0, 0, 0, 0];
    jmp[1..].copy_from_slice(&rel.to_le_bytes());
    if !write_protected(func - 5, &jmp) {
        return None;
    }
    // Set the trampoline target before the short jump goes live.
    let resume = func + 2;
    ORIGINAL.store(resume, Ordering::Release);
    write_protected(func, &[0xEB, 0xF9]).then_some(resume) // jmp short -7 -> func-5
}

pub fn install_swap_hook(callback: fn(HDC)) -> Result<&'static str, String> {
    CALLBACK.store(callback as *const () as usize, Ordering::Release);
    let detour = swap_detour as *const () as usize;
    unsafe {
        for (dll, name, label) in [
            (
                &b"gdi32.dll\0"[..],
                &b"SwapBuffers\0"[..],
                "IAT gdi32!SwapBuffers",
            ),
            (
                &b"opengl32.dll\0"[..],
                &b"wglSwapBuffers\0"[..],
                "IAT opengl32!wglSwapBuffers",
            ),
        ] {
            if let Some(target) = proc_address(dll, name) {
                ORIGINAL.store(target, Ordering::Release);
                if patch_iat(target, detour) {
                    return Ok(label);
                }
            }
        }
        let wgl =
            proc_address(b"opengl32.dll\0", b"wglSwapBuffers\0").ok_or("opengl32 not loaded")?;
        hotpatch(wgl, detour)
            .map(|_| "hot-patch opengl32!wglSwapBuffers")
            .ok_or_else(|| "wglSwapBuffers is not hot-patchable on this system".to_owned())
    }
}
