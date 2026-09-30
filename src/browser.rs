/// Native WKWebView browser panel embedded directly in the Mado NSWindow.
///
/// On macOS the WKWebView is added as a subview of Slint's NSView, positioned
/// to overlay the Slint browser-panel Rectangle.  This gives real keyboard
/// input, 60 fps GPU rendering, native text selection, and Safari Web Inspector
/// support — none of which are achievable via the pixel-plugin protocol.
///
/// On other platforms the same API is backed by the off-screen `mado-webview`
/// helper (see the bottom of this file).

// ── macOS implementation ──────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub use macos_impl::NativeBrowser;

/// Default page to open when no initial URL is configured.
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_BROWSER_URL: &str = "https://www.google.com";

/// JS injected as a WKUserScript on every page (DocumentEnd, main-frame only).
/// Adds a persistent nav bar at the top of every page.
#[cfg(target_os = "macos")]
const NAV_BAR_JS: &str = include_str!("browser_nav_bar.js");

#[cfg(target_os = "macos")]
pub mod macos_impl {
    use super::NAV_BAR_JS;
    use objc2::encode::{Encode, Encoding, RefEncode};
    use objc2::runtime::{AnyClass, AnyObject};

    // CGRect / CGPoint / CGSize — ABI-compatible with the C structs in CoreGraphics.
    // We must implement objc2::Encode so msg_send! can pass these structs by value.
    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    struct CGPoint { x: f64, y: f64 }

    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct(
            "CGPoint",
            &[f64::ENCODING, f64::ENCODING],
        );
    }
    unsafe impl RefEncode for CGPoint {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    struct CGSize { width: f64, height: f64 }

    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct(
            "CGSize",
            &[f64::ENCODING, f64::ENCODING],
        );
    }
    unsafe impl RefEncode for CGSize {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    #[repr(C)]
    #[derive(Copy, Clone, Debug)]
    pub struct CGRect {
        origin: CGPoint,
        size:   CGSize,
    }

    unsafe impl Encode for CGRect {
        const ENCODING: Encoding = Encoding::Struct(
            "CGRect",
            &[CGPoint::ENCODING, CGSize::ENCODING],
        );
    }
    unsafe impl RefEncode for CGRect {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    fn cgrect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size:   CGSize { width: w.max(1.0), height: h.max(1.0) },
        }
    }

    /// Build an NSString from a Rust &str (UTF-8).
    ///
    /// SAFETY: caller must use this on the main thread, within an autorelease pool.
    unsafe fn nsstr(s: &str) -> *mut AnyObject {
        let bytes = s.as_bytes();
        let cls: &AnyClass = AnyClass::get("NSString")
            .expect("NSString class not found");
        let obj: *mut AnyObject = objc2::msg_send![cls, alloc];
        // NSUTF8StringEncoding = 4
        objc2::msg_send![
            obj,
            initWithBytes: bytes.as_ptr() as *const std::ffi::c_void
            length:        bytes.len()
            encoding:      4u64
        ]
    }

    /// A native WKWebView embedded in the Mado window.
    ///
    /// Must only be used on the main thread (same as all AppKit objects).
    pub struct NativeBrowser {
        wk_view: *mut AnyObject,
    }

    // Raw pointer is main-thread-only; stored in Rc<RefCell<>>, never in Arc.
    unsafe impl Send for NativeBrowser {}

    impl NativeBrowser {
        /// Create a WKWebView, inject the nav-bar user script, and attach it as a
        /// subview of `parent_ns_view` (Slint's NSView).
        ///
        /// `x`, `y`, `w`, `h` are in NSView logical-point coordinates
        /// (y=0 at the visual bottom for a non-flipped view).
        /// The view starts hidden; call `set_visible(true)` when the panel opens.
        pub fn new(
            parent_ns_view: *mut AnyObject,
            x: f64, y: f64,
            w: f64, h: f64,
        ) -> Option<Self> {
            if parent_ns_view.is_null() { return None; }

            unsafe {
                // ── WKWebViewConfiguration ────────────────────────────────────
                let cfg_cls = AnyClass::get("WKWebViewConfiguration")?;
                let cfg: *mut AnyObject = objc2::msg_send![cfg_cls, alloc];
                let cfg: *mut AnyObject = objc2::msg_send![cfg, init];

                // Enable Safari Web Inspector via legacy preferences key.
                let prefs: *mut AnyObject = objc2::msg_send![cfg, preferences];
                let key_str = nsstr("developerExtrasEnabled");
                let num_cls = AnyClass::get("NSNumber")?;
                let yes: *mut AnyObject = objc2::msg_send![num_cls, numberWithBool: true];
                let _: () = objc2::msg_send![prefs, setValue: yes forKey: key_str];

                // ── Nav-bar user script ───────────────────────────────────────
                // WKUserScriptInjectionTimeAtDocumentEnd = 1
                if let Some(script_cls) = AnyClass::get("WKUserScript") {
                    let js_src = nsstr(NAV_BAR_JS);
                    let script: *mut AnyObject = objc2::msg_send![script_cls, alloc];
                    let script: *mut AnyObject = objc2::msg_send![
                        script,
                        initWithSource:          js_src
                        injectionTime:           1isize  // WKUserScriptInjectionTimeAtDocumentEnd
                        forMainFrameOnly:        true
                    ];
                    if !script.is_null() {
                        let controller: *mut AnyObject =
                            objc2::msg_send![cfg, userContentController];
                        let _: () = objc2::msg_send![controller, addUserScript: script];
                    }
                }

                // ── WKWebView ─────────────────────────────────────────────────
                let frame = cgrect(x, y, w, h);
                let wk_cls = AnyClass::get("WKWebView")?;
                let wk: *mut AnyObject = objc2::msg_send![wk_cls, alloc];
                let wk: *mut AnyObject = objc2::msg_send![
                    wk, initWithFrame: frame configuration: cfg
                ];
                if wk.is_null() { return None; }

                // setInspectable: YES — macOS 13.3+ (safe for Mado's target range).
                let _: () = objc2::msg_send![wk, setInspectable: true];

                // ── Add as subview ────────────────────────────────────────────
                let _: () = objc2::msg_send![parent_ns_view, addSubview: wk];

                // Start hidden; frame is corrected on first show.
                let _: () = objc2::msg_send![wk, setHidden: true];

                // Reclaim first responder for Slint's view — WKWebView can steal it
                // during addSubview even when hidden.
                let ns_window: *mut AnyObject = objc2::msg_send![parent_ns_view, window];
                if !ns_window.is_null() {
                    let _: bool = objc2::msg_send![ns_window, makeFirstResponder: parent_ns_view];
                }

                // Don't pre-load a URL here — WebKit steals first responder focus
                // during page load even on a hidden view.  The URL is loaded the
                // first time load_url() is called via a MACT navigate action.

                Some(NativeBrowser { wk_view: wk })
            }
        }

        /// Reposition / resize the WKWebView.
        pub fn update_frame(&self, x: f64, y: f64, w: f64, h: f64) {
            unsafe {
                let frame = cgrect(x, y, w, h);
                let _: () = objc2::msg_send![self.wk_view, setFrame: frame];
            }
        }

        /// Scale factor changed — WKWebView positions are in logical pixels so
        /// no action needed; the next update_frame call repositions correctly.
        pub fn set_scale(&self, _scale: f32) {}

        /// Show or hide the WKWebView without destroying it.
        pub fn set_visible(&self, visible: bool) {
            unsafe {
                let _: () = objc2::msg_send![self.wk_view, setHidden: !visible];
            }
        }

        /// Return first responder to Slint's content view (the WKWebView's superview).
        /// Call this after making the WKWebView visible so macOS keyboard events
        /// continue reaching Slint rather than being swallowed by WebKit.
        pub fn restore_first_responder(&self) {
            unsafe {
                use objc2::runtime::AnyObject;
                let parent: *mut AnyObject = objc2::msg_send![self.wk_view, superview];
                if parent.is_null() { return; }
                let ns_window: *mut AnyObject = objc2::msg_send![parent, window];
                if ns_window.is_null() { return; }
                let _: bool = objc2::msg_send![ns_window, makeFirstResponder: parent];
            }
        }


        /// Navigate back in browser history.
        pub fn go_back(&self) {
            unsafe {
                let _: () = objc2::msg_send![self.wk_view, goBack];
            }
        }

        /// Navigate to `url`.
        pub fn load_url(&self, url: &str) {
            unsafe {
                let ns_url_str = nsstr(url);
                let Some(url_cls) = AnyClass::get("NSURL") else { return };
                let ns_url: *mut AnyObject =
                    objc2::msg_send![url_cls, URLWithString: ns_url_str];
                if ns_url.is_null() { return; }
                let Some(req_cls) = AnyClass::get("NSURLRequest") else { return };
                let req: *mut AnyObject =
                    objc2::msg_send![req_cls, requestWithURL: ns_url];
                let _: *mut AnyObject = objc2::msg_send![self.wk_view, loadRequest: req];
            }
        }
    }
}

// ── Linux (and other non-macOS) implementation ────────────────────────────────
//
// Wayland does not allow embedding another toolkit's widget in Mado's window,
// so the page is rendered off-screen by the `mado-webview` helper (WebKitGTK)
// and streamed back over the pixel-plugin protocol. Mado draws the frames in
// the browser panel and forwards pointer, scroll and key input to the helper.
// Positions passed in are logical pixels; the helper works in physical pixels.

#[cfg(not(target_os = "macos"))]
pub use offscreen_impl::NativeBrowser;

#[cfg(not(target_os = "macos"))]
mod offscreen_impl {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use slint::{Rgba8Pixel, SharedPixelBuffer};

    use crate::pixel_plugin::PixelPlugin;

    pub struct NativeBrowser {
        plugin: RefCell<PixelPlugin>,
        scale:  Cell<f32>,
    }

    /// Prefer the helper installed next to the running `mado` binary (cargo
    /// builds both into the same target dir); otherwise rely on $PATH.
    fn helper_path() -> PathBuf {
        std::env::current_exe().ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("mado-webview")))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("mado-webview"))
    }

    impl NativeBrowser {
        /// Start the off-screen browser at `w`×`h` logical pixels. Returns None
        /// (and logs why) if the helper cannot be started.
        pub fn spawn(initial_url: &str, w: f64, h: f64, scale: f32) -> Option<Self> {
            let helper = helper_path();
            let scale_env = scale.to_string();
            let plugin = PixelPlugin::spawn(
                &helper.to_string_lossy(), &[initial_url],
                (w as f32 * scale).max(1.0) as u32, (h as f32 * scale).max(1.0) as u32,
                &[("MADO_SCALE", &scale_env)],
            );
            if plugin.is_none() {
                eprintln!("mado: could not start browser helper '{}' — is it installed?", helper.display());
            }
            let browser = NativeBrowser { plugin: RefCell::new(plugin?), scale: Cell::new(scale) };
            // Start hidden, like the macOS WKWebView; the panel toggle shows it.
            browser.set_visible(false);
            Some(browser)
        }

        /// Resize the page to the panel's new size (position is irrelevant
        /// off-screen, but kept for parity with the macOS API).
        pub fn update_frame(&self, _x: f64, _y: f64, w: f64, h: f64) {
            let (pw, ph) = (self.phys(w as f32).max(1.0), self.phys(h as f32).max(1.0));
            self.plugin.borrow_mut().send_resize(pw as u32, ph as u32);
        }

        /// The display scale changed: re-zoom the page. The caller resizes it
        /// afterwards via `update_frame`.
        pub fn set_scale(&self, scale: f32) {
            self.scale.set(scale);
            self.plugin.borrow_mut().send_scale(scale);
        }

        /// Hidden browsers stop publishing frames to save CPU.
        pub fn set_visible(&self, visible: bool) {
            self.plugin.borrow_mut().send_visible(visible);
        }

        pub fn go_back(&self) {
            self.plugin.borrow_mut().send_back();
        }

        pub fn load_url(&self, url: &str) {
            self.plugin.borrow_mut().send_navigate(url);
        }

        /// The newest frame, if one arrived since the last call.
        pub fn take_frame(&self) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
            let plugin = self.plugin.borrow();
            if !plugin.dirty.swap(false, Ordering::Relaxed) { return None; }
            plugin.image.lock().ok()?.take()
        }

        pub fn set_focused(&self, focused: bool) {
            self.plugin.borrow_mut().send_focus(focused);
        }

        pub fn mouse_press(&self, x: f32, y: f32) {
            self.plugin.borrow_mut().send_mouse_press(self.phys(x), self.phys(y));
        }

        pub fn mouse_move(&self, x: f32, y: f32) {
            self.plugin.borrow_mut().send_mouse_move(self.phys(x), self.phys(y));
        }

        pub fn mouse_release(&self) {
            self.plugin.borrow_mut().send_mouse_release();
        }

        pub fn scroll(&self, delta: f32) {
            self.plugin.borrow_mut().send_scroll(self.phys(delta));
        }

        pub fn key(&self, text: &str, ctrl: bool, meta: bool, alt: bool, shift: bool) {
            self.plugin.borrow_mut().send_key(text, ctrl, meta, alt, shift);
        }

        pub fn restore_first_responder(&self) {}

        fn phys(&self, logical: f32) -> f32 {
            logical * self.scale.get()
        }
    }
}
