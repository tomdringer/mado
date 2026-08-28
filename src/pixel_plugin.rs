// ─── Pixel plugin host ────────────────────────────────────────────────────────
//
// Spawns an external binary that renders UI by writing raw RGBA frames to
// stdout. Mado composites those frames directly into a sidebar panel as a
// slint::Image, bypassing the terminal emulator entirely.
//
// ── Frame protocol (plugin stdout) ───────────────────────────────────────────
//
//   Pixel frame:
//     [4 bytes]  magic: b"MADO"
//     [4 bytes]  width  as u32 little-endian  (physical pixels)
//     [4 bytes]  height as u32 little-endian  (physical pixels)
//     [w*h*4 B]  RGBA8 pixel data, row-major, top-to-bottom
//
//   Action message:
//     [4 bytes]  magic: b"MACT"
//     [4 bytes]  JSON length as u32 little-endian
//     [N bytes]  UTF-8 JSON, e.g. {"action":"paste"}
//
// ── Event protocol (Mado → plugin stdin, newline-delimited JSON) ──────────────
//
//   {"type":"resize","width":600,"height":800}
//   {"type":"click","x":42.0,"y":17.0,"button":"left"}
//   {"type":"key","key":"Return","ctrl":false,"meta":false}
//   {"type":"scroll","delta":-3.0}

use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use slint::SharedPixelBuffer;

pub struct PixelPlugin {
    pub dirty:         Arc<AtomicBool>,
    pub image:         Arc<Mutex<Option<SharedPixelBuffer<slint::Rgba8Pixel>>>>,
    /// Set by the reader thread when the plugin sends a MACT paste action.
    /// Cleared by the render timer after acting on it.
    pub paste_pending: Arc<AtomicBool>,
    stdin:             std::process::ChildStdin,
    _child:            std::process::Child,
}

impl PixelPlugin {
    pub fn spawn(command: &str, args: &[&str], width: u32, height: u32) -> Option<Self> {
        let mut child = std::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;

        let mut stdin  = child.stdin.take()?;
        let     stdout = child.stdout.take()?;

        let dirty:         Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let image:         Arc<Mutex<Option<SharedPixelBuffer<slint::Rgba8Pixel>>>> =
            Arc::new(Mutex::new(None));
        let paste_pending: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        let _ = writeln!(stdin,
            r#"{{"type":"resize","width":{width},"height":{height}}}"#);

        {
            let dirty         = Arc::clone(&dirty);
            let image         = Arc::clone(&image);
            let paste_pending = Arc::clone(&paste_pending);
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut magic = [0u8; 4];
                    if reader.read_exact(&mut magic).is_err() { break; }

                    match &magic {
                        b"MADO" => {
                            let mut dim = [0u8; 8];
                            if reader.read_exact(&mut dim).is_err() { break; }
                            let w = u32::from_le_bytes(dim[0..4].try_into().unwrap());
                            let h = u32::from_le_bytes(dim[4..8].try_into().unwrap());
                            if w == 0 || h == 0 || w > 8192 || h > 8192 { continue; }

                            let n = (w * h * 4) as usize;
                            let mut pixels = vec![0u8; n];
                            if reader.read_exact(&mut pixels).is_err() { break; }

                            let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
                            buf.make_mut_bytes().copy_from_slice(&pixels);
                            if let Ok(mut g) = image.lock() { *g = Some(buf); }
                            dirty.store(true, Ordering::Relaxed);
                        }
                        b"MACT" => {
                            let mut len_bytes = [0u8; 4];
                            if reader.read_exact(&mut len_bytes).is_err() { break; }
                            let len = u32::from_le_bytes(len_bytes) as usize;
                            if len > 4096 { break; } // sanity guard
                            let mut json = vec![0u8; len];
                            if reader.read_exact(&mut json).is_err() { break; }
                            // Only action we support for now is "paste"
                            if json.windows(7).any(|w| w == b"\"paste\"") {
                                paste_pending.store(true, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            eprintln!("mado: pixel plugin bad magic {:?} — stopping", magic);
                            break;
                        }
                    }
                }
            });
        }

        Some(PixelPlugin { dirty, image, paste_pending, stdin, _child: child })
    }

    pub fn send_resize(&mut self, width: u32, height: u32) {
        let _ = writeln!(self.stdin,
            r#"{{"type":"resize","width":{width},"height":{height}}}"#);
    }

    pub fn send_click(&mut self, x: f32, y: f32) {
        let _ = writeln!(self.stdin,
            r#"{{"type":"click","x":{x:.1},"y":{y:.1},"button":"left"}}"#);
    }

    pub fn send_key(&mut self, text: &str, ctrl: bool, meta: bool) {
        // serde_json produces valid JSON escapes (e.g. "\u0008" for backspace).
        // Rust's {:?} produces Rust-style "\u{8}" which is invalid JSON.
        let json_text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        let _ = writeln!(self.stdin,
            r#"{{"type":"key","text":{json_text},"ctrl":{ctrl},"meta":{meta}}}"#);
    }

    pub fn send_focus(&mut self, focused: bool) {
        let kind = if focused { "focus" } else { "blur" };
        let _ = writeln!(self.stdin, r#"{{"type":"{kind}"}}"#);
    }

    pub fn send_scroll(&mut self, delta: f32) {
        let _ = writeln!(self.stdin,
            r#"{{"type":"scroll","delta":{delta:.1}}}"#);
    }
}
