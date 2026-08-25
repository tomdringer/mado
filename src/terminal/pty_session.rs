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
    /// PID of the shell process — used for cwd lookup when OSC 7 hasn't fired.
    pub pid: Option<u32>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl PtySession {
    pub fn spawn(cols: u16, rows: u16, shell: &str, cwd: Option<&str>) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }).expect("openpty failed");

        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn failed");
        let pid = child.process_id();
        drop(child);

        // Drop slave after spawning so the master gets EOF when child exits
        drop(pair.slave);

        let state = Arc::new(Mutex::new(TerminalState::new(cols as usize, rows as usize)));
        let dirty = Arc::new(AtomicBool::new(true));
        let writer = pair.master.take_writer().expect("take_writer failed");

        // Background reader thread
        {
            let state = Arc::clone(&state);
            let dirty = Arc::clone(&dirty);
            let mut reader = pair.master.try_clone_reader().expect("clone_reader failed");
            thread::spawn(move || {
                let mut parser = Parser::new();
                let mut handler = VteHandler(Arc::clone(&state));
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for &b in &buf[..n] {
                                parser.advance(&mut handler, b);
                            }
                            dirty.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        PtySession { state, dirty, pid, writer, master: pair.master }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.state.lock().unwrap().resize(cols as usize, rows as usize);
        // Signal the timer that this session needs re-rendering
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn write_input(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
    }
}
