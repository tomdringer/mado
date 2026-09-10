// ── Key classification ────────────────────────────────────────────────────────
//
// Pure, UI-free functions that translate a raw Slint key event into a
// `KeyAction`. No Rc, no RefCell, no side effects — fully unit-testable.
//
// The `on_pane_key_input` closure in main.rs calls `classify_key`, matches
// on the result, and performs the actual side effects (write to PTY, spawn
// clipboard thread, call do_zoom, etc.).

/// What the key handler should do in response to a key event.
#[derive(Debug, PartialEq)]
pub enum KeyAction {
    /// Cmd+C or Ctrl+C with an active selection: copy text, clear selection.
    CopySelection,
    /// Cmd+C with no active selection: swallow silently.
    Nothing,
    /// Cmd+V or Ctrl+V: start async clipboard paste.
    Paste,
    /// Cmd/Ctrl + = or +: increase font size by one step.
    ZoomIn,
    /// Cmd/Ctrl + -: decrease font size by one step.
    ZoomOut,
    /// Cmd/Ctrl + 0: reset font size to default.
    ZoomReset,
    /// Arrow key held with Alt and/or Ctrl+Shift: pre-built xterm modifier sequence.
    ArrowSeq(Vec<u8>),
    /// Shift-only arrow: drive Mado's own selection. `seq` is forwarded to the
    /// PTY instead when the pane is in alt-screen mode (nvim/helix handle it).
    ShiftArrow { dcol: i8, drow: i8, seq: Vec<u8> },
    /// Cmd+Shift+Left/Right: extend Mado selection to the start/end of the line.
    SelectToLineEdge { to_end: bool },
    /// Bare modifier key (Shift, Ctrl, Alt, Meta, CapsLock): swallow silently.
    /// These alias C0 control bytes and must never reach the terminal.
    ModifierOnly,
    /// Forward these bytes to the active PTY unchanged.
    Forward(Vec<u8>),
}

/// Classify a Slint key event into a `KeyAction`.
///
/// `has_selection` — whether the active pane currently has a non-empty
/// selection; needed to distinguish Cmd+C (copy) from Cmd+C (do nothing).
pub fn classify_key(
    text: &str,
    ctrl: bool,
    meta: bool,
    alt: bool,
    shift: bool,
    has_selection: bool,
) -> KeyAction {
    let zoom_mod = ctrl || meta;

    // ── Copy ──────────────────────────────────────────────────────────────────
    if (meta || ctrl) && text == "c" {
        if has_selection {
            return KeyAction::CopySelection;
        }
        if meta {
            return KeyAction::Nothing; // Cmd+C, no selection → swallow
        }
        // Ctrl+C, no selection → fall through so ^C reaches the terminal
    }

    // ── Paste ─────────────────────────────────────────────────────────────────
    if (meta || ctrl) && text == "v" {
        return KeyAction::Paste;
    }

    // ── Zoom ──────────────────────────────────────────────────────────────────
    if zoom_mod {
        match text {
            "=" | "+" => return KeyAction::ZoomIn,
            "-"       => return KeyAction::ZoomOut,
            "0"       => return KeyAction::ZoomReset,
            _ => {}
        }
    }

    // ── Modified arrow / navigation keys ──────────────────────────────────────
    //
    //  Shift-only      → ShiftArrow: Mado drives its own selection (forwarded
    //                    as ESC[1;2X to the PTY only in alt-screen mode).
    //  Ctrl-only       → line navigation: Ctrl+A (left/Home) or Ctrl+E (right/End).
    //                    Up/Down keep their xterm modifier sequence (N=5).
    //  Alt-only        → N=3  word movement (Option+Arrow macOS convention)
    //  Shift+Ctrl/Alt  → N=6/4 xterm modifier (forwarded to PTY / apps)
    let arrow_byte: Option<u8> = match text {
        "\u{F700}" => Some(b'A'), // Up
        "\u{F701}" => Some(b'B'), // Down
        "\u{F702}" => Some(b'D'), // Left
        "\u{F703}" => Some(b'C'), // Right
        _ => None,
    };
    if let Some(dir) = arrow_byte {
        // Shift-only: Mado-level selection (not PTY, unless alt screen).
        if shift && !ctrl && !alt {
            let (dcol, drow): (i8, i8) = match dir {
                b'A' => (0, -1),
                b'B' => (0,  1),
                b'C' => (1,  0),
                b'D' => (-1, 0),
                _    => (0,  0),
            };
            let seq = format!("\x1b[1;2{}", char::from(dir)).into_bytes();
            return KeyAction::ShiftArrow { dcol, drow, seq };
        }

        // Ctrl-only (no shift/alt): line navigation via universal Ctrl+A/E bytes.
        if ctrl && !alt && !shift {
            return match dir {
                b'D' => KeyAction::Forward(vec![0x01]), // Ctrl+A — beginning of line
                b'C' => KeyAction::Forward(vec![0x05]), // Ctrl+E — end of line
                _ => {
                    // Up/Down: forward as xterm N=5 sequence
                    let seq = format!("\x1b[1;5{}", char::from(dir)).into_bytes();
                    KeyAction::ArrowSeq(seq)
                }
            };
        }

        // Cmd+Shift+Left/Right (ctrl+shift, no alt): select to line edge.
        if ctrl && shift && !alt {
            match dir {
                b'D' => return KeyAction::SelectToLineEdge { to_end: false },
                b'C' => return KeyAction::SelectToLineEdge { to_end: true },
                _ => {}
            }
        }

        // All other modifier combos → xterm N = 1 + shift(1) + alt(2) + ctrl(4)
        if ctrl || alt || shift {
            let n: u32 = 1
                + if shift { 1 } else { 0 }
                + if alt   { 2 } else { 0 }
                + if ctrl  { 4 } else { 0 };
            let seq = format!("\x1b[1;{}{}", n, char::from(dir)).into_bytes();
            return KeyAction::ArrowSeq(seq);
        }
    }

    // Modified Home / End / PageUp / PageDown
    if ctrl || shift {
        let n: u32 = 1
            + if shift { 1 } else { 0 }
            + if ctrl  { 4 } else { 0 };
        let nav_seq: Option<Vec<u8>> = match text {
            "\u{F729}" => Some(format!("\x1b[1;{}H", n).into_bytes()), // Home
            "\u{F72B}" => Some(format!("\x1b[1;{}F", n).into_bytes()), // End
            "\u{F72C}" => Some(format!("\x1b[5;{}~", n).into_bytes()), // PageUp
            "\u{F72D}" => Some(format!("\x1b[6;{}~", n).into_bytes()), // PageDown
            _ => None,
        };
        if let Some(seq) = nav_seq {
            return KeyAction::ArrowSeq(seq);
        }
    }

    // ── Bare modifier key ─────────────────────────────────────────────────────
    if is_modifier_only(text) {
        return KeyAction::ModifierOnly;
    }

    // ── Forward raw bytes ─────────────────────────────────────────────────────
    KeyAction::Forward(key_text_to_bytes(text))
}

/// Accumulate a scroll delta and return how many whole rows to scroll.
///
/// `acc`        — running sub-row accumulator (modified in place)
/// `delta_px`   — raw logical-pixel delta from Slint's scroll-event
/// `scroll_dir` — `1.0` for natural scrolling, `-1.0` for traditional/inverted
/// `cell_h`     — logical-pixel height of one terminal row
///
/// Returns the number of rows to pass to `registry.scroll()`:
/// positive = toward older content (scroll up), negative = toward newer (scroll down).
#[allow(dead_code)]
pub fn accumulate_scroll(acc: &mut f32, delta_px: f32, scroll_dir: f32, cell_h: f32) -> i32 {
    if cell_h <= 0.0 { return 0; }
    *acc += -delta_px * scroll_dir;
    let rows = (*acc / cell_h) as i32;
    if rows != 0 {
        *acc -= rows as f32 * cell_h;
    }
    rows
}

/// Returns `true` when the active selection should be cleared before
/// processing this key event.
///
/// Selection is preserved while a modifier chord is in progress (the user
/// may be about to press Cmd+C) and for bare modifier keypresses.
pub fn should_clear_selection(text: &str, ctrl: bool, meta: bool, shift: bool) -> bool {
    !is_modifier_only(text) && !ctrl && !meta && !shift
}

/// Returns `true` for Slint's virtual modifier-key codepoints.
/// These live in the C0 range and alias terminal control sequences, so they
/// must never be forwarded to the PTY.
pub fn is_modifier_only(text: &str) -> bool {
    matches!(text,
        "\u{0010}" | "\u{0015}" | // Shift L/R
        "\u{0011}" | "\u{0016}" | // Control L/R
        "\u{0012}" | "\u{0013}" | // Alt / AltGr
        "\u{0014}" |              // CapsLock
        "\u{0017}" | "\u{0018}" | // Meta L/R
        "\u{0019}"               // Backtab
    )
}

/// Translate a Slint key text string to the byte sequence a terminal expects.
///
/// Modifier-key codepoints produce an empty vec (they are never forwarded).
/// Navigation and function keys produce standard xterm/VT escape sequences.
/// Everything else passes through as raw UTF-8 bytes — this covers printable
/// characters, Ctrl+letter (U+0001–U+001A), Return, Tab, and Escape.
pub fn key_text_to_bytes(text: &str) -> Vec<u8> {
    match text {
        // ── Modifier keys — never forward to terminal ─────────────────────────
        "\u{0010}" | // Shift (L)   — would send Ctrl+P
        "\u{0015}" | // Shift (R)   — would send Ctrl+U (kill line!)
        "\u{0011}" | // Control (L) — would send Ctrl+Q
        "\u{0016}" | // Control (R) — would send Ctrl+V
        "\u{0012}" | // Alt         — would send Ctrl+R
        "\u{0013}" | // AltGr       — would send Ctrl+S
        "\u{0014}" | // CapsLock    — would send Ctrl+T
        "\u{0017}" | // Meta (L)    — would send Ctrl+W
        "\u{0018}" | // Meta (R)    — would send Ctrl+X
        "\u{0019}"   // Backtab
        => vec![],

        // ── Standard keys ────────────────────────────────────────────────────
        "\u{0008}" => vec![0x7F],                              // Backspace → DEL
        "\u{007F}" => vec![0x1B, b'[', b'3', b'~'],           // Delete → ESC[3~

        // ── Arrow keys ───────────────────────────────────────────────────────
        "\u{F700}" => vec![0x1B, b'[', b'A'],                 // Up
        "\u{F701}" => vec![0x1B, b'[', b'B'],                 // Down
        "\u{F702}" => vec![0x1B, b'[', b'D'],                 // Left
        "\u{F703}" => vec![0x1B, b'[', b'C'],                 // Right

        // ── Navigation ───────────────────────────────────────────────────────
        "\u{F729}" => vec![0x1B, b'[', b'H'],                 // Home
        "\u{F72B}" => vec![0x1B, b'[', b'F'],                 // End
        "\u{F72C}" => vec![0x1B, b'[', b'5', b'~'],           // PageUp
        "\u{F72D}" => vec![0x1B, b'[', b'6', b'~'],           // PageDown

        // ── Function keys ────────────────────────────────────────────────────
        "\u{F704}" => vec![0x1B, b'O', b'P'],                 // F1
        "\u{F705}" => vec![0x1B, b'O', b'Q'],                 // F2
        "\u{F706}" => vec![0x1B, b'O', b'R'],                 // F3
        "\u{F707}" => vec![0x1B, b'O', b'S'],                 // F4
        "\u{F708}" => vec![0x1B, b'[', b'1', b'5', b'~'],    // F5
        "\u{F709}" => vec![0x1B, b'[', b'1', b'7', b'~'],    // F6
        "\u{F70A}" => vec![0x1B, b'[', b'1', b'8', b'~'],    // F7
        "\u{F70B}" => vec![0x1B, b'[', b'1', b'9', b'~'],    // F8
        "\u{F70C}" => vec![0x1B, b'[', b'2', b'0', b'~'],    // F9
        "\u{F70D}" => vec![0x1B, b'[', b'2', b'1', b'~'],    // F10
        "\u{F70E}" => vec![0x1B, b'[', b'2', b'3', b'~'],    // F11
        "\u{F70F}" => vec![0x1B, b'[', b'2', b'4', b'~'],    // F12

        // ── Enter / Return ────────────────────────────────────────────────────
        // Always send CR (0x0D) regardless of whether Slint delivers \r or \n.
        // TUI apps (Claude CLI, etc.) use CR as "submit" and LF as "new line".
        "\r" | "\n" => vec![0x0D],

        // ── Everything else ───────────────────────────────────────────────────
        // Tab (\t), Escape (\u{1B}), Ctrl+letter (\u{0001}–\u{001A})
        // all pass through as raw bytes — correct terminal values.
        _ => text.as_bytes().to_vec(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> KeyAction {
        classify_key(text, false, false, false, false, false)
    }
    fn ctrl(text: &str) -> KeyAction {
        classify_key(text, true, false, false, false, false)
    }
    fn meta(text: &str) -> KeyAction {
        classify_key(text, false, true, false, false, false)
    }
    fn meta_sel(text: &str) -> KeyAction {
        classify_key(text, false, true, false, false, true)
    }
    fn ctrl_sel(text: &str) -> KeyAction {
        classify_key(text, true, false, false, false, true)
    }

    // ── Copy ──────────────────────────────────────────────────────────────────

    #[test]
    fn cmd_c_no_selection_is_nothing() {
        assert_eq!(meta("c"), KeyAction::Nothing);
    }

    #[test]
    fn cmd_c_with_selection_copies() {
        assert_eq!(meta_sel("c"), KeyAction::CopySelection);
    }

    #[test]
    fn ctrl_c_no_selection_forwards_ctrl_c_bytes() {
        // Falls through to Forward so the terminal gets ^C
        assert_eq!(ctrl("c"), KeyAction::Forward(vec![b'c']));
    }

    #[test]
    fn ctrl_c_with_selection_copies() {
        assert_eq!(ctrl_sel("c"), KeyAction::CopySelection);
    }

    // ── Paste ─────────────────────────────────────────────────────────────────

    #[test]
    fn cmd_v_is_paste() {
        assert_eq!(meta("v"), KeyAction::Paste);
    }

    #[test]
    fn ctrl_v_is_paste() {
        assert_eq!(ctrl("v"), KeyAction::Paste);
    }

    // ── Zoom ──────────────────────────────────────────────────────────────────

    #[test]
    fn cmd_equals_zooms_in() {
        assert_eq!(classify_key("=", false, true, false, false, false), KeyAction::ZoomIn);
    }

    #[test]
    fn cmd_plus_zooms_in() {
        assert_eq!(classify_key("+", false, true, false, false, false), KeyAction::ZoomIn);
    }

    #[test]
    fn cmd_minus_zooms_out() {
        assert_eq!(classify_key("-", false, true, false, false, false), KeyAction::ZoomOut);
    }

    #[test]
    fn cmd_zero_zooms_reset() {
        assert_eq!(classify_key("0", false, true, false, false, false), KeyAction::ZoomReset);
    }

    #[test]
    fn ctrl_equals_zooms_in() {
        assert_eq!(classify_key("=", true, false, false, false, false), KeyAction::ZoomIn);
    }

    #[test]
    fn minus_without_modifier_forwards() {
        assert_eq!(key("-"), KeyAction::Forward(vec![b'-']));
    }

    // ── Arrow sequences ───────────────────────────────────────────────────────

    #[test]
    fn opt_left_produces_word_movement_seq() {
        // Alt+Left → N=3 → ESC[1;3D
        let action = classify_key("\u{F702}", false, false, true, false, false);
        assert_eq!(action, KeyAction::ArrowSeq(b"\x1b[1;3D".to_vec()));
    }

    #[test]
    fn opt_right_produces_word_movement_seq() {
        let action = classify_key("\u{F703}", false, false, true, false, false);
        assert_eq!(action, KeyAction::ArrowSeq(b"\x1b[1;3C".to_vec()));
    }

    #[test]
    fn shift_left_produces_char_selection_seq() {
        // Shift+Left → ShiftArrow (Mado handles selection) with the standard
        // xterm N=2 escape sequence embedded so it can be forwarded to PTY
        // apps that understand it (e.g. neovim in alt-screen mode).
        let action = classify_key("\u{F702}", false, false, false, true, false);
        assert_eq!(action, KeyAction::ShiftArrow {
            dcol: -1,
            drow:  0,
            seq:   b"\x1b[1;2D".to_vec(),
        });
    }

    #[test]
    fn shift_opt_left_produces_word_selection_seq() {
        // Shift+Alt+Left → N=4 → ESC[1;4D
        let action = classify_key("\u{F702}", false, false, true, true, false);
        assert_eq!(action, KeyAction::ArrowSeq(b"\x1b[1;4D".to_vec()));
    }

    #[test]
    fn unmodified_arrow_forwards_plain_escape_seq() {
        // No alt/shift → plain arrow, falls through to Forward via key_text_to_bytes
        let action = classify_key("\u{F702}", false, false, false, false, false);
        assert_eq!(action, KeyAction::Forward(vec![0x1B, b'[', b'D']));
    }

    #[test]
    fn all_four_arrow_directions_with_alt() {
        let cases = [
            ("\u{F700}", b"\x1b[1;3A".as_ref()), // Up
            ("\u{F701}", b"\x1b[1;3B".as_ref()), // Down
            ("\u{F702}", b"\x1b[1;3D".as_ref()), // Left
            ("\u{F703}", b"\x1b[1;3C".as_ref()), // Right
        ];
        for (text, expected) in cases {
            let action = classify_key(text, false, false, true, false, false);
            assert_eq!(action, KeyAction::ArrowSeq(expected.to_vec()), "failed for arrow {text:?}");
        }
    }

    // ── Modifier-only ─────────────────────────────────────────────────────────

    #[test]
    fn shift_key_alone_is_modifier_only() {
        assert_eq!(key("\u{0010}"), KeyAction::ModifierOnly);
        assert_eq!(key("\u{0015}"), KeyAction::ModifierOnly);
    }

    #[test]
    fn ctrl_key_alone_is_modifier_only() {
        assert_eq!(key("\u{0011}"), KeyAction::ModifierOnly);
    }

    #[test]
    fn meta_key_alone_is_modifier_only() {
        assert_eq!(key("\u{0017}"), KeyAction::ModifierOnly);
    }

    #[test]
    fn capslock_is_modifier_only() {
        assert_eq!(key("\u{0014}"), KeyAction::ModifierOnly);
    }

    // ── Forward ───────────────────────────────────────────────────────────────

    #[test]
    fn regular_char_forwards() {
        assert_eq!(key("a"), KeyAction::Forward(vec![b'a']));
    }

    #[test]
    fn return_forwards_cr() {
        assert_eq!(key("\r"), KeyAction::Forward(vec![b'\r']));
    }

    #[test]
    fn tab_forwards() {
        assert_eq!(key("\t"), KeyAction::Forward(vec![b'\t']));
    }

    #[test]
    fn escape_forwards() {
        assert_eq!(key("\u{1B}"), KeyAction::Forward(vec![0x1B]));
    }

    // ── should_clear_selection ────────────────────────────────────────────────

    #[test]
    fn regular_key_clears_selection() {
        assert!(should_clear_selection("a", false, false, false));
    }

    #[test]
    fn ctrl_held_does_not_clear() {
        assert!(!should_clear_selection("c", true, false, false));
    }

    #[test]
    fn meta_held_does_not_clear() {
        assert!(!should_clear_selection("c", false, true, false));
    }

    #[test]
    fn shift_held_does_not_clear() {
        assert!(!should_clear_selection("a", false, false, true));
    }

    #[test]
    fn modifier_only_does_not_clear() {
        assert!(!should_clear_selection("\u{0010}", false, false, false));
    }

    // ── key_text_to_bytes ─────────────────────────────────────────────────────

    #[test]
    fn backspace_maps_to_del() {
        assert_eq!(key_text_to_bytes("\u{0008}"), vec![0x7F]);
    }

    #[test]
    fn delete_maps_to_escape_seq() {
        assert_eq!(key_text_to_bytes("\u{007F}"), vec![0x1B, b'[', b'3', b'~']);
    }

    #[test]
    fn f1_maps_to_ss3_p() {
        assert_eq!(key_text_to_bytes("\u{F704}"), vec![0x1B, b'O', b'P']);
    }

    #[test]
    fn f5_maps_to_escape_seq() {
        assert_eq!(key_text_to_bytes("\u{F708}"), vec![0x1B, b'[', b'1', b'5', b'~']);
    }

    #[test]
    fn home_maps_to_escape_seq() {
        assert_eq!(key_text_to_bytes("\u{F729}"), vec![0x1B, b'[', b'H']);
    }

    #[test]
    fn modifier_key_codepoint_maps_to_empty() {
        assert!(key_text_to_bytes("\u{0010}").is_empty()); // Shift
        assert!(key_text_to_bytes("\u{0017}").is_empty()); // Meta
    }

    #[test]
    fn printable_ascii_passes_through() {
        for ch in 'a'..='z' {
            assert_eq!(key_text_to_bytes(&ch.to_string()), vec![ch as u8]);
        }
    }

    // ── accumulate_scroll ─────────────────────────────────────────────────────
    //
    // Convention used throughout Mado:
    //   delta_px from Slint is positive when scrolling DOWN (natural direction).
    //   scroll_dir = 1.0  → natural scrolling (macOS "natural" ON)
    //   scroll_dir = -1.0 → traditional/inverted scrolling (macOS "natural" OFF)
    //   Positive return value → scroll toward older content (up into scrollback).
    //   Handlers pass `delta_px` for sidebar/plugin, `delta` for pane — same sign.

    fn acc_natural(acc: &mut f32, delta: f32, cell_h: f32) -> i32 {
        accumulate_scroll(acc, delta, 1.0, cell_h)
    }
    fn acc_traditional(acc: &mut f32, delta: f32, cell_h: f32) -> i32 {
        accumulate_scroll(acc, delta, -1.0, cell_h)
    }

    #[test]
    fn no_scroll_below_one_row() {
        let mut acc = 0.0f32;
        // Half a row of scroll → not enough to fire
        assert_eq!(acc_natural(&mut acc, 9.0, 20.0), 0);
        assert!(acc.abs() > 0.0); // accumulator holds the remainder
    }

    #[test]
    fn exact_one_row_fires_once() {
        let mut acc = 0.0f32;
        assert_eq!(acc_natural(&mut acc, 20.0, 20.0), -1);
        assert!((acc).abs() < 1e-4);
    }

    #[test]
    fn two_rows_fires_two() {
        let mut acc = 0.0f32;
        assert_eq!(acc_natural(&mut acc, 40.0, 20.0), -2);
    }

    #[test]
    fn remainder_carries_over() {
        let mut acc = 0.0f32;
        acc_natural(&mut acc, 15.0, 20.0); // 15px left in acc
        let rows = acc_natural(&mut acc, 15.0, 20.0); // 30px total → 1 row
        assert_eq!(rows, -1);
        assert!((acc + 10.0).abs() < 1e-3); // 10px remainder (negative = toward down)
    }

    #[test]
    fn scroll_down_natural_is_negative_rows() {
        // Scroll DOWN (positive delta) → negative rows (toward newer content)
        let mut acc = 0.0f32;
        assert_eq!(acc_natural(&mut acc, 20.0, 20.0), -1);
    }

    #[test]
    fn scroll_up_natural_is_positive_rows() {
        // Scroll UP (negative delta) → positive rows (into scrollback)
        let mut acc = 0.0f32;
        assert_eq!(acc_natural(&mut acc, -20.0, 20.0), 1);
    }

    #[test]
    fn traditional_scrolling_flips_direction() {
        // Same physical gesture (positive delta) → opposite sign vs natural
        let mut acc_nat = 0.0f32;
        let mut acc_trad = 0.0f32;
        let rows_nat  = acc_natural(&mut acc_nat, 20.0, 20.0);
        let rows_trad = acc_traditional(&mut acc_trad, 20.0, 20.0);
        assert_eq!(rows_nat, -rows_trad);
    }

    #[test]
    fn zero_cell_height_returns_zero() {
        let mut acc = 0.0f32;
        assert_eq!(accumulate_scroll(&mut acc, 100.0, 1.0, 0.0), 0);
    }

    #[test]
    fn negative_cell_height_returns_zero() {
        let mut acc = 0.0f32;
        assert_eq!(accumulate_scroll(&mut acc, 100.0, 1.0, -5.0), 0);
    }

    #[test]
    fn large_momentum_delta_capped_to_exact_rows() {
        // A big trackpad swipe (500px) at 20px/row = 25 rows, no partial remainder
        let mut acc = 0.0f32;
        assert_eq!(acc_natural(&mut acc, -500.0, 20.0), 25);
        assert!(acc.abs() < 1e-3);
    }

    #[test]
    fn accumulator_resets_direction_correctly() {
        // Scroll down then up in the same accumulator
        let mut acc = 0.0f32;
        acc_natural(&mut acc, 15.0, 20.0); // +15 toward down
        let rows = acc_natural(&mut acc, -35.0, 20.0); // -35 → net -20 → 1 row up
        assert_eq!(rows, 1);
    }
}
