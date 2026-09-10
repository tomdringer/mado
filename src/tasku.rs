// ─── Tasku detection ─────────────────────────────────────────────────────────
//
// Probes the system for an installed `tasku` binary.
// Run once at startup; result is stored in SidebarState.

use std::path::PathBuf;

/// Minimum supported Tasku version.
const MIN_VERSION: (u32, u32, u32) = (0, 3, 9);

pub struct TaskuInstall {
    pub path: PathBuf,
    pub version: Option<String>,
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
                let version = detect_version(&path);
                return Some(TaskuInstall { path: PathBuf::from(path), version });
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
                let version = detect_version(&path);
                return Some(TaskuInstall { path: PathBuf::from(path), version });
            }
        }
    }

    // ── 3. Common install locations ──────────────────────────────────────────
    let candidates = [
        format!("{home}/.asdf/shims/tasku"),
        format!("{home}/.local/bin/tasku"),
        format!("{home}/.cargo/bin/tasku"),
        String::from("/opt/homebrew/bin/tasku"),
        String::from("/usr/local/bin/tasku"),
    ];

    for candidate in &candidates {
        if std::path::Path::new(candidate).exists() {
            let version = detect_version(candidate);
            return Some(TaskuInstall { path: PathBuf::from(candidate), version });
        }
    }

    None
}

/// Run `tasku --version` and return the version string, e.g. `"0.3.9"`.
fn detect_version(path: &str) -> Option<String> {
    let out = std::process::Command::new("/bin/sh")
        .args(["-lc", &format!("{path} --version")])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Output is typically "tasku 0.3.9" — take the last token.
        let ver = s.split_whitespace().last()?.to_string();
        if !ver.is_empty() { return Some(ver); }
    }
    None
}

/// Parse a semver string like `"0.3.9"` into a comparable tuple.
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim().split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Return a status string for the UI:
/// - `"ok"` — installed and at or above the minimum version
/// - `"outdated"` — installed but below the minimum version
/// - `"not-installed"` — not found
pub fn status(install: &Option<TaskuInstall>) -> &'static str {
    match install {
        None => "not-installed",
        Some(t) => {
            if let Some(ref ver) = t.version {
                if let Some(v) = parse_semver(ver) {
                    if v >= MIN_VERSION { return "ok"; }
                    return "outdated";
                }
            }
            // Version couldn't be parsed — assume ok to avoid false warnings.
            "ok"
        }
    }
}
