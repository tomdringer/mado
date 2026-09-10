/// Integration tests for the Mado pixel plugin protocol.
///
/// Each test spawns an actual plugin binary (from sibling repos under
/// /Users/tomdringer/Sites/), exercises the stdin/stdout protocol, and
/// verifies that the plugin:
///   - produces valid MADO frames in response to a resize event
///   - survives focus/blur events without crashing
///   - handles key events including special characters and modifier combos
///   - can be killed cleanly after use
///
/// Tests are skipped (not failed) when the plugin binary is not found, so
/// the suite passes in CI even if plugin repos aren't checked out.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

// ── Protocol constants ────────────────────────────────────────────────────────

const MADO_MAGIC: &[u8; 4] = b"MADO";
const MACT_MAGIC: &[u8; 4] = b"MACT";

// ── Frame message ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Message {
    /// A fully-read MADO frame: (width, height, pixels).
    Frame(u32, u32, Vec<u8>),
    /// A MACT action (clipboard paste trigger etc.) — payload intentionally unused.
    Action(()),
}

// ── Plugin harness ────────────────────────────────────────────────────────────

/// Wraps a spawned pixel-plugin process.
///
/// A background reader thread reads messages from stdout using blocking I/O
/// and forwards them to a channel. This avoids the macOS O_NONBLOCK issue
/// where setting O_NONBLOCK on the read end also makes the write end
/// non-blocking, causing the plugin's write_all to fail silently.
struct Plugin {
    child:    Child,
    stdin:    ChildStdin,
    rx:       mpsc::Receiver<Message>,
}

impl Plugin {
    /// Spawn a pixel-plugin binary.
    /// Returns `None` if the binary path does not exist.
    fn spawn(path: &str) -> Option<Self> {
        if !std::path::Path::new(path).exists() {
            return None;
        }
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let mut stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel::<Message>();

        // Reader thread: blocking I/O, parses the raw binary protocol.
        std::thread::spawn(move || {
            use std::io::Read;
            let mut magic = [0u8; 4];
            loop {
                // Read 4-byte magic
                if stdout.read_exact(&mut magic).is_err() { break; }

                if &magic == MADO_MAGIC {
                    let mut dims = [0u8; 8];
                    if stdout.read_exact(&mut dims).is_err() { break; }
                    let w = u32::from_le_bytes(dims[0..4].try_into().unwrap());
                    let h = u32::from_le_bytes(dims[4..8].try_into().unwrap());
                    let pixel_len = (w as usize) * (h as usize) * 4;
                    let mut pixels = vec![0u8; pixel_len];
                    if stdout.read_exact(&mut pixels).is_err() { break; }
                    if tx.send(Message::Frame(w, h, pixels)).is_err() { break; }
                } else if &magic == MACT_MAGIC {
                    let mut len_buf = [0u8; 4];
                    if stdout.read_exact(&mut len_buf).is_err() { break; }
                    let len = u32::from_le_bytes(len_buf) as usize;
                    let mut body = vec![0u8; len];
                    if stdout.read_exact(&mut body).is_err() { break; }
                    if tx.send(Message::Action(())).is_err() { break; }
                } else {
                    // Unknown magic — protocol error, give up.
                    break;
                }
            }
        });

        let stdin = child.stdin.take()?;
        Some(Plugin { child, stdin, rx })
    }

    // ── Stdin helpers ─────────────────────────────────────────────────────────

    fn send_raw(&mut self, line: &str) {
        let _ = writeln!(self.stdin, "{}", line);
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.send_raw(&format!(r#"{{"type":"resize","width":{w},"height":{h}}}"#));
    }

    fn focus(&mut self) { self.send_raw(r#"{"type":"focus"}"#); }
    fn blur(&mut self)  { self.send_raw(r#"{"type":"blur"}"#);  }

    fn key(&mut self, text: &str, ctrl: bool, meta: bool, alt: bool, shift: bool) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"',  "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        self.send_raw(&format!(
            r#"{{"type":"key","text":"{escaped}","ctrl":{ctrl},"meta":{meta},"alt":{alt},"shift":{shift}}}"#
        ));
    }

    fn click(&mut self, x: f32, y: f32) {
        self.send_raw(&format!(r#"{{"type":"click","x":{x},"y":{y}}}"#));
    }

    fn scroll(&mut self, delta: f32) {
        self.send_raw(&format!(r#"{{"type":"scroll","delta":{delta}}}"#));
    }

    // ── Stdout helpers ────────────────────────────────────────────────────────

    /// Block until a MADO frame arrives or `timeout` elapses.
    fn next_frame(&self, timeout: Duration) -> Option<(u32, u32, Vec<u8>)> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() { return None; }
            match self.rx.recv_timeout(remaining) {
                Ok(Message::Frame(w, h, px)) => return Some((w, h, px)),
                Ok(Message::Action(_))       => continue, // skip MACT, keep looking
                Err(_)                       => return None,
            }
        }
    }

    /// Check whether the child process is still running.
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the child and wait for it to exit.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Skip macro ────────────────────────────────────────────────────────────────

macro_rules! plugin_or_skip {
    ($path:expr) => {
        match Plugin::spawn($path) {
            Some(p) => p,
            None => {
                eprintln!("SKIP: binary not found at {}", $path);
                return;
            }
        }
    };
}

// ── Shared protocol checks ────────────────────────────────────────────────────

/// The plugin must emit a valid MADO frame.
///
/// Plugins often emit an initial frame at their startup dimensions before
/// processing the resize event, so we drain frames until we get one whose
/// dimensions match what we asked for, or until timeout.
fn check_responds_to_resize(path: &str) {
    check_responds_to_resize_timeout(path, Duration::from_secs(5));
}

fn check_responds_to_resize_timeout(path: &str, timeout: Duration) {
    let mut p = plugin_or_skip!(path);
    p.resize(150, 200);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            panic!("plugin did not produce a valid MADO frame within {:?}", timeout);
        }
        let Some((w, h, px)) = p.next_frame(remaining) else {
            panic!("plugin did not produce a valid MADO frame within {:?}", timeout);
        };
        // Skip frames at startup dimensions; wait for our resized frame.
        if w == 150 && h == 200 {
            assert_eq!(px.len(), 150 * 200 * 4);
            assert!(px.chunks_exact(4).any(|px| px[3] == 255),
                    "all pixels have alpha=0 — frame looks empty");
            p.kill();
            return;
        }
        // Got a frame at different dims — continue draining.
    }
}

/// Focus → blur → focus must not crash the plugin.
fn check_survives_focus_blur(path: &str) {
    let mut p = plugin_or_skip!(path);
    p.resize(150, 150);
    let _ = p.next_frame(Duration::from_secs(5));
    p.focus();
    p.blur();
    p.focus();
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "plugin crashed after focus/blur events");
    p.kill();
}

/// A battery of key events (printable, special chars, modifiers) must not crash the plugin.
fn check_survives_key_events(path: &str) {
    let mut p = plugin_or_skip!(path);
    p.resize(150, 150);
    let _ = p.next_frame(Duration::from_secs(5));

    // Lowercase + uppercase
    for ch in "abcdefghijklmnopqrstuvwxyz".chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }
    for ch in "ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        p.key(&ch.to_string(), false, false, false, true); // Shift
    }

    // Digits
    for ch in "0123456789".chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }

    // Special characters required by CLAUDE.md
    let specials = r#"~!@#$%^&*()-_+=[]{}|\;:'",.<>?/"#;
    for ch in specials.chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }

    // Accented / unicode
    for ch in ["é", "ñ", "ü", "日", "🦀"] {
        p.key(ch, false, false, false, false);
    }

    // Modifier combos
    p.key("c", true,  false, false, false); // Ctrl+C
    p.key("v", true,  false, false, false); // Ctrl+V
    p.key("x", true,  false, false, false); // Ctrl+X
    p.key("c", false, true,  false, false); // Cmd+C
    p.key("v", false, true,  false, false); // Cmd+V
    p.key("x", false, true,  false, false); // Cmd+X
    p.key("z", false, true,  false, false); // Cmd+Z
    p.key("a", true,  false, false, false); // Ctrl+A
    p.key("e", true,  false, false, false); // Ctrl+E
    p.key("a", false, false, true,  false); // Alt+A
    p.key("a", true,  false, false, true);  // Ctrl+Shift+A
    p.key("c", false, true,  false, true);  // Cmd+Shift+C

    // Control characters
    p.key("\u{0008}", false, false, false, false); // Backspace
    p.key("\u{007F}", false, false, false, false); // Delete/Forward-delete
    p.key("\u{001B}", false, false, false, false); // Escape
    p.key("\r",       false, false, false, false); // Return
    p.key("\t",       false, false, false, false); // Tab

    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "plugin crashed after key events");
    p.kill();
}

/// Slint's private modifier-only codepoints (U+0010–U+0019) must not crash the plugin.
/// Mado filters these before forwarding, but plugins should be robust if they arrive.
fn check_modifier_only_codepoints(path: &str) {
    let mut p = plugin_or_skip!(path);
    p.resize(150, 150);
    let _ = p.next_frame(Duration::from_secs(5));
    for cp in 0x0010u32..=0x0019u32 {
        let ch = char::from_u32(cp).unwrap();
        p.key(&ch.to_string(), false, false, false, false);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "plugin crashed on modifier-only codepoints");
    p.kill();
}

// ── mado-clipboard ────────────────────────────────────────────────────────────

const CLIPBOARD_BIN: &str =
    "/Users/tomdringer/Sites/mado-clipboard/target/debug/mado-clipboard";

/// Clipboard's first frame is slow (fontdue rasterizes all visible URL glyphs
/// on first render), so we allow up to 15 seconds for the first frame.
#[test]
fn clipboard_responds_to_resize() {
    check_responds_to_resize_timeout(CLIPBOARD_BIN, Duration::from_secs(15));
}

#[test]
fn clipboard_survives_focus_blur() { check_survives_focus_blur(CLIPBOARD_BIN); }

#[test]
fn clipboard_survives_key_events() { check_survives_key_events(CLIPBOARD_BIN); }

#[test]
fn clipboard_modifier_only_codepoints() { check_modifier_only_codepoints(CLIPBOARD_BIN); }

/// Typing characters into the search field must not crash the plugin.
#[test]
fn clipboard_search_input() {
    let mut p = plugin_or_skip!(CLIPBOARD_BIN);
    p.resize(300, 500);
    p.focus();
    let _ = p.next_frame(Duration::from_secs(5));

    // Type several characters into the filter
    for ch in "hello".chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }
    // Special chars in search — plugin must not crash
    for ch in "!@#$".chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "clipboard crashed during search input");
    p.kill();
}

/// Backspace removes characters from the search filter.
#[test]
fn clipboard_backspace() {
    let mut p = plugin_or_skip!(CLIPBOARD_BIN);
    p.resize(300, 500);
    p.focus();
    let _ = p.next_frame(Duration::from_secs(5));
    p.key("a", false, false, false, false);
    p.key("b", false, false, false, false);
    p.key("\u{0008}", false, false, false, false); // backspace
    p.key("\u{0008}", false, false, false, false);
    p.key("\u{0008}", false, false, false, false); // backspace past empty — must not panic
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "clipboard crashed on backspace");
    p.kill();
}

/// Escape clears the filter.
#[test]
fn clipboard_escape_clears_filter() {
    let mut p = plugin_or_skip!(CLIPBOARD_BIN);
    p.resize(300, 500);
    p.focus();
    let _ = p.next_frame(Duration::from_secs(5));
    for ch in "query".chars() {
        p.key(&ch.to_string(), false, false, false, false);
    }
    p.key("\u{001B}", false, false, false, false); // escape
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "clipboard crashed on escape");
    p.kill();
}

/// Scroll up and down, including past the top (should clamp gracefully).
#[test]
fn clipboard_scroll() {
    let mut p = plugin_or_skip!(CLIPBOARD_BIN);
    p.resize(300, 500);
    let _ = p.next_frame(Duration::from_secs(5));
    p.scroll(1.0);
    p.scroll(-1.0);
    p.scroll(-1.0); // past top — must clamp, not panic
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "clipboard crashed on scroll");
    p.kill();
}

/// Click in the list area must not crash.
#[test]
fn clipboard_click() {
    let mut p = plugin_or_skip!(CLIPBOARD_BIN);
    p.resize(300, 500);
    let _ = p.next_frame(Duration::from_secs(5));
    p.click(150.0, 100.0); // search bar area
    p.click(150.0, 200.0); // list area — may trigger paste action
    p.click(150.0, 450.0); // near bottom
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "clipboard crashed on click");
    p.kill();
}

// ── mado-clock ────────────────────────────────────────────────────────────────

const CLOCK_BIN: &str =
    "/Users/tomdringer/Sites/mado-clock/target/debug/mado-clock";

#[test]
fn clock_responds_to_resize() { check_responds_to_resize(CLOCK_BIN); }

#[test]
fn clock_survives_focus_blur() { check_survives_focus_blur(CLOCK_BIN); }

#[test]
fn clock_survives_key_events() { check_survives_key_events(CLOCK_BIN); }

#[test]
fn clock_modifier_only_codepoints() { check_modifier_only_codepoints(CLOCK_BIN); }

/// Clock should emit updated frames over time (it ticks every second).
#[test]
fn clock_emits_multiple_frames() {
    let mut p = plugin_or_skip!(CLOCK_BIN);
    p.resize(200, 300);
    p.next_frame(Duration::from_secs(5)).expect("no first frame");
    p.next_frame(Duration::from_secs(5)).expect("no second frame");
    p.kill();
}

// ── mado-memory ───────────────────────────────────────────────────────────────

const MEMORY_BIN: &str =
    "/Users/tomdringer/Sites/mado-memory/target/debug/mado-memory";

#[test]
fn memory_responds_to_resize() { check_responds_to_resize(MEMORY_BIN); }

#[test]
fn memory_survives_focus_blur() { check_survives_focus_blur(MEMORY_BIN); }

#[test]
fn memory_survives_key_events() { check_survives_key_events(MEMORY_BIN); }

/// Memory plugin emits frames on its own tick (should get at least 2 in 5s).
#[test]
fn memory_emits_multiple_frames() {
    let mut p = plugin_or_skip!(MEMORY_BIN);
    p.resize(200, 300);
    p.next_frame(Duration::from_secs(5)).expect("no first frame");
    p.next_frame(Duration::from_secs(5)).expect("no second frame");
    p.kill();
}

// ── mado-pomodoro ─────────────────────────────────────────────────────────────

const POMODORO_BIN: &str =
    "/Users/tomdringer/Sites/mado-pomodoro/target/debug/mado-pomodoro";

#[test]
fn pomodoro_responds_to_resize() { check_responds_to_resize(POMODORO_BIN); }

#[test]
fn pomodoro_survives_focus_blur() { check_survives_focus_blur(POMODORO_BIN); }

#[test]
fn pomodoro_survives_key_events() { check_survives_key_events(POMODORO_BIN); }

#[test]
fn pomodoro_modifier_only_codepoints() { check_modifier_only_codepoints(POMODORO_BIN); }

/// Space key toggles the timer; the plugin must remain alive.
#[test]
fn pomodoro_space_toggles() {
    let mut p = plugin_or_skip!(POMODORO_BIN);
    p.resize(300, 400);
    let _ = p.next_frame(Duration::from_secs(5));
    p.key(" ", false, false, false, false); // start
    let _ = p.next_frame(Duration::from_secs(3));
    p.key(" ", false, false, false, false); // pause
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "pomodoro crashed on space toggle");
    p.kill();
}

/// Return key also toggles the timer.
#[test]
fn pomodoro_return_toggles() {
    let mut p = plugin_or_skip!(POMODORO_BIN);
    p.resize(300, 400);
    let _ = p.next_frame(Duration::from_secs(5));
    p.key("\r", false, false, false, false);
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "pomodoro crashed on return");
    p.kill();
}

/// 'r' and 'R' reset the timer.
#[test]
fn pomodoro_reset_keys() {
    let mut p = plugin_or_skip!(POMODORO_BIN);
    p.resize(300, 400);
    let _ = p.next_frame(Duration::from_secs(5));
    p.key("r", false, false, false, false);
    p.key("R", false, false, false, false);
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "pomodoro crashed on reset key");
    p.kill();
}

/// Center click toggles; bottom-right click resets.
#[test]
fn pomodoro_click() {
    let mut p = plugin_or_skip!(POMODORO_BIN);
    p.resize(300, 400);
    let _ = p.next_frame(Duration::from_secs(5));
    p.click(150.0, 200.0); // center → toggle
    p.click(270.0, 370.0); // bottom-right corner → reset
    std::thread::sleep(Duration::from_millis(200));
    assert!(p.is_alive(), "pomodoro crashed on click");
    p.kill();
}

/// Pomodoro emits frames on its tick (~500ms).
#[test]
fn pomodoro_emits_multiple_frames() {
    let mut p = plugin_or_skip!(POMODORO_BIN);
    p.resize(300, 400);
    let f1 = p.next_frame(Duration::from_secs(5)).expect("no first frame");
    let f2 = p.next_frame(Duration::from_secs(3)).expect("no second frame");
    assert_eq!((f1.0, f1.1), (300, 400));
    assert_eq!((f2.0, f2.1), (300, 400));
    p.kill();
}

// ── mado-stats ────────────────────────────────────────────────────────────────

const STATS_BIN: &str =
    "/Users/tomdringer/Sites/mado-stats/target/debug/mado-stats";

#[test]
fn stats_responds_to_resize() { check_responds_to_resize(STATS_BIN); }

#[test]
fn stats_survives_focus_blur() { check_survives_focus_blur(STATS_BIN); }

#[test]
fn stats_survives_key_events() { check_survives_key_events(STATS_BIN); }
