// Cross-platform text clipboard.
//
// macOS keeps the pbcopy/pbpaste path Mado has always used. Linux goes through
// arboard, which talks to X11 (and Wayland via XWayland). On Linux the process
// that set the clipboard must stay alive to serve it, so the copy side holds
// one long-lived arboard::Clipboard per thread instead of dropping it.

#[cfg(target_os = "macos")]
pub fn set_text(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

#[cfg(target_os = "macos")]
pub fn get_text() -> Option<Vec<u8>> {
    std::process::Command::new("pbpaste").output().ok().map(|o| o.stdout)
}

#[cfg(not(target_os = "macos"))]
pub fn set_text(text: &str) {
    use std::cell::RefCell;
    thread_local! {
        static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = RefCell::new(None);
    }
    CLIPBOARD.with(|cb| {
        let mut cb = cb.borrow_mut();
        if cb.is_none() {
            *cb = arboard::Clipboard::new().ok();
        }
        if let Some(cb) = cb.as_mut() {
            if let Err(e) = cb.set_text(text) {
                eprintln!("mado: clipboard copy failed: {e}");
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn get_text() -> Option<Vec<u8>> {
    // Try wl-paste (Wayland) first, then xclip (X11), then arboard as fallback.
    // arboard::Clipboard::new() fails on Wayland when called from a background thread
    // because there is no Wayland display connection on that thread.
    if let Ok(out) = std::process::Command::new("wl-paste").arg("--no-newline").output() {
        if out.status.success() && !out.stdout.is_empty() {
            return Some(out.stdout);
        }
    }
    if let Ok(out) = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
    {
        if out.status.success() && !out.stdout.is_empty() {
            return Some(out.stdout);
        }
    }
    arboard::Clipboard::new().ok()?.get_text().ok().map(String::into_bytes)
}
