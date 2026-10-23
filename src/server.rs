//! Request handling shared by the root helper and by the GUI when it already
//! runs as root (then no helper process is needed).

use crate::protocol::{self, Event, Op, Request};
use crate::stream::spawn_streaming;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Default, Clone)]
pub struct Server {
    pgids: Arc<Mutex<HashMap<u64, u32>>>,
    running: Arc<Mutex<usize>>,
}

impl Server {
    pub fn running(&self) -> usize {
        *self.running.lock().unwrap()
    }

    pub fn handle(&self, req: Request, sink: Sink) {
        let id = req.id;
        let reply = |r: Result<(), String>| {
            sink(match r {
                Ok(()) => Event::Ok { id },
                Err(msg) => Event::Err { id, msg },
            })
        };
        match req.op {
            Op::Ping => reply(Ok(())),
            Op::Run { argv, env, cwd } => {
                let Some(first) = argv.first() else {
                    return reply(Err("empty command".into()));
                };
                let Some(program) = resolve_program(first) else {
                    return reply(Err(format!("program not allowed: {first}")));
                };
                let env: Vec<_> = env.into_iter().filter(|(k, _)| protocol::env_key_allowed(k)).collect();
                let pg = self.pgids.clone();
                let run = self.running.clone();
                *self.running.lock().unwrap() += 1;
                let inner = sink.clone();
                let wrapped: Sink = Arc::new(move |ev: Event| {
                    if let Event::Exit { id, .. } = &ev {
                        pg.lock().unwrap().remove(id);
                        let mut r = run.lock().unwrap();
                        *r = r.saturating_sub(1);
                    }
                    inner(ev);
                });
                match spawn_streaming(id, &program, &argv[1..], &env, cwd.as_deref().or(Some("/")), wrapped) {
                    Ok(pid) => {
                        self.pgids.lock().unwrap().insert(id, pid);
                    }
                    Err(e) => {
                        let mut r = self.running.lock().unwrap();
                        *r = r.saturating_sub(1);
                        drop(r);
                        reply(Err(format!("failed to start {program}: {e}")));
                    }
                }
            }
            Op::WriteFile { path, content, backup } => reply(write_file(&path, &content, backup)),
            Op::DeleteFile { path } => reply(
                protocol::path_allowed(&path)
                    .ok_or_else(|| format!("path not allowed: {path}"))
                    .and_then(|p| std::fs::remove_file(p).map_err(|e| e.to_string())),
            ),
            Op::Mkdir { path } => reply(
                protocol::path_allowed(&path)
                    .ok_or_else(|| format!("path not allowed: {path}"))
                    .and_then(|p| std::fs::create_dir_all(p).map_err(|e| e.to_string())),
            ),
            Op::Rename { from, to } => {
                let r = match (protocol::path_allowed(&from), protocol::path_allowed(&to)) {
                    (Some(_), Some(b)) if b.exists() => Err(format!("{} already exists", b.display())),
                    (Some(a), Some(b)) => {
                        if let Some(parent) = b.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::rename(a, b).map_err(|e| e.to_string())
                    }
                    _ => Err("path not allowed".into()),
                };
                reply(r)
            }
            Op::ConfigUpdate { path, accept } => {
                let r = match protocol::config_update_paths(&path) {
                    None => Err(format!("not a config update file: {path}")),
                    Some((upd, target)) => {
                        if !upd.is_file() {
                            Err(format!("{path} does not exist"))
                        } else if accept {
                            std::fs::rename(&upd, &target).map_err(|e| e.to_string())
                        } else {
                            std::fs::remove_file(&upd).map_err(|e| e.to_string())
                        }
                    }
                };
                reply(r)
            }
            Op::Signal { target, signal } => {
                let pid = self.pgids.lock().unwrap().get(&target).copied();
                match pid {
                    Some(pid) => {
                        let rc = unsafe { libc::kill(-(pid as i32), signal) };
                        reply(if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) })
                    }
                    None => reply(Err("no such running job".into())),
                }
            }
        }
    }
}

pub fn resolve_program(name: &str) -> Option<String> {
    let base = Path::new(name).file_name()?.to_str()?;
    if !protocol::ALLOWED_PROGRAMS.contains(&base) {
        return None;
    }
    protocol::PROGRAM_DIRS
        .iter()
        .map(|d| PathBuf::from(d).join(base))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn write_file(path: &str, content: &str, backup: bool) -> Result<(), String> {
    let p = protocol::path_allowed(path).ok_or_else(|| format!("path not allowed: {path}"))?;
    if p.is_dir() {
        return Err(format!("{path} is a directory"));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if backup && p.exists() {
        // `~` suffix: Portage ignores these files.
        let mut b = p.clone().into_os_string();
        b.push("~");
        std::fs::copy(&p, PathBuf::from(b)).map_err(|e| e.to_string())?;
    }
    let mut tmp = p.clone().into_os_string();
    tmp.push(".portagehub-tmp~");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
    // Keep the original mode when replacing a file, default to 0644.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&p).map(|m| m.permissions().mode()).unwrap_or(0o644);
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode & 0o7777));
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}
