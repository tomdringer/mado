//! mado-webview — off-screen WebKitGTK browser for Mado's Linux browser panel.
//!
//! On macOS Mado embeds a WKWebView directly in its window. Wayland does not
//! allow embedding a foreign toolkit's widget, so on Linux the page is rendered
//! in a hidden GtkOffscreenWindow and streamed to Mado as pixel frames using the
//! pixel-plugin protocol (see src/pixel_plugin.rs). Mado forwards pointer,
//! scroll and key events, which are replayed into WebKit as synthetic GDK
//! events so the page sees real input (focus, text entry, selection, etc.).
//!
//! Usage: mado-webview [initial-url]
//!   MADO_SCALE  display scale factor; used as the page zoom level.
//!
//! Stdin (newline-delimited JSON, coordinates in physical pixels):
//!   {"type":"resize","width":800,"height":600}
//!   {"type":"navigate","url":"https://…"}      {"type":"back"}
//!   {"type":"visible","visible":true}          {"type":"focus"} / {"type":"blur"}
//!   {"type":"scale","scale":2.0}
//!   {"type":"mouse_press","x":1.0,"y":2.0}     {"type":"mouse_move",…}
//!   {"type":"mouse_release"}                   {"type":"scroll","delta":-3.0}
//!   {"type":"key","text":"a","ctrl":false,"meta":false,"alt":false,"shift":false}

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mado-webview is only used on Linux");
    std::process::exit(1);
}

// ── Pure helpers (platform-independent, unit-tested) ─────────────────────────

#[allow(dead_code)]
mod protocol {
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum Event {
        Resize { width: u32, height: u32 },
        Navigate { url: String },
        Back,
        Visible { visible: bool },
        Scale { scale: f64 },
        Focus,
        Blur,
        MousePress { x: f64, y: f64 },
        MouseMove { x: f64, y: f64 },
        MouseRelease,
        Scroll { delta: f64 },
        Key {
            text: String,
            #[serde(default)] ctrl: bool,
            #[serde(default)] meta: bool,
            #[serde(default)] alt: bool,
            #[serde(default)] shift: bool,
        },
    }

    /// Parse one stdin line. Unknown or malformed messages are ignored.
    pub fn parse_event(line: &str) -> Option<Event> {
        serde_json::from_str(line.trim()).ok()
    }

    /// Build a MADO pixel frame: magic, little-endian width/height, RGBA bytes.
    pub fn encode_frame(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + rgba.len());
        out.extend_from_slice(b"MADO");
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(rgba);
        out
    }

    /// Copy `height` rows of `width` RGBA pixels out of a buffer whose rows are
    /// `rowstride` bytes apart (GdkPixbuf pads rows).
    pub fn pack_rows(pixels: &[u8], width: usize, height: usize, rowstride: usize) -> Vec<u8> {
        let row_len = width * 4;
        let mut out = Vec::with_capacity(row_len * height);
        for row in 0..height {
            let start = row * rowstride;
            out.extend_from_slice(&pixels[start..start + row_len]);
        }
        out
    }

    /// WebKitGTK scrolls one line step (40 CSS px) per unit of smooth-scroll
    /// delta. Mado sends physical pixels, positive = content moves down (scroll
    /// up); GDK's smooth delta is positive for scrolling down.
    pub fn scroll_delta_to_gdk(delta_phys: f64, scale: f64) -> f64 {
        const PIXELS_PER_LINE_STEP: f64 = 40.0;
        -delta_phys / (PIXELS_PER_LINE_STEP * scale.max(1.0))
    }

    // GDK keysym values (gdk/gdkkeysyms.h).
    pub const KEY_BACKSPACE: u32 = 0xff08;
    pub const KEY_TAB: u32 = 0xff09;
    pub const KEY_RETURN: u32 = 0xff0d;
    pub const KEY_ESCAPE: u32 = 0xff1b;
    pub const KEY_HOME: u32 = 0xff50;
    pub const KEY_LEFT: u32 = 0xff51;
    pub const KEY_UP: u32 = 0xff52;
    pub const KEY_RIGHT: u32 = 0xff53;
    pub const KEY_DOWN: u32 = 0xff54;
    pub const KEY_PAGE_UP: u32 = 0xff55;
    pub const KEY_PAGE_DOWN: u32 = 0xff56;
    pub const KEY_END: u32 = 0xff57;
    pub const KEY_F1: u32 = 0xffbe;
    pub const KEY_DELETE: u32 = 0xffff;
    pub const KEY_ISO_LEFT_TAB: u32 = 0xfe20;

    /// Map Slint key text (see src/keys.rs for the code points) to a GDK keyval.
    /// Returns None for modifier-only presses and empty text.
    pub fn slint_key_to_keyval(text: &str) -> Option<u32> {
        let mut chars = text.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let keyval = match c {
            '\n' | '\r' => KEY_RETURN,
            '\t' => KEY_TAB,
            '\u{1b}' => KEY_ESCAPE,
            '\u{08}' => KEY_BACKSPACE,
            '\u{7f}' => KEY_DELETE,
            '\u{19}' => KEY_ISO_LEFT_TAB, // Backtab
            '\u{10}'..='\u{18}' => return None, // Shift/Control/Alt/Meta/CapsLock
            '\u{F700}' => KEY_UP,
            '\u{F701}' => KEY_DOWN,
            '\u{F702}' => KEY_LEFT,
            '\u{F703}' => KEY_RIGHT,
            '\u{F729}' => KEY_HOME,
            '\u{F72B}' => KEY_END,
            '\u{F72C}' => KEY_PAGE_UP,
            '\u{F72D}' => KEY_PAGE_DOWN,
            '\u{F704}'..='\u{F70F}' => KEY_F1 + (c as u32 - 0xF704),
            c if (c as u32) < 0x20 => return None,
            // gdk_unicode_to_keyval: Latin-1 maps directly, the rest are offset.
            c if (0x20..=0x7e).contains(&(c as u32)) || (0xa0..=0xff).contains(&(c as u32)) => c as u32,
            c => 0x0100_0000 | c as u32,
        };
        Some(keyval)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::{Cell, RefCell};
    use std::io::{BufRead, Write};
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::Duration;

    use glib::translate::ToGlibPtr;
    use gtk::prelude::*;
    use webkit2gtk::{
        HardwareAccelerationPolicy, NavigationPolicyDecisionExt, PolicyDecisionExt,
        PolicyDecisionType, SettingsExt, URIRequestExt, UserContentInjectedFrames,
        UserContentManagerExt, UserScript, UserScriptInjectionTime, WebViewExt,
    };

    use super::protocol::{self, Event};

    const NAV_BAR_JS: &str = include_str!("../browser_nav_bar.js");
    const DEFAULT_URL: &str = "https://www.google.com";

    // GdkModifierType bits.
    const SHIFT_MASK: u32 = 1 << 0;
    const CONTROL_MASK: u32 = 1 << 2;
    const MOD1_MASK: u32 = 1 << 3; // Alt
    const BUTTON1_MASK: u32 = 1 << 8;
    const SUPER_MASK: u32 = 1 << 26;

    /// Latest frame waiting to be written. Frames that arrive while the writer
    /// is busy replace the pending one, so a slow reader never builds a backlog.
    type FrameSlot = Arc<(Mutex<Option<Vec<u8>>>, Condvar)>;

    struct Pointer {
        x: Cell<f64>,
        y: Cell<f64>,
        pressed: Cell<bool>,
    }

    pub fn run() {
        if gtk::init().is_err() {
            eprintln!("mado-webview: could not initialise GTK (no display?)");
            std::process::exit(1);
        }

        let scale: f64 = std::env::var("MADO_SCALE").ok()
            .and_then(|s| s.parse().ok())
            .filter(|s: &f64| *s > 0.0)
            .unwrap_or(1.0);
        let scale = Rc::new(Cell::new(scale));
        let initial_url = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_URL.to_string());

        // ── WebView ───────────────────────────────────────────────────────────
        let ucm = webkit2gtk::UserContentManager::new();
        ucm.add_script(&UserScript::new(
            NAV_BAR_JS,
            UserContentInjectedFrames::TopFrame,
            UserScriptInjectionTime::End,
            &[],
            &[],
        ));
        let web = webkit2gtk::WebView::with_user_content_manager(&ucm);
        if let Some(settings) = WebViewExt::settings(&web) {
            // Offscreen windows can only be captured from the software renderer.
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
            settings.set_enable_developer_extras(true);
        }
        web.set_zoom_level(scale.get());

        // Links that ask for a new window (target=_blank) open in place.
        web.connect_decide_policy(|web, decision, kind| {
            if kind != PolicyDecisionType::NewWindowAction {
                return false;
            }
            let uri = decision.clone()
                .downcast::<webkit2gtk::NavigationPolicyDecision>().ok()
                .and_then(|d| d.navigation_action())
                .and_then(|a| a.request())
                .and_then(|r| r.uri());
            if let Some(uri) = uri {
                web.load_uri(&uri);
            }
            decision.ignore();
            true
        });

        let win = gtk::OffscreenWindow::new();
        win.add(&web);
        resize(&win, &web, 800, 600);
        win.show_all();

        let damaged = Rc::new(Cell::new(true));
        {
            let damaged = Rc::clone(&damaged);
            win.connect_damage_event(move |_, _| {
                damaged.set(true);
                false
            });
        }

        web.load_uri(&initial_url);

        // ── Frame writer thread ───────────────────────────────────────────────
        let slot: FrameSlot = Arc::new((Mutex::new(None), Condvar::new()));
        {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || {
                let mut out = std::io::stdout().lock();
                loop {
                    let frame = {
                        let (lock, cvar) = &*slot;
                        let mut g = lock.lock().unwrap();
                        while g.is_none() {
                            g = cvar.wait(g).unwrap();
                        }
                        g.take().unwrap()
                    };
                    if out.write_all(&frame).and_then(|_| out.flush()).is_err() {
                        std::process::exit(0); // Mado went away
                    }
                }
            });
        }

        // ── Stdin reader thread ───────────────────────────────────────────────
        let (tx, rx) = mpsc::channel::<Option<String>>();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) => { if tx.send(Some(l)).is_err() { return; } }
                    Err(_) => break,
                }
            }
            let _ = tx.send(None); // EOF: Mado exited
        });

        // ── Main tick: apply input, publish frames ────────────────────────────
        let visible = Rc::new(Cell::new(true));
        let active = Rc::new(Cell::new(false));
        let pointer = Rc::new(Pointer { x: Cell::new(0.0), y: Cell::new(0.0), pressed: Cell::new(false) });
        let size = Rc::new(RefCell::new((800u32, 600u32)));

        glib::timeout_add_local(Duration::from_millis(16), move || {
            while let Ok(msg) = rx.try_recv() {
                let Some(line) = msg else {
                    gtk::main_quit();
                    return glib::ControlFlow::Break;
                };
                let Some(event) = protocol::parse_event(&line) else { continue };
                handle_event(event, &win, &web, &pointer, &visible, &active, &size, &damaged, &scale);
            }

            if visible.get() && damaged.get() {
                damaged.set(false);
                if let Some(frame) = capture(&win, *size.borrow()) {
                    let (lock, cvar) = &*slot;
                    *lock.lock().unwrap() = Some(frame);
                    cvar.notify_one();
                }
            }
            glib::ControlFlow::Continue
        });

        gtk::main();
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        event: Event,
        win: &gtk::OffscreenWindow,
        web: &webkit2gtk::WebView,
        pointer: &Pointer,
        visible: &Cell<bool>,
        active: &Cell<bool>,
        size: &RefCell<(u32, u32)>,
        damaged: &Cell<bool>,
        scale: &Cell<f64>,
    ) {
        let Some(gdk_win) = web.window() else { return };
        match event {
            Event::Resize { width, height } => {
                let (w, h) = (width.max(1), height.max(1));
                *size.borrow_mut() = (w, h);
                resize(win, web, w as i32, h as i32);
                damaged.set(true);
            }
            Event::Navigate { url } => web.load_uri(&url),
            Event::Back => web.go_back(),
            Event::Scale { scale: s } if s > 0.0 => {
                scale.set(s);
                web.set_zoom_level(s);
                damaged.set(true);
            }
            Event::Scale { .. } => {}
            Event::Visible { visible: v } => {
                visible.set(v);
                damaged.set(true);
            }
            Event::Focus => set_active(win, web, active, true),
            Event::Blur => set_active(win, web, active, false),
            Event::MousePress { x, y } => {
                pointer.x.set(x);
                pointer.y.set(y);
                pointer.pressed.set(true);
                set_active(win, web, active, true);
                unsafe { send_button(&gdk_win, gdk_sys::GDK_BUTTON_PRESS, x, y, 0) };
            }
            Event::MouseMove { x, y } => {
                pointer.x.set(x);
                pointer.y.set(y);
                let state = if pointer.pressed.get() { BUTTON1_MASK } else { 0 };
                unsafe { send_motion(&gdk_win, x, y, state) };
            }
            Event::MouseRelease => {
                pointer.pressed.set(false);
                unsafe {
                    send_button(&gdk_win, gdk_sys::GDK_BUTTON_RELEASE,
                                pointer.x.get(), pointer.y.get(), BUTTON1_MASK)
                };
            }
            Event::Scroll { delta } => {
                let dy = protocol::scroll_delta_to_gdk(delta, scale.get());
                unsafe { send_scroll(&gdk_win, pointer.x.get(), pointer.y.get(), dy) };
            }
            Event::Key { text, ctrl, meta, alt, shift } => {
                let Some(keyval) = protocol::slint_key_to_keyval(&text) else { return };
                set_active(win, web, active, true);
                let mut state = 0;
                if shift { state |= SHIFT_MASK; }
                if ctrl { state |= CONTROL_MASK; }
                if alt { state |= MOD1_MASK; }
                if meta { state |= SUPER_MASK; }
                unsafe { send_key(&gdk_win, keyval, state) };
            }
        }
    }

    /// WebKit only draws the text caret and focus rings while its toplevel is
    /// the active window, which an offscreen window never becomes on its own.
    /// Activate on focus and on any click or key (in case the focus message was
    /// missed), and deactivate on blur.
    fn set_active(win: &gtk::OffscreenWindow, web: &webkit2gtk::WebView, active: &Cell<bool>, on: bool) {
        if on {
            web.grab_focus();
        }
        if active.get() == on {
            return;
        }
        active.set(on);
        if let Some(top) = win.window() {
            unsafe { send_focus(&top, on) };
        }
    }

    fn resize(win: &gtk::OffscreenWindow, web: &webkit2gtk::WebView, w: i32, h: i32) {
        web.set_size_request(w, h);
        win.resize(w, h);
    }

    /// Grab the offscreen window's contents as a MADO frame, skipping frames
    /// rendered at a stale size while a resize is in flight.
    fn capture(win: &gtk::OffscreenWindow, (want_w, want_h): (u32, u32)) -> Option<Vec<u8>> {
        let pb = win.pixbuf()?;
        let (w, h) = (pb.width() as u32, pb.height() as u32);
        if w != want_w || h != want_h || !pb.has_alpha() || pb.n_channels() != 4 {
            return None;
        }
        let bytes = pb.read_pixel_bytes();
        let rgba = protocol::pack_rows(&bytes, w as usize, h as usize, pb.rowstride() as usize);
        Some(protocol::encode_frame(w, h, &rgba))
    }

    // ── Synthetic GDK events ──────────────────────────────────────────────────
    //
    // gtk-rs does not expose writable event structs, so these fill the C
    // structs directly. Each event holds a reference to its window (released
    // by gdk_event_free) and carries the seat's device, which WebKit requires.

    fn raw(w: &gdk::Window) -> *mut gdk_sys::GdkWindow {
        w.to_glib_none().0
    }

    unsafe fn new_event(kind: gdk_sys::GdkEventType, w: &gdk::Window, keyboard: bool) -> *mut gdk_sys::GdkEvent {
        unsafe {
            let ev = gdk_sys::gdk_event_new(kind);
            let any = ev as *mut gdk_sys::GdkEventAny;
            (*any).window = gobject_sys::g_object_ref(raw(w) as *mut _) as *mut _;
            (*any).send_event = 1;
            let seat = gdk_sys::gdk_display_get_default_seat(gdk_sys::gdk_window_get_display(raw(w)));
            let device = if keyboard {
                gdk_sys::gdk_seat_get_keyboard(seat)
            } else {
                gdk_sys::gdk_seat_get_pointer(seat)
            };
            gdk_sys::gdk_event_set_device(ev, device);
            ev
        }
    }

    unsafe fn dispatch(ev: *mut gdk_sys::GdkEvent) {
        unsafe {
            gtk_sys::gtk_main_do_event(ev);
            gdk_sys::gdk_event_free(ev);
        }
    }

    fn now_ms() -> u32 {
        (glib::monotonic_time() / 1000) as u32
    }

    unsafe fn send_button(w: &gdk::Window, kind: gdk_sys::GdkEventType, x: f64, y: f64, state: u32) {
        unsafe {
            let ev = new_event(kind, w, false);
            let b = ev as *mut gdk_sys::GdkEventButton;
            (*b).time = now_ms();
            (*b).x = x;
            (*b).y = y;
            (*b).x_root = x;
            (*b).y_root = y;
            (*b).button = 1;
            (*b).state = state;
            dispatch(ev);
        }
    }

    unsafe fn send_motion(w: &gdk::Window, x: f64, y: f64, state: u32) {
        unsafe {
            let ev = new_event(gdk_sys::GDK_MOTION_NOTIFY, w, false);
            let m = ev as *mut gdk_sys::GdkEventMotion;
            (*m).time = now_ms();
            (*m).x = x;
            (*m).y = y;
            (*m).x_root = x;
            (*m).y_root = y;
            (*m).state = state;
            dispatch(ev);
        }
    }

    unsafe fn send_scroll(w: &gdk::Window, x: f64, y: f64, delta_y: f64) {
        unsafe {
            let ev = new_event(gdk_sys::GDK_SCROLL, w, false);
            let s = ev as *mut gdk_sys::GdkEventScroll;
            (*s).time = now_ms();
            (*s).x = x;
            (*s).y = y;
            (*s).x_root = x;
            (*s).y_root = y;
            (*s).direction = gdk_sys::GDK_SCROLL_SMOOTH;
            (*s).delta_x = 0.0;
            (*s).delta_y = delta_y;
            dispatch(ev);
        }
    }

    unsafe fn send_key(w: &gdk::Window, keyval: u32, state: u32) {
        unsafe {
            let keymap = gdk_sys::gdk_keymap_get_for_display(gdk_sys::gdk_window_get_display(raw(w)));
            let mut entries: *mut gdk_sys::GdkKeymapKey = std::ptr::null_mut();
            let mut n = 0;
            let mut keycode = 0u16;
            if gdk_sys::gdk_keymap_get_entries_for_keyval(keymap, keyval, &mut entries, &mut n) != 0 {
                if n > 0 {
                    keycode = (*entries).keycode as u16;
                }
                glib_sys::g_free(entries as *mut _);
            }
            for kind in [gdk_sys::GDK_KEY_PRESS, gdk_sys::GDK_KEY_RELEASE] {
                let ev = new_event(kind, w, true);
                let k = ev as *mut gdk_sys::GdkEventKey;
                (*k).time = now_ms();
                (*k).state = state;
                (*k).keyval = keyval;
                (*k).hardware_keycode = keycode;
                dispatch(ev);
            }
        }
    }

    unsafe fn send_focus(w: &gdk::Window, focus_in: bool) {
        unsafe {
            let ev = new_event(gdk_sys::GDK_FOCUS_CHANGE, w, true);
            (*(ev as *mut gdk_sys::GdkEventFocus)).in_ = focus_in as i16;
            dispatch(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::protocol::*;

    #[test]
    fn parses_input_events() {
        assert_eq!(parse_event(r#"{"type":"resize","width":800,"height":600}"#),
                   Some(Event::Resize { width: 800, height: 600 }));
        assert_eq!(parse_event(r#"{"type":"navigate","url":"https://a.b/"}"#),
                   Some(Event::Navigate { url: "https://a.b/".into() }));
        assert_eq!(parse_event(r#"{"type":"mouse_release"}"#), Some(Event::MouseRelease));
        assert_eq!(parse_event(r#"{"type":"key","text":"a","ctrl":true,"meta":false,"alt":false,"shift":false}"#),
                   Some(Event::Key { text: "a".into(), ctrl: true, meta: false, alt: false, shift: false }));
        assert_eq!(parse_event(r#"{"type":"visible","visible":false}"#), Some(Event::Visible { visible: false }));
        assert_eq!(parse_event(r#"{"type":"scale","scale":2.0}"#), Some(Event::Scale { scale: 2.0 }));
    }

    #[test]
    fn ignores_unknown_and_malformed_events() {
        assert_eq!(parse_event(r#"{"type":"chdir","path":"/tmp"}"#), None);
        assert_eq!(parse_event("not json"), None);
    }

    #[test]
    fn frame_has_magic_and_little_endian_dims() {
        let f = encode_frame(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&f[..4], b"MADO");
        assert_eq!(&f[4..8], &2u32.to_le_bytes());
        assert_eq!(&f[8..12], &1u32.to_le_bytes());
        assert_eq!(&f[12..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn pack_rows_drops_row_padding() {
        // 1×2 image, rowstride 8 (4 bytes padding per row)
        let src = [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0];
        assert_eq!(pack_rows(&src, 1, 2, 8), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn scroll_up_in_mado_scrolls_up_in_gdk() {
        assert!(scroll_delta_to_gdk(40.0, 1.0) < 0.0);
        assert_eq!(scroll_delta_to_gdk(-80.0, 2.0), 1.0);
    }

    #[test]
    fn maps_special_and_printable_keys() {
        assert_eq!(slint_key_to_keyval("\n"), Some(KEY_RETURN));
        assert_eq!(slint_key_to_keyval("\u{8}"), Some(KEY_BACKSPACE));
        assert_eq!(slint_key_to_keyval("\u{F700}"), Some(KEY_UP));
        assert_eq!(slint_key_to_keyval("\u{F70F}"), Some(KEY_F1 + 11));
        assert_eq!(slint_key_to_keyval("a"), Some('a' as u32));
        assert_eq!(slint_key_to_keyval("é"), Some(0xe9));
        assert_eq!(slint_key_to_keyval("€"), Some(0x0100_20ac));
    }

    #[test]
    fn ignores_modifier_only_and_empty_keys() {
        assert_eq!(slint_key_to_keyval("\u{10}"), None); // Shift
        assert_eq!(slint_key_to_keyval("\u{11}"), None); // Control
        assert_eq!(slint_key_to_keyval(""), None);
        assert_eq!(slint_key_to_keyval("ab"), None);
    }
}
