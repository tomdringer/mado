use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

pub struct PtySession {
    /// Raw PTY bytes received from the child process.
    pub bytes_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Set by the reader thread when new bytes arrive.
    pub dirty: Arc<AtomicBool>,
    /// Set by the on_bell callback; cleared by drain_bells in registry.
    pub bell: Arc<AtomicBool>,
    /// PID of the shell process.
    pub pid: Option<u32>,
    /// Physical pixel dimensions of the pane.
    pub phys_w: usize,
    pub phys_h: usize,
    /// Set by the wait thread when the child process exits.
    pub exited: Arc<AtomicBool>,
    /// True if the process exited successfully.
    pub exit_success: Arc<AtomicBool>,
    /// Shared writer used for both user key input and the on_pty_write callback.
    pub pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// PTY master handle (for resize).
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Bytes to inject via vt_write before the first PTY bytes arrive.
    pub pre_bytes: Vec<u8>,
}

impl PtySession {
    pub fn spawn(cols: u16, rows: u16, shell: &str, cwd: Option<&str>) -> Self {
        let mut cmd = CommandBuilder::new(shell);
        Self::set_env(&mut cmd, cwd);
        Self::launch(cols, rows, cmd, &[])
    }

    pub fn spawn_with_banner(cols: u16, rows: u16, shell: &str, cwd: Option<&str>, pre_bytes: &[u8]) -> Self {
        let mut cmd = CommandBuilder::new(shell);
        Self::set_env(&mut cmd, cwd);
        Self::launch(cols, rows, cmd, pre_bytes)
    }

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

    fn set_env(cmd: &mut CommandBuilder, cwd: Option<&str>) {
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
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

        let mut child = pair.slave.spawn_command(cmd).expect("spawn failed");
        let pid = child.process_id();

        drop(pair.slave);

        let exited: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let exit_success: Arc<AtomicBool> = Arc::new(AtomicBool::new(true));
        {
            let exited_w = Arc::clone(&exited);
            let exit_success_w = Arc::clone(&exit_success);
            thread::spawn(move || {
                if let Ok(status) = child.wait() {
                    exit_success_w.store(status.success(), Ordering::Relaxed);
                }
                exited_w.store(true, Ordering::Relaxed);
            });
        }

        let dirty = Arc::new(AtomicBool::new(true));
        let bell = Arc::new(AtomicBool::new(false));

        // Single writer wrapped in Arc<Mutex> so both user input and
        // the on_pty_write callback can share it.
        let writer = pair.master.take_writer().expect("take_writer failed");
        let pty_writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));

        // Reader thread: reads PTY bytes → sends via channel → sets dirty
        let (bytes_tx, bytes_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        {
            let dirty = Arc::clone(&dirty);
            let mut reader = pair.master.try_clone_reader().expect("clone_reader failed");
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = bytes_tx.send(buf[..n].to_vec());
                            dirty.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }

        let phys_w = cols as usize;
        let phys_h = rows as usize;
        PtySession {
            bytes_rx,
            dirty,
            bell,
            pid,
            phys_w,
            phys_h,
            exited,
            exit_success,
            pty_writer,
            master: pair.master,
            pre_bytes: pre_bytes.to_vec(),
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16, phys_w: usize, phys_h: usize) {
        self.phys_w = phys_w;
        self.phys_h = phys_h;
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn write_input(&mut self, data: &[u8]) {
        if let Ok(mut w) = self.pty_writer.lock() {
            let _ = w.write_all(data);
        }
    }
}
