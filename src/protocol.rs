//! JSON-lines protocol spoken between the GUI and the root helper.
//!
//! The GUI writes one [`Request`] per line to the helper's stdin, the helper
//! answers with [`Event`]s, one per line, on its stdout.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Ping,
    /// Spawn a program and stream its output back.
    Run {
        argv: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Atomically replace a file (creating parent directories when needed).
    WriteFile {
        path: String,
        content: String,
        #[serde(default)]
        backup: bool,
    },
    DeleteFile {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Send a signal to the process group of a previously started `Run`.
    Signal {
        target: u64,
        signal: i32,
    },
    /// Resolve a CONFIG_PROTECT update: `path` must be a `._cfgNNNN_name`
    /// file below /etc. Accept replaces `name` with it, otherwise it is deleted.
    ConfigUpdate {
        path: String,
        accept: bool,
    },
}

/// Validate a `._cfgNNNN_name` path below /etc and return (update, target).
pub fn config_update_paths(path: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    use std::path::{Component, Path, PathBuf};
    let p = Path::new(path);
    if !path.starts_with("/etc/") || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let name = p.file_name()?.to_str()?;
    let rest = name.strip_prefix("._cfg")?;
    let (num, target) = rest.split_once('_')?;
    if num.len() != 4 || !num.bytes().all(|b| b.is_ascii_digit()) || target.is_empty() || target.contains('/') {
        return None;
    }
    let dir = p.parent()?;
    Some((PathBuf::from(path), dir.join(target)))
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// Sent once by the helper right after start-up.
    Ready { uid: u32 },
    Started { id: u64, pid: u32 },
    /// A complete output line (ANSI escapes removed).
    Line { id: u64, stream: StreamKind, text: String },
    /// A carriage-return terminated partial line (progress meters).
    Partial { id: u64, stream: StreamKind, text: String },
    Exit { id: u64, code: Option<i32>, signal: Option<i32> },
    Ok { id: u64 },
    Err { id: u64, msg: String },
}

impl Event {
    pub fn id(&self) -> Option<u64> {
        match self {
            Event::Ready { .. } => None,
            Event::Started { id, .. }
            | Event::Line { id, .. }
            | Event::Partial { id, .. }
            | Event::Exit { id, .. }
            | Event::Ok { id }
            | Event::Err { id, .. } => Some(*id),
        }
    }
}

/// Programs the helper is willing to run as root.
pub const ALLOWED_PROGRAMS: &[&str] = &[
    "emerge",
    "emaint",
    "eselect",
    "egencache",
    "emerge-webrsync",
    "eix-update",
    "eix-sync",
    "etc-update",
    "dispatch-conf",
    "env-update",
    "revdep-rebuild",
    "glsa-check",
    "eclean",
    "eclean-dist",
    "eclean-pkg",
    "ebuild",
    "perl-cleaner",
    "smart-live-rebuild",
    "haskell-updater",
    "fixpackages",
    "quickpkg",
    "portageq",
    "eread",
];

/// Directories searched for allowed programs (never `$PATH`).
pub const PROGRAM_DIRS: &[&str] = &["/usr/bin", "/usr/sbin", "/bin", "/sbin", "/usr/local/bin"];

/// Root of the only tree the helper writes into directly.
pub const WRITE_ROOT: &str = "/etc/portage/";

/// Environment variables the helper refuses to pass on.
pub fn env_key_allowed(key: &str) -> bool {
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let banned = key.starts_with("LD_")
        || matches!(
            key,
            "PATH" | "IFS" | "BASH_ENV" | "ENV" | "PYTHONPATH" | "PYTHONSTARTUP" | "PYTHONHOME" | "PERL5LIB" | "PERL5OPT"
        );
    valid && !banned
}

/// Normalise a path lexically and check that it stays below [`WRITE_ROOT`].
pub fn path_allowed(path: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    if !path.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::from("/");
    for c in std::path::Path::new(path).components() {
        match c {
            Component::RootDir => {}
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    let s = out.to_string_lossy().to_string();
    if s.starts_with(WRITE_ROOT) && s.len() > WRITE_ROOT.len() {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(path_allowed("/etc/portage/package.use/foo").is_some());
        assert!(path_allowed("/etc/portage/../shadow").is_none());
        assert!(path_allowed("/etc/portage").is_none());
        assert!(path_allowed("/etc/passwd").is_none());
        assert!(path_allowed("etc/portage/x").is_none());
    }

    #[test]
    fn cfg_updates() {
        let (a, b) = config_update_paths("/etc/ssh/._cfg0001_sshd_config").unwrap();
        assert_eq!(a.to_str(), Some("/etc/ssh/._cfg0001_sshd_config"));
        assert_eq!(b.to_str(), Some("/etc/ssh/sshd_config"));
        assert!(config_update_paths("/etc/../root/._cfg0001_x").is_none());
        assert!(config_update_paths("/home/u/._cfg0001_x").is_none());
        assert!(config_update_paths("/etc/ssh/sshd_config").is_none());
        assert!(config_update_paths("/etc/._cfg01_x").is_none());
    }

    #[test]
    fn env() {
        assert!(env_key_allowed("FEATURES"));
        assert!(!env_key_allowed("LD_PRELOAD"));
        assert!(!env_key_allowed("PATH"));
        assert!(!env_key_allowed("lower"));
    }

    #[test]
    fn roundtrip() {
        let r = Request { id: 7, op: Op::Run { argv: vec!["emerge".into()], env: vec![], cwd: None } };
        let s = serde_json::to_string(&r).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, 7);
        assert!(matches!(back.op, Op::Run { .. }));
    }
}
