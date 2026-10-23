//! Running commands: unprivileged ones directly, privileged ones through the
//! root helper (one `pkexec` prompt per session), or in-process when the GUI
//! itself runs as root.

use portagehub::protocol::{Event, Op, Request};
use portagehub::server::{Server, Sink};
use portagehub::stream::spawn_streaming;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priv {
    User,
    Root,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelperState {
    NotStarted,
    Starting,
    Ready,
    Failed(String),
}

/// Messages from executor threads to the app.
#[derive(Debug)]
pub enum ExecMsg {
    Event(Event),
    HelperReady,
    HelperGone(String),
}

struct HelperConn {
    stdin: ChildStdin,
    child: Child,
}

pub struct Executor {
    next_id: u64,
    tx: Sender<ExecMsg>,
    ctx: egui::Context,
    pub is_root: bool,
    pub helper_state: HelperState,
    helper: Option<HelperConn>,
    local_root: Server,
    /// Requests sent to the helper that have not finished yet.
    pending_root: Arc<Mutex<HashSet<u64>>>,
    /// Requests waiting for the helper to start.
    queued: VecDeque<Request>,
    user_pids: Arc<Mutex<HashMap<u64, u32>>>,
    pub helper_path: Option<PathBuf>,
}

pub fn find_helper() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        cands.push(dir.join("portagehub-helper"));
    }
    cands.push(PathBuf::from("/usr/libexec/portagehub-helper"));
    cands.push(PathBuf::from("/usr/bin/portagehub-helper"));
    cands.push(PathBuf::from("/usr/local/bin/portagehub-helper"));
    cands.into_iter().find(|p| p.is_file())
}

impl Executor {
    pub fn new(tx: Sender<ExecMsg>, ctx: egui::Context) -> Executor {
        let is_root = unsafe { libc::geteuid() } == 0;
        Executor {
            next_id: 1,
            tx,
            ctx,
            is_root,
            helper_state: if is_root { HelperState::Ready } else { HelperState::NotStarted },
            helper: None,
            local_root: Server::default(),
            pending_root: Arc::default(),
            queued: VecDeque::new(),
            user_pids: Arc::default(),
            helper_path: find_helper(),
        }
    }

    pub fn sink(&self) -> Sink {
        let tx = Mutex::new(self.tx.clone());
        let ctx = self.ctx.clone();
        Arc::new(move |ev| {
            let _ = tx.lock().unwrap().send(ExecMsg::Event(ev));
            ctx.request_repaint();
        })
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn root_ready(&self) -> bool {
        self.is_root || self.helper_state == HelperState::Ready
    }

    /// Run a program; output arrives as `ExecMsg::Event`s tagged with the returned id.
    pub fn run(&mut self, argv: Vec<String>, env: Vec<(String, String)>, privilege: Priv) -> u64 {
        let id = self.alloc_id();
        match privilege {
            Priv::User => {
                let sink = self.sink();
                let pids = self.user_pids.clone();
                let wrapped: Sink = Arc::new(move |ev: Event| {
                    if let Event::Exit { id, .. } = &ev {
                        pids.lock().unwrap().remove(id);
                    }
                    sink(ev)
                });
                match spawn_streaming(id, &argv[0], &argv[1..], &env, None, wrapped) {
                    Ok(pid) => {
                        self.user_pids.lock().unwrap().insert(id, pid);
                    }
                    Err(e) => {
                        let _ = self.tx.send(ExecMsg::Event(Event::Err { id, msg: format!("{}: {e}", argv[0]) }));
                    }
                }
            }
            Priv::Root => self.submit_root(Request { id, op: Op::Run { argv, env, cwd: None } }),
        }
        id
    }

    pub fn root_op(&mut self, op: Op) -> u64 {
        let id = self.alloc_id();
        self.submit_root(Request { id, op });
        id
    }

    pub fn cancel(&mut self, id: u64, privilege: Priv, signal: i32) {
        match privilege {
            Priv::User => {
                if let Some(pid) = self.user_pids.lock().unwrap().get(&id) {
                    unsafe {
                        libc::kill(-(*pid as i32), signal);
                    }
                }
            }
            Priv::Root => {
                let sid = self.alloc_id();
                self.submit_root(Request { id: sid, op: Op::Signal { target: id, signal } });
            }
        }
    }

    fn submit_root(&mut self, req: Request) {
        if self.is_root {
            self.local_root.handle(req, self.sink());
            return;
        }
        match &self.helper_state {
            HelperState::Ready => self.send_to_helper(req),
            HelperState::Starting => self.queued.push_back(req),
            HelperState::NotStarted | HelperState::Failed(_) => {
                self.queued.push_back(req);
                self.start_helper();
            }
        }
    }

    fn send_to_helper(&mut self, req: Request) {
        let id = req.id;
        let line = serde_json::to_string(&req).unwrap_or_default();
        let ok = match self.helper.as_mut() {
            Some(h) => writeln!(h.stdin, "{line}").and_then(|_| h.stdin.flush()).is_ok(),
            None => false,
        };
        if ok {
            self.pending_root.lock().unwrap().insert(id);
        } else {
            let _ = self.tx.send(ExecMsg::Event(Event::Err { id, msg: "root helper is not running".into() }));
            self.helper_state = HelperState::Failed("helper pipe closed".into());
        }
    }

    pub fn start_helper(&mut self) {
        let Some(path) = self.helper_path.clone() else {
            self.fail_queued("portagehub-helper binary not found next to the GUI or in /usr/libexec".into());
            return;
        };
        self.helper_state = HelperState::Starting;
        // PORTAGEHUB_ELEVATE: "pkexec" (default), "none" (run unprivileged — testing only),
        // or any launcher command such as "doas" or "sudo".
        let launcher = std::env::var("PORTAGEHUB_ELEVATE").unwrap_or_else(|_| "pkexec".into());
        let mut cmd = if launcher == "none" {
            Command::new(&path)
        } else {
            let mut parts = launcher.split_whitespace();
            let mut c = Command::new(parts.next().unwrap_or("pkexec"));
            c.args(parts).arg(&path);
            c
        };
        let spawn = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawn {
            Ok(c) => c,
            Err(e) => {
                self.fail_queued(format!("could not start pkexec: {e}"));
                return;
            }
        };
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let pending = self.pending_root.clone();
        let err_text: Arc<Mutex<String>> = Arc::default();
        let err2 = err_text.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut e = err2.lock().unwrap();
                if e.len() < 2000 {
                    e.push_str(&line);
                    e.push('\n');
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match serde_json::from_str::<Event>(&line) {
                    Ok(Event::Ready { .. }) => {
                        let _ = tx.send(ExecMsg::HelperReady);
                    }
                    Ok(ev) => {
                        if let Some(id) = ev.id()
                            && matches!(ev, Event::Exit { .. } | Event::Ok { .. } | Event::Err { .. })
                        {
                            pending.lock().unwrap().remove(&id);
                        }
                        let _ = tx.send(ExecMsg::Event(ev));
                    }
                    Err(_) => {}
                }
                ctx.request_repaint();
            }
            // Helper ended: fail whatever was still pending.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let ids: Vec<u64> = pending.lock().unwrap().drain().collect();
            for id in ids {
                let _ = tx.send(ExecMsg::Event(Event::Err { id, msg: "root helper exited".into() }));
            }
            let reason = err_text.lock().unwrap().trim().to_string();
            let _ = tx.send(ExecMsg::HelperGone(if reason.is_empty() { "authentication cancelled or helper exited".into() } else { reason }));
            ctx.request_repaint();
        });
        self.helper = Some(HelperConn { stdin, child });
    }

    fn fail_queued(&mut self, msg: String) {
        self.helper_state = HelperState::Failed(msg.clone());
        while let Some(r) = self.queued.pop_front() {
            let _ = self.tx.send(ExecMsg::Event(Event::Err { id: r.id, msg: msg.clone() }));
        }
        self.ctx.request_repaint();
    }

    /// Called by the app when the helper reported Ready.
    pub fn on_helper_ready(&mut self) {
        self.helper_state = HelperState::Ready;
        while let Some(r) = self.queued.pop_front() {
            self.send_to_helper(r);
        }
    }

    pub fn on_helper_gone(&mut self, reason: String) {
        if self.helper.is_none() && self.helper_state == HelperState::NotStarted {
            // We closed it ourselves.
            return;
        }
        if let Some(mut h) = self.helper.take() {
            let _ = h.child.try_wait();
        }
        self.fail_queued(reason);
    }

    pub fn stop_helper(&mut self) {
        // Closing stdin makes the helper exit once its jobs are done.
        self.helper = None;
        if !self.is_root {
            self.helper_state = HelperState::NotStarted;
        }
    }
}
