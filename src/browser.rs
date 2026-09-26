/// Native WKWebView browser panel embedded directly in the Mado NSWindow.
///
/// On macOS the WKWebView is added as a subview of Slint's NSView, positioned
/// to overlay the Slint browser-panel Rectangle.  This gives real keyboard
/// input, 60 fps GPU rendering, native text selection, and Safari Web Inspector
/// support — none of which are achievable via the pixel-plugin protocol.
///
/// On non-macOS targets the struct is a no-op stub so the rest of the code
/// can be written without cfg gates everywhere.

// ── macOS implementation ──────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub use macos_impl::NativeBrowser;

/// Default page to open when no initial URL is configured.
pub const DEFAULT_BROWSER_URL: &str = "https://www.google.com";

/// JS injected as a WKUserScript on every page (DocumentEnd, main-frame only).
/// Adds a persistent nav bar at the top of every page.
const NAV_BAR_JS: &str = r#"
(function() {
    if (document.getElementById('__mado_nav_bar')) return;

    // Push page content down so the bar doesn't cover it.
    var style = document.createElement('style');
    style.id = '__mado_nav_style';
    style.textContent = 'body { margin-top: 44px !important; }';
    document.head.appendChild(style);

    var bar = document.createElement('div');
    bar.id = '__mado_nav_bar';
    bar.style.cssText = [
        'position:fixed', 'top:0', 'left:0', 'right:0', 'height:44px',
        'background:#1e293b', 'display:flex', 'align-items:center',
        'padding:0 8px', 'gap:6px', 'z-index:2147483647',
        'box-shadow:0 1px 4px rgba(0,0,0,0.6)', 'font-family:system-ui,sans-serif'
    ].join(';');

    function btn(label, action) {
        var b = document.createElement('button');
        b.textContent = label;
        b.title = label;
        b.onclick = action;
        b.style.cssText = [
            'background:#334155', 'border:none', 'color:#cbd5e1',
            'width:28px', 'height:28px', 'border-radius:5px',
            'cursor:pointer', 'font-size:15px', 'flex-shrink:0',
            'display:flex', 'align-items:center', 'justify-content:center'
        ].join(';');
        return b;
    }

    bar.appendChild(btn('←', function(){ history.back(); }));
    bar.appendChild(btn('→', function(){ history.forward(); }));
    bar.appendChild(btn('↻', function(){ location.reload(); }));

    var input = document.createElement('input');
    input.type = 'text';
    input.spellcheck = false;
    input.value = location.href;
    input.style.cssText = [
        'flex:1', 'background:#0f172a', 'border:1px solid #475569',
        'color:#f1f5f9', 'padding:4px 12px', 'border-radius:6px',
        'font-size:13px', 'outline:none', 'min-width:0'
    ].join(';');
    input.addEventListener('focus', function() {
        this.value = location.href;
        this.select();
    });
    input.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter') return;
        var raw = this.value.trim();
        if (!raw) return;
        var url = raw;
        if (!/^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(url)) {
            // Looks like a hostname (has a dot, no spaces) or a search query
            if (/^[^\s]+\.[^\s]+$/.test(url)) {
                url = 'https://' + url;
            } else {
                url = 'https://duckduckgo.com/?q=' + encodeURIComponent(url);
            }
        }
        location.href = url;
    });
    bar.appendChild(input);

    // Update the URL input whenever the visible URL changes (SPA navigation, etc.)
    var lastHref = location.href;
    setInterval(function() {
        if (location.href !== lastHref) {
            lastHref = location.href;
            input.value = location.href;
        }
    }, 500);

    document.body.insertBefore(bar, document.body.firstChild);
})();
"#;

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
            initial_url: &str,
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

                let browser = NativeBrowser { wk_view: wk };
                browser.load_url(initial_url);

                Some(browser)
            }
        }

        /// Reposition / resize the WKWebView.
        pub fn update_frame(&self, x: f64, y: f64, w: f64, h: f64) {
            unsafe {
                let frame = cgrect(x, y, w, h);
                let _: () = objc2::msg_send![self.wk_view, setFrame: frame];
            }
        }

        /// Show or hide the WKWebView without destroying it.
        pub fn set_visible(&self, visible: bool) {
            unsafe {
                let _: () = objc2::msg_send![self.wk_view, setHidden: !visible];
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

// ── Non-macOS stub ────────────────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
pub struct NativeBrowser;

#[cfg(not(target_os = "macos"))]
impl NativeBrowser {
    pub fn new(_: *mut (), _: f64, _: f64, _: f64, _: f64, _: &str) -> Option<Self> {
        None
    }
    pub fn update_frame(&self, _: f64, _: f64, _: f64, _: f64) {}
    pub fn set_visible(&self, _: bool) {}
    pub fn go_back(&self) {}
    pub fn load_url(&self, _: &str) {}
}
