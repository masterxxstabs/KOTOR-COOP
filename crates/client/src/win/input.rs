//! Chat input: subclasses the game window's message procedure. F8 opens the
//! chat line, Enter sends, Escape cancels. While typing, key messages are
//! swallowed so the game's window procedure doesn't see them. (KOTOR also
//! reads the keyboard through DirectInput, which this can't block; see docs.)

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{WindowFromDC, HDC};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_BACK, VK_ESCAPE, VK_F8, VK_RETURN};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static TYPING: Mutex<Option<String>> = Mutex::new(None);
static SUBMITTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MAX_INPUT: usize = 200;

pub fn ensure_installed(hdc: HDC) {
    if ORIGINAL_WNDPROC.load(Ordering::Acquire) != 0 {
        return;
    }
    unsafe {
        let hwnd = WindowFromDC(hdc);
        if hwnd.is_null() {
            return;
        }
        let prev = SetWindowLongW(hwnd, GWL_WNDPROC, wndproc as *const () as usize as i32);
        if prev != 0 {
            ORIGINAL_WNDPROC.store(prev as isize, Ordering::Release);
            super::log("chat input installed (F8)");
        }
    }
}

pub fn current_text() -> Option<String> {
    TYPING.lock().unwrap().clone()
}

pub fn take_submitted() -> Vec<String> {
    std::mem::take(&mut *SUBMITTED.lock().unwrap())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let mut typing = TYPING.lock().unwrap();
    match (msg, typing.is_some()) {
        (WM_KEYDOWN, false) if wp == VK_F8 as usize => {
            *typing = Some(String::new());
            return 0;
        }
        (WM_KEYDOWN, true) => {
            match wp as u16 {
                VK_RETURN => {
                    let line = typing.take().unwrap_or_default();
                    if !line.trim().is_empty() {
                        SUBMITTED.lock().unwrap().push(line);
                    }
                }
                VK_ESCAPE | VK_F8 => *typing = None,
                VK_BACK => {
                    typing.as_mut().unwrap().pop();
                }
                _ => {}
            }
            return 0;
        }
        (WM_CHAR, true) => {
            if let Some(c) = char::from_u32(wp as u32).filter(|c| !c.is_control()) {
                let t = typing.as_mut().unwrap();
                if t.len() < MAX_INPUT {
                    t.push(c);
                }
            }
            return 0;
        }
        (WM_KEYUP | WM_SYSCHAR, true) => return 0,
        _ => {}
    }
    drop(typing);
    let orig: WNDPROC = std::mem::transmute(ORIGINAL_WNDPROC.load(Ordering::Acquire));
    CallWindowProcW(orig, hwnd, msg, wp, lp)
}
