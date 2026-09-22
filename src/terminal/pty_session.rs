use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use vte::Parser;

use super::terminal_state::{TerminalState, VteHandler};

pub struct PtySession {
    pub state: Arc<Mutex<TerminalState>>,
    pub dirty: Arc<AtomicBool>,
    /// Set by the reader thread when the terminal rings the bell, cleared by drain_bells.
    pub bell: Arc<AtomicBool>,
    /// PID of the shell process — used for cwd lookup when OSC 7 hasn't fired.
    pub pid: Option<u32>,
    /// Physical pixel dimensions of the pane — used to size the render buffer
    /// exactly so Slint doesn't need to scale it (which causes blurry artifacts).
    pub phys_w: usize,
    pub phys_h: usize,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl PtySession {
    pub fn spawn(cols: u16, rows: u16, shell: &str, cwd: Option<&str>) -> Self {
        let mut cmd = CommandBuilder::new(shell);
        Self::set_env(&mut cmd, cwd);
        Self::launch(cols, rows, cmd, &[])
    }

    /// Spawn a shell with `pre_bytes` injected into the terminal state before
    /// the reader thread starts — guaranteed to appear before any shell output.
    pub fn spawn_with_banner(cols: u16, rows: u16, shell: &str, cwd: Option<&str>, pre_bytes: &[u8]) -> Self {
        let mut cmd = CommandBuilder::new(shell);
        Self::set_env(&mut cmd, cwd);
        Self::launch(cols, rows, cmd, pre_bytes)
    }

    /// Spawn with an explicit program + argument list and optional extra env vars.
    pub fn spawn_cmd(cols: u16, rows: u16, program: &str, args: &[&str],
                     cwd: Option<&str>, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        Self::set_env(&mut cmd, cwd);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        Self::launch(cols, rows, cmd, &[])
    }

    /// Apply standard environment variables to a CommandBuilder.
    fn set_env(cmd: &mut CommandBuilder, cwd: Option<&str>) {
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // Explicitly propagate STARSHIP_CONFIG so shells pick up Mado's theme palette
        // even if the PTY system doesn't inherit the full parent environment.
        if let Ok(sc) = std::env::var("STARSHIP_CONFIG") {
            cmd.env("STARSHIP_CONFIG", sc);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
    }

    fn launch(cols: u16, rows: u16, cmd: CommandBuilder, pre_bytes: &[u8]) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }).expect("openpty failed");

        let child = pair.slave.spawn_command(cmd).expect("spawn failed");
        let pid = child.process_id();
        drop(child);

        // Drop slave after spawning so the master gets EOF when child exits
        drop(pair.slave);

        let state = Arc::new(Mutex::new(TerminalState::new(cols as usize, rows as usize)));
        let dirty = Arc::new(AtomicBool::new(true));
        let bell  = Arc::new(AtomicBool::new(false));
        let writer = pair.master.take_writer().expect("take_writer failed");

        // Inject pre_bytes BEFORE the reader thread starts — no contention possible.
        if !pre_bytes.is_empty() {
            let mut parser = Parser::new();
            let mut st = state.lock().unwrap();
            let mut handler = VteHandler(&mut *st);
            for &b in pre_bytes {
                parser.advance(&mut handler, b);
            }
            dirty.store(true, Ordering::Relaxed);
        }

        // Background reader thread — lock once per buffer read (not per byte).
        {
            let state = Arc::clone(&state);
            let dirty = Arc::clone(&dirty);
            let bell  = Arc::clone(&bell);
            let mut reader = pair.master.try_clone_reader().expect("clone_reader failed");
            thread::spawn(move || {
                let mut parser = Parser::new();
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            {
                                let mut st = state.lock().unwrap();
                                let mut handler = VteHandler(&mut *st);
                                for &b in &buf[..n] {
                                    parser.advance(&mut handler, b);
                                }
                                if st.bell {
                                    st.bell = false;
                                    bell.store(true, Ordering::Relaxed);
                                }
                            }
                            dirty.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        let phys_w = cols as usize; // placeholder; registry sets real values after spawn
        let phys_h = rows as usize;
        PtySession { state, dirty, bell, pid, phys_w, phys_h, writer, master: pair.master }
    }

    pub fn resize(&mut self, cols: u16, rows: u16, phys_w: usize, phys_h: usize) {
        self.phys_w = phys_w;
        self.phys_h = phys_h;
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.state.lock().unwrap().resize(cols as usize, rows as usize);
        // Signal the timer that this session needs re-rendering
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn write_input(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
    }

    /// Inject raw bytes (e.g. ANSI art) directly into the terminal state,
    /// bypassing the PTY. Safe to call from the main thread.
    #[allow(dead_code)]
    pub fn inject_bytes(&self, data: &[u8]) {
        let mut parser = Parser::new();
        let mut st = self.state.lock().unwrap();
        let mut handler = VteHandler(&mut *st);
        for &b in data {
            parser.advance(&mut handler, b);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }
}
