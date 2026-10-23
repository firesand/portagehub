//! Application state, background loading, job bookkeeping and the window shell.

use crate::data::{self, Data, DataMsg, PkgDetail, Reload};
use crate::exec::{ExecMsg, Executor, HelperState, Priv};
use crate::portage::emerge::EmergeOptions;
use crate::portage::pretend::{self, Plan};
use crate::portage::progress::{self, Tracker};
use crate::portage::{catalog, config, history, makeconf, repos, system, useflags, vdb};
use crate::settings::Settings;
use crate::ui::theme::{self, *};
use crate::ui::widgets as w;
use crate::views;
use egui::{Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke};
use egui_phosphor::regular as icon;
use portagehub::protocol::{Event, Op};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Store,
    Library,
    Emerge,
    Overlays,
    Config,
    History,
    Settings,
}

impl Tab {
    pub const MAIN: [Tab; 6] = [Tab::Store, Tab::Library, Tab::Emerge, Tab::Overlays, Tab::Config, Tab::History];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Store => "Store",
            Tab::Library => "Library",
            Tab::Emerge => "Emerge",
            Tab::Overlays => "Overlays",
            Tab::Config => "Configuration",
            Tab::History => "History",
            Tab::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub created: Instant,
}

/// A file modification to be confirmed and applied as root.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub old: String,
    /// `None` deletes the file.
    pub new: Option<String>,
}

pub enum Action {
    Open(String),
    Tab(Tab),
    Search(String),
    Pretend(EmergeOptions),
    Emerge(EmergeOptions, Vec<(String, String, bool)>),
    Root { title: String, argv: Vec<String>, reload: Vec<Reload>, kind: JobKind },
    Capture { tag: String, argv: Vec<String>, privilege: Priv },
    Batch { title: String, ops: Vec<Op>, reload: Vec<Reload>, then: Option<Box<Action>> },
    Write { title: String, changes: Vec<FileChange>, reload: Vec<Reload>, then: Option<Box<Action>> },
    Reload(Reload),
    Toast(ToastKind, String),
    Cancel(u64),
    StartHelper,
    LoadDetail(String),
    LoadEbuild(PathBuf),
    RePretend,
    CheckWorld,
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Generic,
    Pretend { opts: EmergeOptions },
    Emerge { opts: EmergeOptions, tracker: Box<Tracker> },
    Sync,
    Capture { tag: String },
    /// Background `emerge -uDN @world --pretend` for the Store's update list.
    WorldCheck,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub title: String,
    pub kind: JobKind,
    pub privilege: Priv,
    pub argv: Vec<String>,
    pub lines: Vec<(bool, String)>,
    pub partial: Option<String>,
    pub started: Instant,
    pub ended: Option<Instant>,
    pub exit: Option<Option<i32>>,
    pub error: Option<String>,
    pub reload: Vec<Reload>,
}

impl Job {
    pub fn running(&self) -> bool {
        self.ended.is_none()
    }

    pub fn success(&self) -> bool {
        self.exit == Some(Some(0)) && self.error.is_none()
    }

    pub fn output(&self) -> String {
        let mut s = String::new();
        for (_, l) in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    pub fn status_text(&self) -> String {
        if self.running() {
            "Running".into()
        } else if let Some(e) = &self.error {
            format!("Error: {e}")
        } else {
            match self.exit {
                Some(Some(0)) => "Finished".into(),
                Some(Some(c)) => format!("Exited with code {c}"),
                Some(None) => "Killed".into(),
                None => "Finished".into(),
            }
        }
    }
}

pub struct Batch {
    pub title: String,
    pub ops: VecDeque<Op>,
    pub current: Option<u64>,
    pub reload: Vec<Reload>,
    pub then: Option<Box<Action>>,
    pub done: usize,
}

#[derive(Default)]
pub struct Captured {
    pub output: String,
    pub code: Option<i32>,
    pub done: bool,
    pub job: u64,
}

pub struct ReviewDialog {
    pub title: String,
    pub changes: Vec<FileChange>,
    pub reload: Vec<Reload>,
    pub then: Option<Box<Action>>,
    pub expanded: usize,
}

pub struct App {
    pub data: Data,
    pub details: HashMap<String, Arc<PkgDetail>>,
    pub ebuilds: HashMap<PathBuf, Arc<String>>,
    pub loading: HashSet<Reload>,
    pending_reload: HashSet<Reload>,
    pub detail_loading: HashSet<String>,
    ebuild_loading: HashSet<PathBuf>,
    data_tx: Sender<DataMsg>,
    data_rx: Receiver<DataMsg>,
    exec_rx: Receiver<ExecMsg>,
    pub exec: Executor,
    pub jobs: Vec<Job>,
    pub batches: Vec<Batch>,
    pub captures: HashMap<String, Captured>,
    pub actions: Vec<Action>,
    pub toasts: Vec<Toast>,
    pub tab: Tab,
    pub settings: Settings,
    pub ctx: egui::Context,
    pub review: Option<ReviewDialog>,
    pub store: views::store::State,
    pub library: views::library::State,
    pub detail: views::details::State,
    pub emerge: views::emerge::State,
    pub overlays: views::overlays::State,
    pub config_view: views::config::State,
    pub history_view: views::history::State,
    pub settings_view: views::settings::State,
    last_log_poll: Instant,
    fonts_applied: Option<String>,
    script: Option<crate::shot::Script>,
    confirm_close: bool,
    allow_close: bool,
    /// Last `emerge -uDN @world --pretend` result and when it ran.
    pub world_plan: Option<(Arc<Plan>, i64)>,
    pub world_job: Option<u64>,
    world_auto_done: bool,
}

fn world_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("portagehub").join("world-check.txt")
}

/// First line: "<unix time> <exit code>", then the raw emerge output.
fn load_world_cache() -> Option<(Arc<Plan>, i64)> {
    let text = std::fs::read_to_string(world_cache_path()).ok()?;
    let (head, body) = text.split_once('\n')?;
    let mut it = head.split_whitespace();
    let ts: i64 = it.next()?.parse().ok()?;
    let code: Option<i32> = it.next().and_then(|c| c.parse().ok());
    Some((Arc::new(pretend::parse(body, code)), ts))
}

fn save_world_cache(ts: i64, code: Option<i32>, output: &str) {
    let p = world_cache_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, format!("{ts} {}\n{output}", code.map(|c| c.to_string()).unwrap_or_else(|| "-".into())));
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let settings = Settings::load();
        let ctx = cc.egui_ctx.clone();
        theme::install_fonts(&ctx, settings.font_family.as_deref());
        theme::apply(&ctx);
        ctx.set_zoom_factor(settings.ui_scale);
        let (data_tx, data_rx) = channel();
        let (exec_tx, exec_rx) = channel();
        let exec = Executor::new(exec_tx, ctx.clone());
        let mut app = App {
            data: Data::default(),
            details: HashMap::new(),
            ebuilds: HashMap::new(),
            loading: HashSet::new(),
            pending_reload: HashSet::new(),
            detail_loading: HashSet::new(),
            ebuild_loading: HashSet::new(),
            data_tx,
            data_rx,
            exec_rx,
            exec,
            jobs: Vec::new(),
            batches: Vec::new(),
            captures: HashMap::new(),
            actions: Vec::new(),
            toasts: Vec::new(),
            tab: Tab::Store,
            fonts_applied: settings.font_family.clone(),
            settings,
            ctx,
            review: None,
            store: Default::default(),
            library: Default::default(),
            detail: Default::default(),
            emerge: Default::default(),
            overlays: Default::default(),
            config_view: Default::default(),
            history_view: Default::default(),
            settings_view: Default::default(),
            last_log_poll: Instant::now(),
            script: crate::shot::Script::from_env(),
            confirm_close: false,
            allow_close: false,
            world_plan: load_world_cache(),
            world_job: None,
            world_auto_done: false,
        };
        for r in [Reload::Repos, Reload::Vdb, Reload::History, Reload::Config, Reload::MakeConf, Reload::Env] {
            app.spawn_load(r);
        }
        if std::env::var_os("PORTAGEHUB_DEMO").is_some() {
            app.start_demo();
        }
        app
    }

    /// Replay a synthetic parallel emerge so the progress UI can be tried
    /// without building anything (PORTAGEHUB_DEMO=1).
    fn start_demo(&mut self) {
        let pkgs = [
            ("dev-libs/libfoo-1.2.3", "gentoo", 6u64),
            ("media-libs/libbar-0.9", "gentoo", 9),
            ("app-misc/demo-app-2.0", "guru", 14),
            ("x11-misc/demo-tool-1.0_rc1", "gentoo", 5),
        ];
        let planned: Vec<(String, String, bool)> = pkgs.iter().map(|(c, r, _)| (c.to_string(), r.to_string(), false)).collect();
        let mut history = crate::portage::history::History::default();
        for (i, (c, _, secs)) in pkgs.iter().enumerate() {
            let key = crate::portage::atom::split_cpv(c).map(|(a, b, _)| format!("{a}/{b}")).unwrap();
            history.merges.push(crate::portage::history::MergeRecord { cpv: c.to_string(), key: key.clone(), ver: String::new(), start: 0, end: *secs as i64, ok: true, binary: false });
            history.by_key.entry(key).or_default().push(i);
        }
        let tracker = Box::new(Tracker::new(&planned, &history));
        let id = self.exec.alloc_id();
        let opts = EmergeOptions::default().targets(&["@demo"]);
        self.jobs.push(Job {
            id,
            title: "emerge --update @demo (demo)".into(),
            kind: JobKind::Emerge { opts, tracker },
            privilege: Priv::User,
            argv: vec!["emerge".into(), "@demo".into()],
            lines: Vec::new(),
            partial: None,
            started: Instant::now(),
            ended: None,
            exit: None,
            error: None,
            reload: vec![],
        });
        self.emerge.active_job = Some(id);
        let sink = self.exec.sink();
        std::thread::spawn(move || {
            use portagehub::protocol::StreamKind;
            let line = |t: &str| sink(Event::Line { id, stream: StreamKind::Stdout, text: t.to_string() });
            let wait = |s: f32| std::thread::sleep(Duration::from_secs_f32(s));
            line(">>> Verifying ebuild manifests");
            line(">>> Starting parallel fetch");
            let n = pkgs.len();
            line(&format!(">>> Emerging (1 of {n}) {}::{}", pkgs[0].0, pkgs[0].1));
            line(&format!(">>> Emerging (2 of {n}) {}::{}", pkgs[1].0, pkgs[1].1));
            for k in 0..8 {
                line(&format!("make[{k}]: compiling src/file{k}.c"));
                wait(0.5);
            }
            line(&format!(">>> Installing (1 of {n}) {}::{}", pkgs[0].0, pkgs[0].1));
            wait(1.0);
            line(&format!(">>> Completed (1 of {n}) {}::{}", pkgs[0].0, pkgs[0].1));
            line(&format!(">>> Jobs: 1 of {n} complete, 1 running                Load avg: 7.92, 6.10, 4.33"));
            line(&format!(">>> Emerging (3 of {n}) {}::{}", pkgs[2].0, pkgs[2].1));
            wait(3.0);
            line(" * WARNING: demo warning from the build system");
            line(&format!(">>> Installing (2 of {n}) {}::{}", pkgs[1].0, pkgs[1].1));
            wait(1.0);
            line(&format!(">>> Completed (2 of {n}) {}::{}", pkgs[1].0, pkgs[1].1));
            line(&format!(">>> Jobs: 2 of {n} complete, 1 running                Load avg: 5.20, 5.80, 4.40"));
            wait(600.0);
        });
    }

    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { kind, text: text.into(), created: Instant::now() });
        self.ctx.request_repaint();
    }

    pub fn is_loading(&self, r: Reload) -> bool {
        self.loading.contains(&r)
    }

    pub fn running_emerge(&self) -> Option<&Job> {
        self.jobs.iter().rev().find(|j| j.running() && matches!(j.kind, JobKind::Emerge { .. }))
    }

    pub fn job(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn running_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.running() && !matches!(j.kind, JobKind::Capture { .. } | JobKind::WorldCheck)).count() + self.batches.len()
    }

    // ------------------------------------------------------------------
    // Background loading

    pub fn spawn_load(&mut self, r: Reload) {
        if self.loading.contains(&r) {
            // A load started from older data is still running: redo it when it lands.
            self.pending_reload.insert(r);
            return;
        }
        let snapshot = self.data.clone();
        let tx = self.data_tx.clone();
        let ctx = self.ctx.clone();
        match r {
            Reload::Catalog | Reload::UseDesc | Reload::News | Reload::Masks if snapshot.repos.is_none() => {
                self.spawn_load(Reload::Repos);
                return;
            }
            Reload::Updates
                if snapshot.catalog.is_none()
                    || snapshot.vdb.is_none()
                    || snapshot.env.is_none()
                    || snapshot.config.is_none()
                    || snapshot.masks.is_none() =>
            {
                return;
            }
            _ => {}
        }
        self.loading.insert(r);
        std::thread::spawn(move || {
            let msg = match r {
                Reload::Repos => DataMsg::Repos(Arc::new(repos::load())),
                Reload::Catalog => DataMsg::Catalog(Arc::new(catalog::scan(snapshot.repos.as_ref().unwrap()))),
                Reload::Vdb => DataMsg::Vdb(Arc::new(vdb::load())),
                Reload::History => DataMsg::History(Arc::new(history::load())),
                Reload::Config => DataMsg::Config(Arc::new(config::load())),
                Reload::MakeConf => DataMsg::MakeConf(Arc::new(makeconf::load())),
                Reload::Env => DataMsg::Env(Arc::new(system::load_env())),
                Reload::UseDesc => {
                    let r = snapshot.repos.as_ref().unwrap();
                    let locs: Vec<&std::path::Path> = r.repos.iter().filter(|r| r.exists).map(|r| r.location.as_path()).collect();
                    DataMsg::UseDesc(Arc::new(useflags::load(&locs)))
                }
                Reload::RemoteRepos => {
                    let out = std::process::Command::new("eselect").args(["--colour=no", "repository", "list"]).output();
                    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
                    DataMsg::RemoteRepos(Arc::new(repos::parse_eselect_list(&text)))
                }
                Reload::Profiles => {
                    let out = std::process::Command::new("eselect").args(["--colour=no", "profile", "list"]).output();
                    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
                    DataMsg::Profiles(Arc::new(system::parse_profiles(&text)))
                }
                Reload::News => {
                    let r = snapshot.repos.as_ref().unwrap();
                    let mut all = Vec::new();
                    for repo in r.repos.iter().filter(|r| r.exists) {
                        all.extend(system::load_news(&repo.name, &repo.location));
                    }
                    all.sort_by(|a, b| b.posted.cmp(&a.posted));
                    DataMsg::News(Arc::new(all))
                }
                Reload::Updates => DataMsg::Updates(Arc::new(data::compute_updates(&snapshot))),
                Reload::Masks => DataMsg::Masks(Arc::new(crate::portage::profile::load(snapshot.repos.as_ref().unwrap()))),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    pub fn request_detail(&mut self, key: &str) {
        if self.details.contains_key(key) || self.detail_loading.contains(key) || self.data.catalog.is_none() {
            return;
        }
        self.detail_loading.insert(key.to_string());
        let snapshot = self.data.clone();
        let tx = self.data_tx.clone();
        let ctx = self.ctx.clone();
        let key = key.to_string();
        std::thread::spawn(move || {
            if let Some(d) = data::build_detail(&key, &snapshot) {
                let _ = tx.send(DataMsg::Detail(key, Arc::new(d)));
            } else {
                let _ = tx.send(DataMsg::Detail(
                    key.clone(),
                    Arc::new(PkgDetail {
                        key: key.clone(),
                        cat: key.split('/').next().unwrap_or("").into(),
                        pn: key.split('/').nth(1).unwrap_or("").into(),
                        versions: Vec::new(),
                        xml: Default::default(),
                        pkg_dir: None,
                        files: Vec::new(),
                    }),
                ));
            }
            ctx.request_repaint();
        });
    }

    fn invalidate_details(&mut self) {
        self.details.clear();
        self.detail_loading.clear();
    }

    fn handle_data(&mut self, msg: DataMsg) {
        let done = |s: &mut Self, r: Reload| {
            s.loading.remove(&r);
        };
        match msg {
            DataMsg::Repos(r) => {
                done(self, Reload::Repos);
                for w in &r.warnings {
                    self.toast(ToastKind::Warn, w.clone());
                }
                self.data.repos = Some(r);
                self.spawn_load(Reload::Catalog);
                self.spawn_load(Reload::UseDesc);
                self.spawn_load(Reload::News);
                self.spawn_load(Reload::Masks);
            }
            DataMsg::Catalog(c) => {
                done(self, Reload::Catalog);
                self.data.catalog = Some(c);
                self.invalidate_details();
                self.spawn_load(Reload::Updates);
            }
            DataMsg::Vdb(v) => {
                done(self, Reload::Vdb);
                self.data.vdb = Some(v);
                self.invalidate_details();
                self.spawn_load(Reload::Updates);
            }
            DataMsg::History(h) => {
                done(self, Reload::History);
                self.data.history = Some(h);
            }
            DataMsg::Config(c) => {
                done(self, Reload::Config);
                self.data.config = Some(c);
                self.invalidate_details();
                self.spawn_load(Reload::Updates);
            }
            DataMsg::MakeConf(m) => {
                done(self, Reload::MakeConf);
                self.data.makeconf = Some(m);
            }
            DataMsg::Env(e) => {
                done(self, Reload::Env);
                self.data.env = Some(e);
                self.invalidate_details();
                self.spawn_load(Reload::Updates);
            }
            DataMsg::UseDesc(u) => {
                done(self, Reload::UseDesc);
                self.data.usedesc = Some(u);
            }
            DataMsg::RemoteRepos(r) => {
                done(self, Reload::RemoteRepos);
                self.data.remote_repos = Some(r);
            }
            DataMsg::Profiles(p) => {
                done(self, Reload::Profiles);
                self.data.profiles = Some(p);
            }
            DataMsg::News(n) => {
                done(self, Reload::News);
                self.data.news = Some(n);
            }
            DataMsg::Masks(m) => {
                done(self, Reload::Masks);
                self.data.masks = Some(m);
                self.invalidate_details();
                self.spawn_load(Reload::Updates);
            }
            DataMsg::Updates(u) => {
                done(self, Reload::Updates);
                self.data.updates = Some(u);
            }
            DataMsg::Detail(k, d) => {
                self.detail_loading.remove(&k);
                self.details.insert(k, d);
            }
            DataMsg::Ebuild(p, t) => {
                self.ebuild_loading.remove(&p);
                self.ebuilds.insert(p, t);
            }
        }
        let ready: Vec<Reload> = self.pending_reload.iter().filter(|r| !self.loading.contains(r)).copied().collect();
        for r in ready {
            self.pending_reload.remove(&r);
            self.spawn_load(r);
        }
    }

    // ------------------------------------------------------------------
    // Jobs

    fn start_job(&mut self, title: String, argv: Vec<String>, env: Vec<(String, String)>, privilege: Priv, kind: JobKind, reload: Vec<Reload>) -> u64 {
        let id = self.exec.run(argv.clone(), env, privilege);
        self.jobs.push(Job {
            id,
            title,
            kind,
            privilege,
            argv,
            lines: Vec::new(),
            partial: None,
            started: Instant::now(),
            ended: None,
            exit: None,
            error: None,
            reload,
        });
        // Keep the job list bounded.
        if self.jobs.len() > 60 {
            let drop_n = self.jobs.len() - 60;
            let mut removed = 0;
            self.jobs.retain(|j| {
                if removed < drop_n && !j.running() {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        id
    }

    fn handle_exec(&mut self, msg: ExecMsg) {
        match msg {
            ExecMsg::HelperReady => {
                self.exec.on_helper_ready();
                self.toast(ToastKind::Success, "Administrator session started — privileged actions won't ask again.");
            }
            ExecMsg::HelperGone(reason) => {
                let was = self.exec.helper_state.clone();
                self.exec.on_helper_gone(reason.clone());
                if was != HelperState::NotStarted {
                    self.toast(ToastKind::Warn, format!("Administrator session ended: {reason}"));
                }
            }
            ExecMsg::Event(ev) => self.handle_event(ev),
        }
    }

    fn handle_event(&mut self, ev: Event) {
        let Some(id) = ev.id() else { return };
        // Batches first.
        if let Some(bi) = self.batches.iter().position(|b| b.current == Some(id)) {
            let outcome = match &ev {
                Event::Ok { .. } => Some(Ok(())),
                Event::Exit { code: Some(0), .. } => Some(Ok(())),
                Event::Exit { code, .. } => Some(Err(format!("command failed (exit {:?})", code))),
                Event::Err { msg, .. } => Some(Err(msg.clone())),
                _ => None,
            };
            if let Some(o) = outcome {
                self.advance_batch(bi, o);
            }
            return;
        }
        let Some(ji) = self.jobs.iter().position(|j| j.id == id) else { return };
        let history = self.data.history.clone().unwrap_or_default();
        let job = &mut self.jobs[ji];
        match ev {
            Event::Started { .. } | Event::Ready { .. } | Event::Ok { .. } => {}
            Event::Line { stream, text, .. } => {
                if let JobKind::Emerge { tracker, .. } = &mut job.kind {
                    tracker.feed(&text, &history);
                }
                job.partial = None;
                job.lines.push((stream == portagehub::protocol::StreamKind::Stderr, text));
                if job.lines.len() > 60_000 {
                    job.lines.drain(..10_000);
                }
            }
            Event::Partial { text, .. } => {
                if let JobKind::Emerge { tracker, .. } = &mut job.kind {
                    tracker.feed(&text, &history);
                }
                job.partial = Some(text);
            }
            Event::Err { msg, .. } => {
                job.error = Some(msg);
                job.ended = Some(Instant::now());
                self.finish_job(ji);
            }
            Event::Exit { code, .. } => {
                job.exit = Some(code);
                job.ended = Some(Instant::now());
                self.finish_job(ji);
            }
        }
    }

    fn finish_job(&mut self, ji: usize) {
        let job = self.jobs[ji].clone();
        let ok = job.success();
        for r in &job.reload {
            self.spawn_load(*r);
        }
        match &job.kind {
            JobKind::Pretend { opts } => {
                let mut plan: Plan = pretend::parse(&job.output(), job.exit.flatten());
                if let Some(e) = &job.error {
                    plan.messages.push(e.clone());
                }
                if self.emerge.pretend_job == Some(job.id) {
                    self.emerge.plan = Some(plan);
                    self.emerge.plan_opts = Some(opts.clone());
                }
            }
            JobKind::Emerge { .. } => {
                if let JobKind::Emerge { tracker, .. } = &mut self.jobs[ji].kind {
                    tracker.finish(ok);
                }
                if ok {
                    self.toast(ToastKind::Success, format!("{} finished successfully", job.title));
                } else {
                    self.toast(ToastKind::Error, format!("{} failed — see the log", job.title));
                }
                for r in [Reload::Vdb, Reload::History, Reload::Config] {
                    self.spawn_load(r);
                }
                if self.settings.auto_world_check {
                    self.actions.push(Action::CheckWorld);
                }
                let out = job.output();
                if out.contains("needs updating") {
                    self.toast(ToastKind::Warn, "Some configuration files need updating (Configuration → Config updates).");
                }
                if out.contains("@preserved-rebuild") {
                    self.toast(ToastKind::Warn, "Preserved libraries detected: run “Rebuild preserved libraries”.");
                }
            }
            JobKind::Sync => {
                if ok {
                    self.toast(ToastKind::Success, format!("{} completed", job.title));
                } else {
                    self.toast(ToastKind::Error, format!("{} failed", job.title));
                }
                for r in [Reload::Repos, Reload::History] {
                    self.spawn_load(r);
                }
                if ok && self.settings.auto_world_check {
                    self.actions.push(Action::CheckWorld);
                }
            }
            JobKind::WorldCheck => {
                if self.world_job == Some(job.id) {
                    self.world_job = None;
                }
                if job.error.is_none() && job.exit.flatten().is_some() {
                    let out = job.output();
                    let code = job.exit.flatten();
                    let ts = crate::fmt::now_ts();
                    self.world_plan = Some((Arc::new(pretend::parse(&out, code)), ts));
                    save_world_cache(ts, code, &out);
                }
            }
            JobKind::Capture { tag } => {
                let c = self.captures.entry(tag.clone()).or_default();
                c.output = job.output();
                c.code = job.exit.flatten();
                c.done = true;
            }
            JobKind::Generic => {
                if ok {
                    self.toast(ToastKind::Success, format!("{}: done", job.title));
                } else {
                    let tail = job.lines.iter().rev().take(3).map(|l| l.1.clone()).collect::<Vec<_>>();
                    let reason = job.error.clone().unwrap_or_else(|| tail.into_iter().rev().collect::<Vec<_>>().join(" "));
                    self.toast(ToastKind::Error, format!("{} failed: {}", job.title, reason));
                }
            }
        }
    }

    fn advance_batch(&mut self, bi: usize, outcome: Result<(), String>) {
        match outcome {
            Err(e) => {
                let b = self.batches.remove(bi);
                self.toast(ToastKind::Error, format!("{} failed: {e}", b.title));
                for r in b.reload {
                    self.spawn_load(r);
                }
            }
            Ok(()) => {
                self.batches[bi].done += 1;
                if let Some(op) = self.batches[bi].ops.pop_front() {
                    let id = self.exec.root_op(op);
                    self.batches[bi].current = Some(id);
                } else {
                    let b = self.batches.remove(bi);
                    self.toast(ToastKind::Success, format!("{} — saved", b.title));
                    for r in b.reload {
                        self.spawn_load(r);
                    }
                    if let Some(then) = b.then {
                        self.actions.push(*then);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Actions

    fn process_actions(&mut self) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            self.apply(a);
        }
    }

    fn apply(&mut self, a: Action) {
        match a {
            Action::Open(key) => {
                self.request_detail(&key);
                self.detail.open(&key);
                if self.tab != Tab::Library {
                    self.tab = Tab::Store;
                    self.store.show_detail = true;
                } else {
                    self.library.selected = Some(key);
                }
            }
            Action::Tab(t) => self.tab = t,
            Action::Search(q) => {
                self.tab = Tab::Store;
                self.store.query = q;
                self.store.show_detail = false;
                self.store.page = views::store::Page::Search;
            }
            Action::Pretend(opts) => {
                let mut env = opts.env.clone();
                env.retain(|(_, v)| !v.is_empty());
                let mut argv = vec!["emerge".to_string()];
                argv.extend(opts.pretend_args());
                let privilege = if self.settings.pretend_as_root { Priv::Root } else { Priv::User };
                let title = format!("Preview: {}", opts.command_line());
                let id = self.start_job(title, argv, env, privilege, JobKind::Pretend { opts: opts.clone() }, vec![]);
                self.emerge.pretend_job = Some(id);
                self.emerge.plan = None;
                self.emerge.plan_opts = None;
                self.emerge.options = opts;
                self.emerge.page = views::emerge::Page::Review;
                self.tab = Tab::Emerge;
            }
            Action::RePretend => {
                if let Some(o) = self.emerge.plan_opts.clone().or_else(|| Some(self.emerge.options.clone())) {
                    self.apply(Action::Pretend(o));
                }
            }
            Action::Emerge(opts, planned) => {
                if self.running_emerge().is_some() {
                    self.toast(ToastKind::Warn, "Another emerge is already running. Wait for it to finish.");
                    return;
                }
                let history = self.data.history.clone().unwrap_or_default();
                let tracker = Box::new(Tracker::new(&planned, &history));
                let mut argv = vec!["emerge".to_string()];
                argv.extend(opts.run_args());
                let mut env = opts.env.clone();
                env.retain(|(_, v)| !v.is_empty());
                let title = format!("emerge {}", opts.args().join(" "));
                let kind = if opts.action == crate::portage::emerge::Action::Sync { JobKind::Sync } else { JobKind::Emerge { opts: opts.clone(), tracker } };
                let id = self.start_job(title, argv, env, Priv::Root, kind, vec![]);
                self.emerge.active_job = Some(id);
                self.emerge.page = views::emerge::Page::Activity;
                self.tab = Tab::Emerge;
            }
            Action::Root { title, argv, reload, kind } => {
                let id = self.start_job(title, argv, vec![], Priv::Root, kind, reload);
                self.emerge.selected_job = Some(id);
            }
            Action::Capture { tag, argv, privilege } => {
                let id = self.start_job(format!("{}", argv.join(" ")), argv, vec![], privilege, JobKind::Capture { tag: tag.clone() }, vec![]);
                self.captures.insert(tag, Captured { job: id, ..Default::default() });
            }
            Action::Batch { title, ops, reload, then } => {
                let mut ops: VecDeque<Op> = ops.into();
                if let Some(first) = ops.pop_front() {
                    let id = self.exec.root_op(first);
                    self.batches.push(Batch { title, ops, current: Some(id), reload, then, done: 0 });
                }
            }
            Action::Write { title, changes, reload, then } => {
                if changes.is_empty() {
                    self.toast(ToastKind::Info, "Nothing to change.");
                    return;
                }
                if self.settings.review_writes {
                    self.review = Some(ReviewDialog { title, changes, reload, then, expanded: 0 });
                } else {
                    self.apply_changes(title, changes, reload, then);
                }
            }
            Action::Reload(r) => self.spawn_load(r),
            Action::Toast(k, t) => self.toast(k, t),
            Action::Cancel(id) => {
                if let Some(j) = self.jobs.iter().find(|j| j.id == id) {
                    let p = j.privilege;
                    self.exec.cancel(id, p, libc::SIGINT);
                    self.toast(ToastKind::Info, "Stop requested (SIGINT). emerge will stop after cleaning up.");
                }
            }
            Action::StartHelper => {
                if !self.exec.root_ready() && self.exec.helper_state != HelperState::Starting {
                    self.exec.start_helper();
                }
            }
            Action::CheckWorld => {
                if self.world_job.is_some_and(|id| self.job(id).is_some_and(|j| j.running())) {
                    return;
                }
                let opts = (crate::portage::emerge::recipes()[0].build)();
                // Low priority: it only reads, and can take a while on big systems.
                let mut argv = vec!["nice".to_string(), "-n".into(), "19".into(), "emerge".into()];
                argv.extend(opts.pretend_args());
                let id = self.start_job("Checking @world updates (emerge --pretend)".into(), argv, vec![], Priv::User, JobKind::WorldCheck, vec![]);
                self.world_job = Some(id);
            }
            Action::LoadDetail(k) => self.request_detail(&k),
            Action::LoadEbuild(p) => {
                if !self.ebuilds.contains_key(&p) && self.ebuild_loading.insert(p.clone()) {
                    let tx = self.data_tx.clone();
                    let ctx = self.ctx.clone();
                    std::thread::spawn(move || {
                        let t = std::fs::read_to_string(&p).unwrap_or_else(|e| format!("# cannot read {}: {e}", p.display()));
                        let _ = tx.send(DataMsg::Ebuild(p, Arc::new(t)));
                        ctx.request_repaint();
                    });
                }
            }
        }
    }

    pub fn apply_changes(&mut self, title: String, changes: Vec<FileChange>, reload: Vec<Reload>, then: Option<Box<Action>>) {
        let backup = self.settings.backup_on_write;
        let ops = changes
            .into_iter()
            .map(|c| match c.new {
                Some(content) => Op::WriteFile { path: c.path.to_string_lossy().to_string(), content, backup },
                None => Op::DeleteFile { path: c.path.to_string_lossy().to_string() },
            })
            .collect();
        self.apply(Action::Batch { title, ops, reload, then });
    }

    /// PORTAGE_TMPDIR for a package, honouring package.env overrides (e.g. a "notmpfs" env file).
    pub fn tmpdir_for(&self, cpv: &str) -> PathBuf {
        use crate::portage::config::ConfKind;
        let mut dir = self.data.env.as_ref().map(|e| e.get("PORTAGE_TMPDIR").to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "/var/tmp".into());
        if let (Some(cfg), Some((c, p, v))) = (self.data.config.as_ref(), crate::portage::atom::split_cpv(cpv))
            && let Some(set) = cfg.set(ConfKind::Env)
        {
            for f in set.files.iter().filter(|f| !f.ignored) {
                for (_, e) in f.entries() {
                    if e.disabled || !e.atom.as_ref().is_some_and(|a| a.matches(&c, &p, &v, None, None)) {
                        continue;
                    }
                    for name in &e.values {
                        if let Some(envf) = cfg.env_files.iter().find(|x| x.name == *name) {
                            for (k, val, _) in makeconf::scan(&envf.content) {
                                if k == "PORTAGE_TMPDIR" && !val.is_empty() {
                                    dir = val;
                                }
                            }
                        }
                    }
                }
            }
        }
        PathBuf::from(dir)
    }

    /// Tail build logs of running packages to show their phase.
    fn poll_build_logs(&mut self) {
        if self.last_log_poll.elapsed() < Duration::from_millis(1500) {
            return;
        }
        self.last_log_poll = Instant::now();
        // Resolve each active package's build dir first (PORTAGE_TMPDIR can differ per package).
        let mut dirs: HashMap<String, PathBuf> = HashMap::new();
        for j in self.jobs.iter().filter(|j| j.running()) {
            if let JobKind::Emerge { tracker, .. } = &j.kind {
                for i in tracker.items.iter().filter(|i| i.stage.active()) {
                    dirs.insert(i.cpv.clone(), self.tmpdir_for(&i.cpv));
                }
            }
        }
        for j in self.jobs.iter_mut().filter(|j| j.running()) {
            if let JobKind::Emerge { tracker, .. } = &mut j.kind {
                let active: Vec<(usize, PathBuf)> = tracker
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.stage.active())
                    .filter_map(|(n, i)| dirs.get(&i.cpv).and_then(|d| i.build_dir_log(d)).map(|p| (n, p)))
                    .collect();
                for (n, p) in active {
                    if let Some(t) = progress::tail(&p, 16 * 1024) {
                        tracker.set_phase_from_log(n, &t);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Shell

    fn nav_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 14.0, Color32::from_rgb(0x2a, 0x47, 0x5e));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon::CUBE, egui::FontId::proportional(18.0), ACCENT);
            ui.label(RichText::new("PORTAGE").size(17.0).family(theme::bold()).color(WHITE));
            ui.label(RichText::new("HUB").size(17.0).family(theme::light()).color(ACCENT));
            ui.add_space(24.0);
            for t in Tab::MAIN {
                let mut label = t.label().to_string();
                if t == Tab::Emerge && self.running_emerge().is_some() {
                    label = format!("{} {}", t.label(), icon::CIRCLE_NOTCH);
                }
                if w::nav_tab(ui, &label, self.tab == t).clicked() {
                    if self.tab == t && t == Tab::Store {
                        self.store.show_detail = false;
                    }
                    self.tab = t;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if w::icon_button(ui, icon::GEAR_SIX, "Settings").clicked() {
                    self.tab = Tab::Settings;
                }
                ui.add_space(4.0);
                self.root_pill(ui);
                ui.add_space(8.0);
                if let Some(env) = &self.data.env
                    && !env.portage_version.is_empty()
                {
                    ui.label(RichText::new(format!("Portage {}", env.portage_version)).size(12.0).color(TEXT_FAINT));
                }
            });
        });
    }

    fn root_pill(&mut self, ui: &mut egui::Ui) {
        let (txt, col, tip) = if self.exec.is_root {
            (format!("{} root", icon::SHIELD_CHECK), GREEN, "PortageHub itself runs as root.".to_string())
        } else {
            match &self.exec.helper_state {
                HelperState::Ready => (format!("{} Admin session", icon::LOCK_OPEN), GREEN, "Privileged actions run through the helper without asking again.".into()),
                HelperState::Starting => (format!("{} Authenticating…", icon::HOURGLASS), YELLOW, "Waiting for the polkit password dialog.".into()),
                HelperState::Failed(e) => (format!("{} Not authorized", icon::LOCK), RED, format!("Last attempt failed: {e}\nClick to retry.")),
                HelperState::NotStarted => (format!("{} User mode", icon::LOCK), TEXT_DIM, "Browsing works without root. Click to unlock privileged actions (one password prompt per session).".into()),
            }
        };
        let r = ui.add(egui::Button::new(RichText::new(txt).color(col).size(13.0)).fill(Color32::from_rgb(0x22, 0x28, 0x33)).corner_radius(CornerRadius::same(12))).on_hover_text(tip);
        if r.clicked() {
            self.push(Action::StartHelper);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            let busy: Vec<&str> = [
                (Reload::Repos, "repositories"),
                (Reload::Catalog, "package tree"),
                (Reload::Vdb, "installed packages"),
                (Reload::History, "history"),
                (Reload::Updates, "update check"),
            ]
            .iter()
            .filter(|(r, _)| self.loading.contains(r))
            .map(|(_, n)| *n)
            .collect();
            if !busy.is_empty() {
                ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                ui.label(w::dim(format!("Loading {}…", busy.join(", "))));
            } else if let Some(c) = &self.data.catalog {
                ui.label(w::dim(format!(
                    "{} {} packages · {} ebuilds in {} repos",
                    icon::DATABASE,
                    c.pkgs.len(),
                    c.ebuild_count,
                    c.repo_locations.len()
                )));
                if let Some(v) = &self.data.vdb {
                    ui.label(w::dim(format!("· {} installed", v.pkgs.len())));
                }
                if let Some(ts) = self.data.last_sync() {
                    ui.label(w::dim(format!("· synced {}", crate::fmt::ago(ts))));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                let running = self.running_count();
                if running > 0 {
                    ui.label(RichText::new(format!("{} {} active", icon::CIRCLE_NOTCH, running)).color(ACCENT));
                }
                let emerge = self.running_emerge().map(|j| match &j.kind {
                    JobKind::Emerge { tracker, .. } => Some((tracker.fraction(), tracker.counts(), tracker.total, tracker.remaining(), tracker.elapsed(), tracker.last_status.clone())),
                    _ => None,
                });
                if let Some(Some((frac, (done, _failed, _active), total, rem, el, status))) = emerge {
                    let resp = ui
                        .allocate_ui(egui::vec2(380.0, 22.0), |ui| {
                            let text = format!("{done}/{total} · {} elapsed · ~{} left", history::fmt_duration(el), history::fmt_duration(rem));
                            w::progress(ui, frac, 18.0, &text, true)
                        })
                        .inner;
                    if resp.interact(egui::Sense::click()).on_hover_text(status).clicked() {
                        self.tab = Tab::Emerge;
                        self.emerge.page = views::emerge::Page::Activity;
                    }
                    ui.label(RichText::new("EMERGING").size(11.0).family(theme::bold()).color(ACCENT));
                }
            });
        });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.created.elapsed() < Duration::from_secs(if t.kind == ToastKind::Error { 12 } else { 6 }));
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("toasts"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.right() - 400.0, screen.bottom() - 60.0 - 70.0 * self.toasts.len().min(5) as f32))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                let mut close = None;
                for (i, t) in self.toasts.iter().enumerate().rev().take(5) {
                    let (col, ic) = match t.kind {
                        ToastKind::Info => (ACCENT, icon::INFO),
                        ToastKind::Success => (GREEN, icon::CHECK_CIRCLE),
                        ToastKind::Warn => (ORANGE, icon::WARNING),
                        ToastKind::Error => (RED, icon::X_CIRCLE),
                    };
                    Frame::new()
                        .fill(Color32::from_rgb(0x23, 0x31, 0x44))
                        .stroke(Stroke::new(1.0, col.gamma_multiply(0.6)))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::same(10))
                        .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(120) })
                        .show(ui, |ui| {
                            ui.set_width(360.0);
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(ic).size(18.0).color(col));
                                ui.allocate_ui(egui::vec2(300.0, 0.0), |ui| {
                                    ui.add(egui::Label::new(RichText::new(&t.text).color(TEXT)).wrap());
                                });
                                if ui.add(egui::Button::new(RichText::new(icon::X).color(TEXT_DIM)).frame(false)).clicked() {
                                    close = Some(i);
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
                if let Some(i) = close {
                    self.toasts.remove(i);
                }
            });
    }

    /// Ask before quitting while an emerge is running.
    fn close_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.running_emerge().is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        if !self.confirm_close {
            return;
        }
        let in_process = self.exec.is_root;
        egui::Modal::new(egui::Id::new("close-guard"))
            .frame(Frame::new().fill(PANEL).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(18)))
            .show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::WARNING).size(22.0).color(ORANGE));
                    w::title(ui, "An emerge is still running");
                });
                ui.add(egui::Label::new(RichText::new(if in_process {
                    "PortageHub runs as root, so quitting now stops the emerge. You can continue it later with “Resume interrupted emerge”."
                } else {
                    "If you quit, the emerge keeps running in the background (the admin helper stays alive until it finishes), but you won't see its progress here. emerge.log and History will still record it."
                }).color(TEXT)).wrap());
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if w::primary(ui, icon::ARROW_LEFT, "Keep PortageHub open").clicked() {
                        self.confirm_close = false;
                    }
                    if w::danger(ui, icon::SIGN_OUT, "Quit anyway").clicked() {
                        self.allow_close = true;
                        self.confirm_close = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
    }

    fn review_ui(&mut self, ctx: &egui::Context) {
        let Some(review) = &mut self.review else { return };
        let mut apply = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("review-dialog"))
            .frame(Frame::new().fill(PANEL).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(18)))
            .show(ctx, |ui| {
                ui.set_width(760.0f32.min(ctx.content_rect().width() - 80.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::FLOPPY_DISK).size(22.0).color(ACCENT));
                    w::title(ui, "Review changes");
                });
                w::subtitle(ui, &review.title);
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.6).show(ui, |ui| {
                    for (i, c) in review.changes.iter().enumerate() {
                        let header = match &c.new {
                            None => format!("{} delete {}", icon::TRASH, c.path.display()),
                            Some(_) if c.old.is_empty() => format!("{} create {}", icon::FILE_PLUS, c.path.display()),
                            Some(_) => format!("{} modify {}", icon::PENCIL_SIMPLE, c.path.display()),
                        };
                        let open = review.expanded == i;
                        if ui.selectable_label(open, RichText::new(header).color(WHITE)).clicked() {
                            review.expanded = i;
                        }
                        if open {
                            Frame::new().fill(BG_DEEP).inner_margin(Margin::same(8)).corner_radius(CornerRadius::same(3)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let new = c.new.clone().unwrap_or_default();
                                for d in crate::fmt::line_diff(&c.old, &new) {
                                    let (prefix, col) = match d.0 {
                                        '+' => ("+ ", GREEN),
                                        '-' => ("- ", RED),
                                        _ => ("  ", TEXT_DIM),
                                    };
                                    ui.label(RichText::new(format!("{prefix}{}", d.1)).monospace().color(col));
                                }
                            });
                        }
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if w::big_button(ui, &format!("{}  APPLY", icon::CHECK), GRAD_GREEN, 140.0).clicked() {
                        apply = true;
                    }
                    if w::button(ui, "", "Cancel").clicked() {
                        cancel = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(w::dim(if self.exec.root_ready() { "Runs as root through the admin session" } else { "You will be asked for your password once" }));
                    });
                });
            });
        if apply {
            let r = self.review.take().unwrap();
            self.apply_changes(r.title, r.changes, r.reload, r.then);
        } else if cancel {
            self.review = None;
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.data_rx.try_recv() {
            self.handle_data(m);
        }
        while let Ok(m) = self.exec_rx.try_recv() {
            self.handle_exec(m);
        }
        self.poll_build_logs();
        if !self.world_auto_done && self.data.catalog.is_some() && self.data.vdb.is_some() && self.data.repos.is_some() {
            self.world_auto_done = true;
            let last_sync = self.data.last_sync().unwrap_or(0);
            let stale = self.world_plan.as_ref().is_none_or(|(_, ts)| *ts < last_sync || crate::fmt::now_ts() - ts > 6 * 3600);
            if self.settings.auto_world_check && stale && std::env::var_os("PORTAGEHUB_SCRIPT").is_none() {
                self.actions.push(Action::CheckWorld);
            }
        }
        self.process_actions();
        if self.running_emerge().is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.fonts_applied != self.settings.font_family {
            theme::install_fonts(&ctx, self.settings.font_family.as_deref());
            self.fonts_applied = self.settings.font_family.clone();
        }
        // F5 reloads everything that's cheap to reload.
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            for r in [Reload::Repos, Reload::Vdb, Reload::Config, Reload::MakeConf, Reload::History] {
                self.spawn_load(r);
            }
        }
        egui::Panel::top("nav")
            .frame(Frame::new().fill(NAV).inner_margin(Margin::symmetric(8, 6)))
            .show_separator_line(false)
            .show(ui, |ui| self.nav_bar(ui));
        egui::Panel::bottom("status")
            .frame(Frame::new().fill(NAV).inner_margin(Margin::symmetric(10, 3)))
            .show_separator_line(false)
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| match self.tab {
            Tab::Store => views::store::show(self, ui),
            Tab::Library => views::library::show(self, ui),
            Tab::Emerge => views::emerge::show(self, ui),
            Tab::Overlays => views::overlays::show(self, ui),
            Tab::Config => views::config::show(self, ui),
            Tab::History => views::history::show(self, ui),
            Tab::Settings => views::settings::show(self, ui),
        });
        self.review_ui(&ctx);
        self.close_guard(&ctx);
        self.toasts_ui(&ctx);
        self.process_actions();
        if let Some(mut s) = self.script.take() {
            crate::shot::run(self, &ctx, &mut s);
            self.script = Some(s);
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.settings.save();
    }

    fn on_exit(&mut self) {
        self.settings.save();
        self.exec.stop_helper();
    }
}
