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
//   {"type":"key","text":"Return","ctrl":false,"meta":false}
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

// ── Pure event-format helpers ─────────────────────────────────────────────────
//
// These build the JSON strings that Mado writes to plugin stdin.
// Keeping them separate from the I/O lets unit tests verify exact format
// without spawning a real child process.

pub(crate) fn fmt_resize(width: u32, height: u32) -> String {
    format!(r#"{{"type":"resize","width":{width},"height":{height}}}"#)
}

pub(crate) fn fmt_click(x: f32, y: f32) -> String {
    format!(r#"{{"type":"click","x":{x:.1},"y":{y:.1},"button":"left"}}"#)
}

pub(crate) fn fmt_mouse_press(x: f32, y: f32) -> String {
    format!(r#"{{"type":"mouse_press","x":{x:.1},"y":{y:.1}}}"#)
}

pub(crate) fn fmt_mouse_move(x: f32, y: f32) -> String {
    format!(r#"{{"type":"mouse_move","x":{x:.1},"y":{y:.1}}}"#)
}

pub(crate) fn fmt_mouse_release() -> String {
    r#"{"type":"mouse_release"}"#.to_string()
}

pub(crate) fn fmt_key(text: &str, ctrl: bool, meta: bool, alt: bool, shift: bool) -> String {
    // serde_json produces valid JSON escapes (e.g. "\u0008" for backspace).
    // Rust's {:?} produces Rust-style "\u{8}" which is invalid JSON.
    let json_text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"type":"key","text":{json_text},"ctrl":{ctrl},"meta":{meta},"alt":{alt},"shift":{shift}}}"#)
}

pub(crate) fn fmt_focus(focused: bool) -> String {
    let kind = if focused { "focus" } else { "blur" };
    format!(r#"{{"type":"{kind}"}}"#)
}

pub(crate) fn fmt_scroll(delta: f32) -> String {
    format!(r#"{{"type":"scroll","delta":{delta:.1}}}"#)
}

pub(crate) fn fmt_paste(text: &str) -> String {
    let json_text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"type":"paste","text":{json_text}}}"#)
}

pub(crate) fn fmt_chdir(path: &str) -> String {
    let json_path = serde_json::to_string(path).unwrap_or_else(|_| "\"\"".to_string());
    format!(r#"{{"type":"chdir","path":{json_path}}}"#)
}

/// Returns `true` if `json` contains the `"paste"` action string.
pub(crate) fn mact_is_paste(json: &[u8]) -> bool {
    json.windows(7).any(|w| w == b"\"paste\"")
}

/// Validate MADO frame dimensions.  Returns `(width, height)` only when
/// both are non-zero and within the 8192-pixel cap.
pub(crate) fn validate_frame_dims(w: u32, h: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || w > 8192 || h > 8192 { None } else { Some((w, h)) }
}

// ── PixelPlugin ───────────────────────────────────────────────────────────────

impl PixelPlugin {
    pub fn spawn(command: &str, args: &[&str], width: u32, height: u32,
                 env: &[(&str, &str)]) -> Option<Self> {
        let mut cmd = std::process::Command::new(command);
        cmd.args(args)
           .stdin(std::process::Stdio::piped())
           .stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::null());
        for (k, v) in env { cmd.env(k, v); }
        let mut child = cmd.spawn().ok()?;

        let mut stdin  = child.stdin.take()?;
        let     stdout = child.stdout.take()?;

        let dirty:         Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let image:         Arc<Mutex<Option<SharedPixelBuffer<slint::Rgba8Pixel>>>> =
            Arc::new(Mutex::new(None));
        let paste_pending: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        let _ = writeln!(stdin, "{}", fmt_resize(width, height));

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
                            if validate_frame_dims(w, h).is_none() { continue; }

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
                            if mact_is_paste(&json) {
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
        let _ = writeln!(self.stdin, "{}", fmt_resize(width, height));
    }

    pub fn send_click(&mut self, x: f32, y: f32) {
        let _ = writeln!(self.stdin, "{}", fmt_click(x, y));
    }

    pub fn send_mouse_press(&mut self, x: f32, y: f32) {
        let _ = writeln!(self.stdin, "{}", fmt_mouse_press(x, y));
    }

    pub fn send_mouse_move(&mut self, x: f32, y: f32) {
        let _ = writeln!(self.stdin, "{}", fmt_mouse_move(x, y));
    }

    pub fn send_mouse_release(&mut self) {
        let _ = writeln!(self.stdin, "{}", fmt_mouse_release());
    }

    pub fn send_key(&mut self, text: &str, ctrl: bool, meta: bool, alt: bool, shift: bool) {
        let _ = writeln!(self.stdin, "{}", fmt_key(text, ctrl, meta, alt, shift));
    }

    pub fn send_focus(&mut self, focused: bool) {
        let _ = writeln!(self.stdin, "{}", fmt_focus(focused));
    }

    pub fn send_scroll(&mut self, delta: f32) {
        let _ = writeln!(self.stdin, "{}", fmt_scroll(delta));
    }

    pub fn send_paste(&mut self, text: &str) {
        let _ = writeln!(self.stdin, "{}", fmt_paste(text));
    }

    pub fn send_chdir(&mut self, path: &str) {
        let _ = writeln!(self.stdin, "{}", fmt_chdir(path));
    }

    pub fn send_paste_image(&mut self, base64_data: &str, media_type: &str) {
        let json = format!(
            r#"{{"type":"paste_image","data":"{base64_data}","media_type":"{media_type}"}}"#
        );
        let _ = writeln!(self.stdin, "{json}");
    }
}

impl Drop for PixelPlugin {
    fn drop(&mut self) {
        // Kill the child process when the plugin panel is collapsed or removed,
        // so it doesn't orphan. The reader thread exits naturally once stdout closes.
        let _ = self._child.kill();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Parse the JSON produced by fmt_key (or any other fmt_* function) and
    /// return the value at `field` as a String.
    fn json_str(json: &str, field: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        v[field].as_str().unwrap_or("").to_string()
    }

    fn json_bool(json: &str, field: &str) -> bool {
        let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        v[field].as_bool().unwrap_or(false)
    }

    fn json_f64(json: &str, field: &str) -> f64 {
        let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        v[field].as_f64().unwrap_or(0.0)
    }

    fn json_u64(json: &str, field: &str) -> u64 {
        let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        v[field].as_u64().unwrap_or(0)
    }

    // ── Frame dimension validation ─────────────────────────────────────────────

    #[test]
    fn valid_frame_dims_accepted() {
        assert_eq!(validate_frame_dims(100, 200), Some((100, 200)));
        assert_eq!(validate_frame_dims(1, 1),     Some((1, 1)));
        assert_eq!(validate_frame_dims(8192, 8192), Some((8192, 8192)));
    }

    #[test]
    fn zero_width_rejected() {
        assert_eq!(validate_frame_dims(0, 100), None);
    }

    #[test]
    fn zero_height_rejected() {
        assert_eq!(validate_frame_dims(100, 0), None);
    }

    #[test]
    fn oversized_width_rejected() {
        assert_eq!(validate_frame_dims(8193, 100), None);
    }

    #[test]
    fn oversized_height_rejected() {
        assert_eq!(validate_frame_dims(100, 8193), None);
    }

    #[test]
    fn both_zero_rejected() {
        assert_eq!(validate_frame_dims(0, 0), None);
    }

    // ── MACT paste detection ──────────────────────────────────────────────────

    #[test]
    fn mact_paste_detected() {
        assert!(mact_is_paste(br#"{"action":"paste"}"#));
    }

    #[test]
    fn mact_non_paste_not_detected() {
        assert!(!mact_is_paste(br#"{"action":"copy"}"#));
        assert!(!mact_is_paste(br#"{"action":"reload"}"#));
        assert!(!mact_is_paste(b""));
    }

    #[test]
    fn mact_paste_substring_not_confused() {
        // "paste_image" starts with "paste" but is a distinct action.
        // The 7-byte window search looks for exactly `"paste"` (with closing quote);
        // "paste_image" has `"paste_` at that position, so it must NOT match.
        assert!(!mact_is_paste(br#"{"action":"paste_image"}"#));
    }

    // ── resize event ──────────────────────────────────────────────────────────

    #[test]
    fn resize_event_format() {
        let msg = fmt_resize(640, 480);
        assert_eq!(json_str(&msg, "type"), "resize");
        assert_eq!(json_u64(&msg, "width"),  640);
        assert_eq!(json_u64(&msg, "height"), 480);
    }

    // ── mouse events ──────────────────────────────────────────────────────────

    #[test]
    fn click_event_format() {
        let msg = fmt_click(12.5, 34.0);
        assert_eq!(json_str(&msg, "type"), "click");
        assert!((json_f64(&msg, "x") - 12.5).abs() < 0.01);
        assert!((json_f64(&msg, "y") - 34.0).abs() < 0.01);
        assert_eq!(json_str(&msg, "button"), "left");
    }

    #[test]
    fn mouse_press_event_format() {
        let msg = fmt_mouse_press(5.0, 10.0);
        assert_eq!(json_str(&msg, "type"), "mouse_press");
        assert!((json_f64(&msg, "x") - 5.0).abs() < 0.01);
        assert!((json_f64(&msg, "y") - 10.0).abs() < 0.01);
    }

    #[test]
    fn mouse_move_event_format() {
        let msg = fmt_mouse_move(100.0, 200.0);
        assert_eq!(json_str(&msg, "type"), "mouse_move");
        assert!((json_f64(&msg, "x") - 100.0).abs() < 0.01);
        assert!((json_f64(&msg, "y") - 200.0).abs() < 0.01);
    }

    #[test]
    fn mouse_release_event_format() {
        let msg = fmt_mouse_release();
        assert_eq!(json_str(&msg, "type"), "mouse_release");
    }

    // ── scroll event ──────────────────────────────────────────────────────────

    #[test]
    fn scroll_event_format() {
        let msg = fmt_scroll(-3.0);
        assert_eq!(json_str(&msg, "type"), "scroll");
        assert!((json_f64(&msg, "delta") - -3.0).abs() < 0.01);
    }

    #[test]
    fn scroll_positive_delta() {
        let msg = fmt_scroll(5.5);
        assert!((json_f64(&msg, "delta") - 5.5).abs() < 0.01);
    }

    // ── focus / blur events ───────────────────────────────────────────────────

    #[test]
    fn focus_event_format() {
        let msg = fmt_focus(true);
        assert_eq!(json_str(&msg, "type"), "focus");
    }

    #[test]
    fn blur_event_format() {
        let msg = fmt_focus(false);
        assert_eq!(json_str(&msg, "type"), "blur");
    }

    // ── key event — modifier flags ─────────────────────────────────────────────

    #[test]
    fn key_no_modifiers() {
        let msg = fmt_key("a", false, false, false, false);
        assert_eq!(json_str(&msg, "type"), "key");
        assert_eq!(json_str(&msg, "text"), "a");
        assert!(!json_bool(&msg, "ctrl"));
        assert!(!json_bool(&msg, "meta"));
        assert!(!json_bool(&msg, "alt"));
        assert!(!json_bool(&msg, "shift"));
    }

    #[test]
    fn key_ctrl_held() {
        let msg = fmt_key("c", true, false, false, false);
        assert!(json_bool(&msg, "ctrl"));
        assert!(!json_bool(&msg, "meta"));
        assert!(!json_bool(&msg, "alt"));
        assert!(!json_bool(&msg, "shift"));
        assert_eq!(json_str(&msg, "text"), "c");
    }

    #[test]
    fn key_cmd_held() {
        let msg = fmt_key("v", false, true, false, false);
        assert!(!json_bool(&msg, "ctrl"));
        assert!(json_bool(&msg, "meta"));
        assert!(!json_bool(&msg, "alt"));
        assert!(!json_bool(&msg, "shift"));
        assert_eq!(json_str(&msg, "text"), "v");
    }

    #[test]
    fn key_alt_held() {
        let msg = fmt_key("b", false, false, true, false);
        assert!(json_bool(&msg, "alt"));
        assert!(!json_bool(&msg, "ctrl"));
        assert!(!json_bool(&msg, "meta"));
        assert!(!json_bool(&msg, "shift"));
    }

    #[test]
    fn key_shift_held() {
        let msg = fmt_key("A", false, false, false, true);
        assert!(json_bool(&msg, "shift"));
        assert_eq!(json_str(&msg, "text"), "A");
    }

    #[test]
    fn key_all_modifiers() {
        let msg = fmt_key("z", true, true, true, true);
        assert!(json_bool(&msg, "ctrl"));
        assert!(json_bool(&msg, "meta"));
        assert!(json_bool(&msg, "alt"));
        assert!(json_bool(&msg, "shift"));
    }

    // Cmd+C / Cmd+V / Cmd+X copy-paste combos
    #[test]
    fn key_cmd_c_copy() {
        let msg = fmt_key("c", false, true, false, false);
        assert!(json_bool(&msg, "meta"));
        assert_eq!(json_str(&msg, "text"), "c");
    }

    #[test]
    fn key_cmd_v_paste() {
        let msg = fmt_key("v", false, true, false, false);
        assert!(json_bool(&msg, "meta"));
        assert_eq!(json_str(&msg, "text"), "v");
    }

    #[test]
    fn key_cmd_x_cut() {
        let msg = fmt_key("x", false, true, false, false);
        assert!(json_bool(&msg, "meta"));
        assert_eq!(json_str(&msg, "text"), "x");
    }

    // ── key event — special characters ────────────────────────────────────────

    #[test]
    fn key_tilde() {
        let msg = fmt_key("~", false, false, false, false);
        assert_eq!(json_str(&msg, "text"), "~");
    }

    #[test]
    fn key_exclamation() { assert_eq!(json_str(&fmt_key("!", false, false, false, false), "text"), "!"); }

    #[test]
    fn key_at_sign() { assert_eq!(json_str(&fmt_key("@", false, false, false, false), "text"), "@"); }

    #[test]
    fn key_hash() { assert_eq!(json_str(&fmt_key("#", false, false, false, false), "text"), "#"); }

    #[test]
    fn key_dollar() { assert_eq!(json_str(&fmt_key("$", false, false, false, false), "text"), "$"); }

    #[test]
    fn key_percent() { assert_eq!(json_str(&fmt_key("%", false, false, false, false), "text"), "%"); }

    #[test]
    fn key_caret() { assert_eq!(json_str(&fmt_key("^", false, false, false, false), "text"), "^"); }

    #[test]
    fn key_ampersand() { assert_eq!(json_str(&fmt_key("&", false, false, false, false), "text"), "&"); }

    #[test]
    fn key_asterisk() { assert_eq!(json_str(&fmt_key("*", false, false, false, false), "text"), "*"); }

    #[test]
    fn key_parens() {
        assert_eq!(json_str(&fmt_key("(", false, false, false, false), "text"), "(");
        assert_eq!(json_str(&fmt_key(")", false, false, false, false), "text"), ")");
    }

    #[test]
    fn key_minus_underscore() {
        assert_eq!(json_str(&fmt_key("-", false, false, false, false), "text"), "-");
        assert_eq!(json_str(&fmt_key("_", false, false, false, false), "text"), "_");
    }

    #[test]
    fn key_plus_equals() {
        assert_eq!(json_str(&fmt_key("+", false, false, false, false), "text"), "+");
        assert_eq!(json_str(&fmt_key("=", false, false, false, false), "text"), "=");
    }

    #[test]
    fn key_brackets() {
        assert_eq!(json_str(&fmt_key("[", false, false, false, false), "text"), "[");
        assert_eq!(json_str(&fmt_key("]", false, false, false, false), "text"), "]");
        assert_eq!(json_str(&fmt_key("{", false, false, false, false), "text"), "{");
        assert_eq!(json_str(&fmt_key("}", false, false, false, false), "text"), "}");
    }

    #[test]
    fn key_pipe_backslash() {
        assert_eq!(json_str(&fmt_key("|", false, false, false, false), "text"), "|");
        assert_eq!(json_str(&fmt_key("\\", false, false, false, false), "text"), "\\");
    }

    #[test]
    fn key_semicolon_colon() {
        assert_eq!(json_str(&fmt_key(";", false, false, false, false), "text"), ";");
        assert_eq!(json_str(&fmt_key(":", false, false, false, false), "text"), ":");
    }

    #[test]
    fn key_quotes() {
        assert_eq!(json_str(&fmt_key("'", false, false, false, false), "text"), "'");
        assert_eq!(json_str(&fmt_key("\"", false, false, false, false), "text"), "\"");
    }

    #[test]
    fn key_comma_period_slash() {
        assert_eq!(json_str(&fmt_key(",", false, false, false, false), "text"), ",");
        assert_eq!(json_str(&fmt_key(".", false, false, false, false), "text"), ".");
        assert_eq!(json_str(&fmt_key("/", false, false, false, false), "text"), "/");
    }

    #[test]
    fn key_angle_brackets_question() {
        assert_eq!(json_str(&fmt_key("<", false, false, false, false), "text"), "<");
        assert_eq!(json_str(&fmt_key(">", false, false, false, false), "text"), ">");
        assert_eq!(json_str(&fmt_key("?", false, false, false, false), "text"), "?");
    }

    #[test]
    fn key_accented_unicode() {
        // Common accented characters must pass through as valid JSON strings.
        for ch in ["é", "ñ", "ü", "ç", "ā", "ō", "ß", "å", "ø"] {
            let msg = fmt_key(ch, false, false, false, false);
            assert_eq!(json_str(&msg, "text"), ch, "failed for '{ch}'");
        }
    }

    // ── key event — JSON escaping for control characters ──────────────────────

    #[test]
    fn key_backspace_is_valid_json() {
        // Slint sends U+0008 for Backspace; serde_json must escape it as \u0008.
        let msg = fmt_key("\u{0008}", false, false, false, false);
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("backspace must produce valid JSON");
        assert_eq!(parsed["text"].as_str().unwrap(), "\u{0008}");
    }

    #[test]
    fn key_return_is_valid_json() {
        let msg = fmt_key("\r", false, false, false, false);
        let _: serde_json::Value = serde_json::from_str(&msg)
            .expect("Return must produce valid JSON");
    }

    #[test]
    fn key_tab_is_valid_json() {
        let msg = fmt_key("\t", false, false, false, false);
        let _: serde_json::Value = serde_json::from_str(&msg)
            .expect("Tab must produce valid JSON");
    }

    #[test]
    fn key_escape_is_valid_json() {
        let msg = fmt_key("\u{001B}", false, false, false, false);
        let _: serde_json::Value = serde_json::from_str(&msg)
            .expect("Escape must produce valid JSON");
    }

    // ── modifier-only guard (pixel plugin MUST NOT forward these) ────────────
    //
    // Slint fires key-pressed events for bare modifier presses using private
    // codepoints.  Forwarding them to a plugin inserts garbage control chars.
    // The guard is in keys::is_modifier_only; verify it covers every codepoint.

    #[test]
    fn shift_left_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0010}"));
    }

    #[test]
    fn shift_right_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0015}"));
    }

    #[test]
    fn control_left_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0011}"));
    }

    #[test]
    fn control_right_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0016}"));
    }

    #[test]
    fn alt_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0012}"));
    }

    #[test]
    fn altgr_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0013}"));
    }

    #[test]
    fn capslock_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0014}"));
    }

    #[test]
    fn meta_left_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0017}"));
    }

    #[test]
    fn meta_right_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0018}"));
    }

    #[test]
    fn backtab_is_modifier_only() {
        assert!(keys::is_modifier_only("\u{0019}"));
    }

    // Regular characters must NOT be blocked by the modifier-only guard.
    #[test]
    fn printable_chars_are_not_modifier_only() {
        for ch in ["a", "A", "z", "Z", "0", "9",
                   "~", "!", "@", "#", "$", "%", "^", "&", "*",
                   "(", ")", "-", "_", "+", "=",
                   "[", "]", "{", "}", "|", "\\",
                   ";", ":", "'", "\"", ",", ".", "/",
                   "<", ">", "?", " ", "\t", "\r"] {
            assert!(!keys::is_modifier_only(ch), "wrongly blocked: {ch:?}");
        }
    }

    #[test]
    fn letter_with_ctrl_not_modifier_only() {
        // Ctrl+letter arrives as a C0 char (U+0001–U+001A), not a modifier codepoint.
        assert!(!keys::is_modifier_only("\u{0001}")); // Ctrl+A  — C0, not modifier-only
        assert!(!keys::is_modifier_only("\u{0003}")); // Ctrl+C  — C0, not modifier-only
        // U+0016 is Control (R) in Slint's private range — it IS modifier-only.
        // Ctrl+V as typed arrives as the Slint text "v" with ctrl=true, not as \u{0016}.
        assert!(keys::is_modifier_only("\u{0016}")); // Control (R) — must be filtered
    }

    // ── paste event ───────────────────────────────────────────────────────────

    #[test]
    fn paste_plain_text() {
        let msg = fmt_paste("hello world");
        assert_eq!(json_str(&msg, "type"), "paste");
        assert_eq!(json_str(&msg, "text"), "hello world");
    }

    #[test]
    fn paste_with_newlines() {
        let msg = fmt_paste("line1\nline2\r\nline3");
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("newlines in paste must produce valid JSON");
        assert_eq!(parsed["text"].as_str().unwrap(), "line1\nline2\r\nline3");
    }

    #[test]
    fn paste_with_special_chars() {
        let text = r#"she said "hello" & it's <great>"#;
        let msg = fmt_paste(text);
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("special chars in paste must produce valid JSON");
        assert_eq!(parsed["text"].as_str().unwrap(), text);
    }

    #[test]
    fn paste_with_unicode() {
        let text = "こんにちは 🦀 Héllo";
        let msg = fmt_paste(text);
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("unicode in paste must produce valid JSON");
        assert_eq!(parsed["text"].as_str().unwrap(), text);
    }

    #[test]
    fn paste_with_backslash_and_quote() {
        let text = "path\\to\\file and \"quoted\"";
        let msg = fmt_paste(text);
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("backslash/quote in paste must produce valid JSON");
        assert_eq!(parsed["text"].as_str().unwrap(), text);
    }

    // ── chdir event ───────────────────────────────────────────────────────────

    #[test]
    fn chdir_event_format() {
        let msg = fmt_chdir("/Users/alice/projects/mado");
        assert_eq!(json_str(&msg, "type"), "chdir");
        assert_eq!(json_str(&msg, "path"), "/Users/alice/projects/mado");
    }

    #[test]
    fn chdir_path_with_spaces() {
        let msg = fmt_chdir("/Users/alice/My Documents/project");
        let parsed: serde_json::Value = serde_json::from_str(&msg)
            .expect("path with spaces must be valid JSON");
        assert_eq!(parsed["path"].as_str().unwrap(), "/Users/alice/My Documents/project");
    }

    // ── all fmt_* functions produce valid JSON ─────────────────────────────────

    #[test]
    fn all_events_are_valid_json() {
        let events = [
            fmt_resize(800, 600),
            fmt_click(1.0, 2.0),
            fmt_mouse_press(3.0, 4.0),
            fmt_mouse_move(5.0, 6.0),
            fmt_mouse_release(),
            fmt_key("a", false, false, false, false),
            fmt_key("A", false, false, false, true),
            fmt_key("c", true,  false, false, false),
            fmt_key("v", false, true,  false, false),
            fmt_key("~", false, false, false, false),
            fmt_key("\"", false, false, false, false),
            fmt_focus(true),
            fmt_focus(false),
            fmt_scroll(-1.5),
            fmt_paste("some text"),
            fmt_chdir("/tmp"),
        ];
        for e in &events {
            serde_json::from_str::<serde_json::Value>(e)
                .unwrap_or_else(|err| panic!("invalid JSON: {err}\n  input: {e}"));
        }
    }
}
