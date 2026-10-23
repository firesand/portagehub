//! PortageHub — a Steam-styled graphical front end for Gentoo's Portage.

mod app;
mod data;
mod edits;
mod exec;
mod fmt;
mod portage;
mod settings;
mod shot;
mod ui;
mod views;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Is a program available in the standard system directories?
pub fn exec_available(name: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(name) {
        return *v;
    }
    let found = portagehub::protocol::PROGRAM_DIRS.iter().any(|d| std::path::Path::new(d).join(name).is_file());
    cache.lock().unwrap().insert(name.to_string(), found);
    found
}

fn self_test() {
    let t = std::time::Instant::now();
    let repos = portage::repos::load();
    println!("repos: {} ({:?})", repos.repos.len(), t.elapsed());
    for r in &repos.repos {
        println!("  {:<20} {:<40} sync={} prio={} exists={}", r.name, r.location.display(), r.sync_type(), r.priority(), r.exists);
    }
    let t = std::time::Instant::now();
    let cat = portage::catalog::scan(&repos);
    println!("catalog: {} packages, {} ebuilds ({:?})", cat.pkgs.len(), cat.ebuild_count, t.elapsed());
    let t = std::time::Instant::now();
    let vdb = portage::vdb::load();
    println!("vdb: {} installed, world {} ({:?})", vdb.pkgs.len(), vdb.world_raw.len(), t.elapsed());
    let t = std::time::Instant::now();
    let h = portage::history::load();
    println!("history: {} merges, {} syncs, median {:?} ({:?})", h.merges.len(), h.syncs.len(), h.median_build, t.elapsed());
    let cfg = portage::config::load();
    for s in &cfg.sets {
        println!("  {:<26} exists={} dir={} files={} entries={}", s.kind.file_name(), s.exists, s.is_dir, s.files.len(), s.entry_count());
    }
    for s in &cfg.strays {
        println!("  stray: {} {:?}", s.path.display(), s.reason);
    }
    let mc = portage::makeconf::load();
    println!("make.conf: {} assignments, MAKEOPTS={}", mc.assignments.len(), mc.value("MAKEOPTS"));
    let env = portage::system::load_env();
    println!("env: ARCH={} portage={} testing={}", env.arch(), env.portage_version, env.accepts_testing());
    let masks = portage::profile::load(&repos);
    println!("masks: {} entries, profile chain:", masks.len());
    for p in &masks.chain {
        println!("  {}", p.display());
    }
    let data = data::Data {
        masks: Some(masks.into()),
        repos: Some(repos.into()),
        catalog: Some(cat.into()),
        vdb: Some(vdb.into()),
        history: Some(h.into()),
        config: Some(cfg.into()),
        env: Some(env.into()),
        ..Default::default()
    };
    let t = std::time::Instant::now();
    if let Some(d) = data::build_detail("app-editors/vim", &data) {
        println!("detail vim: {} versions, slots {:?}, flags {} ({:?})", d.versions.len(), d.slots(), d.xml.flags.len(), t.elapsed());
    }
    let t = std::time::Instant::now();
    let u = data::compute_updates(&data);
    println!("updates: {} ({:?})", u.len(), t.elapsed());
    for c in u.iter() {
        println!("  {} {} -> {} ::{}", c.key, c.installed, c.available, c.repo);
    }
}

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--self-test") {
        self_test();
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PortageHub")
            .with_app_id("portagehub")
            .with_inner_size([1380.0, 860.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native("PortageHub", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
