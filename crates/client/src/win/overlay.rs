//! Minimal text overlay drawn with legacy OpenGL bitmap fonts, inside the
//! game's own GL context right before the frame is presented.

use super::Runtime;
use std::fmt::Write;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::OpenGL::*;

const FIRST_GLYPH: u32 = 32;
const GLYPHS: u32 = 96;
const LINE_HEIGHT: i32 = 16;

#[derive(Default)]
pub struct Overlay {
    font_lists: u32,
    failed: bool,
}

impl Overlay {
    fn ensure_font(&mut self, hdc: HDC) -> bool {
        if self.font_lists != 0 {
            return true;
        }
        if self.failed {
            return false;
        }
        unsafe {
            let font = CreateFontA(
                -14,
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                ANSI_CHARSET as u32,
                OUT_TT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                (FF_DONTCARE | DEFAULT_PITCH) as u32,
                c"Consolas".as_ptr().cast(),
            );
            let old = SelectObject(hdc, font);
            let base = glGenLists(GLYPHS as i32);
            let ok = base != 0 && wglUseFontBitmapsA(hdc, FIRST_GLYPH, GLYPHS, base) != 0;
            SelectObject(hdc, old);
            DeleteObject(font);
            if ok {
                self.font_lists = base;
            } else {
                self.failed = true;
                super::log("overlay: could not build font, overlay disabled");
            }
            ok
        }
    }

    pub fn draw(&mut self, hdc: HDC, coop: &Runtime, input: Option<&str>) {
        if !self.ensure_font(hdc) {
            return;
        }
        let mut lines = Vec::new();
        let mut head = format!("KOTOR Co-op | {}", coop.status);
        if let Some(p) = coop.ping_ms {
            let _ = write!(head, " | {p} ms");
        }
        lines.push((head, [0.55, 0.8, 1.0]));
        if coop.my_id.is_some() {
            let mut party = String::from("Party: you");
            if coop.is_leader() {
                party.push_str(" (leader)");
            }
            for (id, p) in &coop.players {
                let lead = if coop.leader == Some(*id) {
                    " (leader)"
                } else {
                    ""
                };
                let place = p
                    .state
                    .as_ref()
                    .map(|s| s.module.as_str())
                    .unwrap_or("loading");
                let _ = write!(party, ", {}{lead} [{place}]", p.name);
            }
            lines.push((party, [0.85, 0.85, 0.85]));
        }
        for l in &coop.chat {
            lines.push((l.clone(), [1.0, 1.0, 1.0]));
        }
        match input {
            Some(t) => lines.push((format!("Say: {t}_"), [1.0, 0.9, 0.4])),
            None => lines.push(("F8: chat".into(), [0.6, 0.6, 0.6])),
        }

        unsafe {
            let mut vp = [0i32; 4];
            glGetIntegerv(GL_VIEWPORT, vp.as_mut_ptr());
            glPushAttrib(GL_ALL_ATTRIB_BITS);
            glMatrixMode(GL_PROJECTION);
            glPushMatrix();
            glLoadIdentity();
            glOrtho(0.0, vp[2] as f64, vp[3] as f64, 0.0, -1.0, 1.0);
            glMatrixMode(GL_MODELVIEW);
            glPushMatrix();
            glLoadIdentity();
            for cap in [
                GL_TEXTURE_2D,
                GL_DEPTH_TEST,
                GL_LIGHTING,
                GL_FOG,
                GL_CULL_FACE,
                GL_ALPHA_TEST,
                GL_BLEND,
            ] {
                glDisable(cap);
            }
            // ARB vertex/fragment programs, if the game left them enabled.
            glDisable(0x8620);
            glDisable(0x8804);
            glListBase(self.font_lists - FIRST_GLYPH);

            for (i, (text, rgb)) in lines.iter().enumerate() {
                let bytes: Vec<u8> = text
                    .bytes()
                    .map(|b| if (32..128).contains(&b) { b } else { b'?' })
                    .collect();
                let y = 20 + i as i32 * LINE_HEIGHT;
                glColor3f(0.0, 0.0, 0.0); // drop shadow
                glRasterPos2i(11, y + 1);
                glCallLists(bytes.len() as i32, GL_UNSIGNED_BYTE, bytes.as_ptr().cast());
                glColor3f(rgb[0], rgb[1], rgb[2]);
                glRasterPos2i(10, y);
                glCallLists(bytes.len() as i32, GL_UNSIGNED_BYTE, bytes.as_ptr().cast());
            }

            glMatrixMode(GL_MODELVIEW);
            glPopMatrix();
            glMatrixMode(GL_PROJECTION);
            glPopMatrix();
            glPopAttrib();
        }
    }
}
