// ─── Tasku detection ─────────────────────────────────────────────────────────
//
// Probes the system for an installed `tasku` binary.
// Run once at startup; result is stored in SidebarState.

use std::path::PathBuf;

pub struct TaskuInstall {
    pub path: PathBuf,
}

/// Detect a `tasku` binary. Returns `Some` with the resolved path, or `None`.
///
/// Strategy:
///   1. Ask the login shell (`/bin/sh -lc`) — picks up `/etc/profile` / `~/.profile`.
///   2. Ask zsh in interactive mode (`/bin/zsh -ic`) — picks up `~/.zshrc` where
///      asdf, homebrew, and similar tools are typically configured on macOS.
///   3. Fall back to probing well-known install locations directly.
pub fn detect() -> Option<TaskuInstall> {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("."));

    // ── 1. Login shell probe (/bin/sh reads ~/.profile) ──────────────────────
    if let Ok(out) = std::process::Command::new("/bin/sh")
        .args(["-lc", "command -v tasku"])
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(TaskuInstall { path: PathBuf::from(path) });
            }
        }
    }

    // ── 2. Zsh interactive probe (/bin/zsh -ic reads ~/.zshrc) ───────────────
    // Needed when asdf/homebrew are configured in ~/.zshrc rather than ~/.profile.
    if let Ok(out) = std::process::Command::new("/bin/zsh")
        .args(["-ic", "command -v tasku"])
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(TaskuInstall { path: PathBuf::from(path) });
            }
        }
    }

    // ── 2. Common install locations ──────────────────────────────────────────
    let candidates = [
        format!("{home}/.asdf/shims/tasku"),
        format!("{home}/.local/bin/tasku"),
        format!("{home}/.cargo/bin/tasku"),
        String::from("/opt/homebrew/bin/tasku"),
        String::from("/usr/local/bin/tasku"),
    ];

    for candidate in &candidates {
        if std::path::Path::new(candidate).exists() {
            return Some(TaskuInstall { path: PathBuf::from(candidate) });
        }
    }

    None
}
