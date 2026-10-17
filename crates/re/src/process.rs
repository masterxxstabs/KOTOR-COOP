//! Read-only access to a running process (Windows). Opens it with
//! PROCESS_VM_READ only: the probe never writes to the game.

use crate::scan::{Memory, Region};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

pub struct ProcessMemory {
    handle: HANDLE,
    regions: Vec<Region>,
    image: (usize, usize),
    pub pid: u32,
    pub exe_path: PathBuf,
}

impl Drop for ProcessMemory {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn find_pid(exe_name: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            if wide_to_string(&e.szExeFile).eq_ignore_ascii_case(exe_name) {
                found = Some(e.th32ProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        found
    }
}

fn main_module(pid: u32) -> Option<(usize, usize, PathBuf)> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut m: MODULEENTRY32W = std::mem::zeroed();
        m.dwSize = size_of::<MODULEENTRY32W>() as u32;
        // The first module of a process is its executable.
        let res = (Module32FirstW(snap, &mut m) != 0).then(|| {
            (
                m.modBaseAddr as usize,
                m.modBaseSize as usize,
                PathBuf::from(wide_to_string(&m.szExePath)),
            )
        });
        CloseHandle(snap);
        res
    }
}

impl ProcessMemory {
    pub fn attach(exe_name: &str) -> Result<ProcessMemory, String> {
        let pid = find_pid(exe_name).ok_or_else(|| format!("{exe_name} is not running"))?;
        let (base, size, exe_path) = main_module(pid).ok_or(
            "could not read the game's modules (run the launcher as the same user as the game)",
        )?;
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "could not open {exe_name}: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut pm = ProcessMemory {
            handle,
            regions: Vec::new(),
            image: (base, size),
            pid,
            exe_path,
        };
        pm.regions = pm.enumerate_regions();
        Ok(pm)
    }

    fn enumerate_regions(&self) -> Vec<Region> {
        let readable = PAGE_READONLY
            | PAGE_READWRITE
            | PAGE_WRITECOPY
            | PAGE_EXECUTE_READ
            | PAGE_EXECUTE_READWRITE
            | PAGE_EXECUTE_WRITECOPY;
        let (img, img_size) = self.image;
        let mut out = Vec::new();
        let mut addr = 0usize;
        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            let n = unsafe {
                VirtualQueryEx(
                    self.handle,
                    addr as *const _,
                    &mut mbi,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if n == 0 {
                break;
            }
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            let ok_prot =
                mbi.Protect & readable != 0 && mbi.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0;
            // Heap objects live in private memory; static roots in the exe image.
            let interesting = mbi.Type == MEM_PRIVATE
                || (mbi.Type == MEM_IMAGE && base >= img && base < img + img_size);
            if mbi.State == MEM_COMMIT && ok_prot && interesting {
                out.push(Region { base, size });
            }
            match base.checked_add(size) {
                Some(next) if next > addr => addr = next,
                _ => break,
            }
        }
        out
    }

    pub fn total_bytes(&self) -> usize {
        self.regions.iter().map(|r| r.size).sum()
    }
}

impl Memory for ProcessMemory {
    fn regions(&self) -> &[Region] {
        &self.regions
    }

    fn read(&self, addr: usize, buf: &mut [u8]) -> bool {
        let mut n = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const _,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut n,
            )
        };
        ok != 0 && n == buf.len()
    }

    fn image(&self) -> (usize, usize) {
        self.image
    }
}
