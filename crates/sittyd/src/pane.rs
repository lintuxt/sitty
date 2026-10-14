use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use sitty_core::{AlacrittyTerminal, Size, Terminal, Transcript};
use tokio::sync::watch;

const SCROLLBACK_LINES: usize = 10_000;
const TRANSCRIPT_BYTES: usize = 1 << 20;
/// After the child exits, how long to wait for the reader to drain remaining output.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

pub struct SpawnSpec {
    /// None = the user's login shell.
    pub cmd: Option<Vec<String>>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: Size,
}

pub struct PaneIo {
    pub term: Box<dyn Terminal>,
    pub transcript: Transcript,
    pub last_output: Instant,
    pub exit_code: Option<i32>,
}

pub struct PaneRuntime {
    io: Mutex<PaneIo>,
    changed: watch::Sender<u64>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    _master: Mutex<Box<dyn MasterPty + Send>>,
    cmd: Vec<String>,
}

/// Recover from a poisoned lock instead of cascading one task's panic into every pane.
fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl PaneRuntime {
    pub fn spawn(spec: SpawnSpec) -> std::io::Result<Arc<PaneRuntime>> {
        let to_io = |e: anyhow::Error| std::io::Error::other(e.to_string());
        let pair = native_pty_system()
            .openpty(PtySize { rows: spec.size.rows, cols: spec.size.cols, pixel_width: 0, pixel_height: 0 })
            .map_err(to_io)?;

        let mut cmd = match &spec.cmd {
            Some(argv) if !argv.is_empty() => {
                let mut c = CommandBuilder::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
            _ => CommandBuilder::new_default_prog(),
        };
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", "xterm-256color");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let shown_cmd = match &spec.cmd {
            Some(argv) if !argv.is_empty() => argv.clone(),
            _ => vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())],
        };

        let mut child = pair.slave.spawn_command(cmd).map_err(to_io)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(to_io)?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(to_io)?));

        let (changed, _) = watch::channel(0u64);
        let rt = Arc::new(PaneRuntime {
            io: Mutex::new(PaneIo {
                term: Box::new(AlacrittyTerminal::new(spec.size, SCROLLBACK_LINES)),
                transcript: Transcript::new(TRANSCRIPT_BYTES),
                last_output: Instant::now(),
                exit_code: None,
            }),
            changed,
            writer,
            killer: Mutex::new(killer),
            _master: Mutex::new(pair.master),
            cmd: shown_cmd,
        });

        // Reader: PTY output -> terminal + transcript; query replies -> PTY.
        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        let r = Arc::clone(&rt);
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let replies = {
                            let mut io = lock(&r.io);
                            let replies = io.term.feed(&buf[..n]);
                            io.transcript.feed(&buf[..n]);
                            io.last_output = Instant::now();
                            replies
                        };
                        if !replies.is_empty() {
                            let _ = lock(&r.writer).write_all(&replies);
                        }
                        r.changed.send_modify(|g| *g += 1);
                    }
                }
            }
            let _ = drained_tx.send(());
        });

        // Waiter: child exit -> wait for the reader to drain -> publish exit code.
        let w = Arc::clone(&rt);
        thread::spawn(move || {
            let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
            let _ = drained_rx.recv_timeout(DRAIN_GRACE);
            lock(&w.io).exit_code = Some(code);
            w.changed.send_modify(|g| *g += 1);
        });

        Ok(rt)
    }

    pub fn io(&self) -> MutexGuard<'_, PaneIo> {
        lock(&self.io)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = lock(&self.writer);
        w.write_all(bytes)?;
        w.flush()
    }

    pub fn kill(&self) {
        let _ = lock(&self.killer).kill();
    }

    pub fn cmd(&self) -> &[String] {
        &self.cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn spec(cmd: &[&str]) -> SpawnSpec {
        SpawnSpec {
            cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
            cwd: std::env::temp_dir(),
            env: vec![("SITTY_PANE".into(), "p1".into())],
            size: Size { cols: 80, rows: 24 },
        }
    }

    fn wait_exit(p: &PaneRuntime) -> i32 {
        for _ in 0..200 {
            if let Some(c) = p.io().exit_code {
                return c;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("pane never exited");
    }

    #[test]
    fn runs_command_with_env_and_reports_exit() {
        let p = PaneRuntime::spawn(spec(&["/bin/sh", "-c", "echo pane=$SITTY_PANE; exit 3"])).unwrap();
        assert_eq!(wait_exit(&p), 3);
        assert_eq!(p.io().term.screen_lines()[0], "pane=p1");
    }

    #[test]
    fn exit_reported_after_final_output() {
        // Lots of output right before exit: exit must not be visible before all of it is fed.
        let p = PaneRuntime::spawn(spec(&["/bin/sh", "-c", "seq 1 3000; echo LAST"])).unwrap();
        wait_exit(&p);
        assert!(p.io().transcript.since(0).ends_with("LAST\n"));
    }

    #[test]
    fn input_reaches_the_process() {
        let p = PaneRuntime::spawn(spec(&["/bin/cat"])).unwrap();
        p.write(b"hello\r").unwrap();
        for _ in 0..200 {
            if p.io().transcript.since(0).contains("hello") {
                p.kill();
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("echo never arrived");
    }

    #[test]
    fn kill_ends_the_process() {
        let p = PaneRuntime::spawn(spec(&["/bin/sleep", "60"])).unwrap();
        p.kill();
        wait_exit(&p);
    }

    #[test]
    fn subscribers_see_changes() {
        let p = PaneRuntime::spawn(spec(&["/bin/sh", "-c", "echo hi"])).unwrap();
        let rx = p.subscribe();
        wait_exit(&p);
        assert!(*rx.borrow() > 0);
    }
}
