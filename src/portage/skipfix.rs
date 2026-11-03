//! Why an update was skipped, and the keyword changes that would let it through.
//!
//! emerge reports "One or more updates/rebuilds have been skipped due to a
//! dependency conflict" with every installed package that names the old
//! version. Most of those rows only bind the old sub-slot, and emerge rebuilds
//! those packages itself. The rows that matter pin the old version, usually
//! packages released in lockstep (all of Qt, KDE Frameworks, LLVM and clang).
//! When a version of such a package that accepts the new one exists but the
//! user's keywords don't allow it, the update stays skipped until they agree.

use super::atom::{Atom, split_cpv};
use super::config::{ConfKind, Entry, PortageConfig};
use super::dep;
use super::pretend::{ReqKind, Requirement, classify_requirements};
use super::version::Version;
use crate::data::{KwState, VersionInfo};
use std::path::{Path, PathBuf};

/// The update emerge skipped.
#[derive(Debug, Clone)]
pub struct Target {
    pub cat: String,
    pub pn: String,
    pub ver: Version,
    pub slot: String,
}

impl Target {
    pub fn parse(slot_key: &str, candidate: &str) -> Option<Target> {
        let (cat, pn, ver) = split_cpv(candidate)?;
        let slot = slot_key.split_once(':').map(|(_, s)| s.to_string()).unwrap_or_else(|| "0".into());
        Some(Target { cat, pn, ver, slot })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Its version that accepts the update is a keyword away (`~arch`, or `**`
    /// when it has none). `newer`: a newer unmasked version exists, so the new
    /// line must not be unversioned.
    Keyword { version: String, kw: KwState, newer: bool },
    /// That version is masked, or keyworded broken for this architecture.
    Blocked { version: String, why: String },
    /// No version of it works with the update yet.
    NoVersion,
    /// Not in any repository PortageHub has loaded.
    Unknown,
    /// That version is already allowed: it moves along with the update.
    Follows(String),
}

impl Outcome {
    fn rank(&self) -> u8 {
        match self {
            Outcome::Keyword { .. } => 0,
            Outcome::Blocked { .. } => 1,
            Outcome::NoVersion => 2,
            Outcome::Unknown => 3,
            Outcome::Follows(_) => 4,
        }
    }
}

/// A package whose dependency excludes the new version.
#[derive(Debug, Clone)]
pub struct Pinned {
    pub key: String,
    /// Installed version, from emerge's text.
    pub installed: String,
    /// The atoms it pins with, as emerge printed them.
    pub atoms: Vec<String>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub struct SkipReport {
    pub slot_key: String,
    pub candidate: String,
    pub reqs: Vec<Requirement>,
    /// Keyword causes first, packages that follow by themselves last.
    pub pinned: Vec<Pinned>,
}

/// A change to package.accept_keywords that lets one pinned package follow.
// A report holds a handful of these; boxing the entry would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum KeywordFix {
    /// Rewrite a version-pinned line (`=dev-python/pyside-6.11.2 ~amd64`).
    Replace { path: PathBuf, idx: usize, old: String, new: Entry },
    /// Add a line, in the file that already accepts the update when there is one.
    Add { path: PathBuf, line: String },
}

impl SkipReport {
    pub fn causes(&self) -> impl Iterator<Item = &Pinned> {
        self.pinned.iter().filter(|p| matches!(p.outcome, Outcome::Keyword { .. }))
    }

    /// Requiring packages that only need a rebuild against the new sub-slot.
    pub fn rebuilds(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for r in self.reqs.iter().filter(|r| r.kind == ReqKind::Rebuild) {
            let who = r.requirer.as_deref().unwrap_or(&r.by);
            if !out.contains(&who) {
                out.push(who);
            }
        }
        out
    }

    /// The keyword lines to change. `exact` pins every cause to its version
    /// (`~cat/pkg-1.2`) instead of an unversioned line; `default_path` is
    /// where new lines go when no keyword file mentions the update itself.
    pub fn fixes(&self, cfg: &PortageConfig, arch: &str, exact: bool, default_path: &Path) -> Vec<(String, KeywordFix)> {
        let Some(t) = Target::parse(&self.slot_key, &self.candidate) else { return Vec::new() };
        // New lines go next to the update's own keyword line, so the packages
        // that move together stay together in one file.
        let home = keyword_entries(cfg, &[ConfKind::AcceptKeywords])
            .find(|(_, _, e)| e.atom.as_ref().is_some_and(|a| a.matches(&t.cat, &t.pn, &t.ver, Some(&t.slot), None)))
            .map(|(p, _, _)| p);
        let mut out = Vec::new();
        for p in self.causes() {
            let Outcome::Keyword { version, kw, newer } = &p.outcome else { continue };
            let (Some((cat, pn)), Some(ver), Some(installed)) = (p.key.split_once('/'), Version::parse(version), Version::parse(&p.installed)) else {
                continue;
            };
            let token = if *kw == KwState::Missing { "**".to_string() } else { format!("~{arch}") };
            let atom = if *kw == KwState::Missing {
                format!("={}-{version}", p.key)
            } else if exact || *newer {
                format!("~{}-{}", p.key, ver.without_revision())
            } else {
                p.key.clone()
            };
            // A version-pinned line that allows the installed version but not the needed one.
            let pinned = keyword_entries(cfg, &[ConfKind::AcceptKeywords, ConfKind::Keywords]).find(|(_, _, e)| {
                e.atom.as_ref().is_some_and(|a| a.ver.is_some() && a.matches(cat, pn, &installed, None, None) && !a.matches(cat, pn, &ver, None, None))
            });
            let fix = match pinned {
                Some((path, idx, e)) => {
                    let mut values = e.values.clone();
                    let implied = values.is_empty() && token.starts_with('~');
                    if !implied && !values.contains(&token) {
                        values.push(token);
                    }
                    let new = Entry { atom_text: atom.clone(), atom: Atom::parse(&atom), values, comment: e.comment.clone(), disabled: false };
                    KeywordFix::Replace { path, idx, old: e.render(), new }
                }
                None => KeywordFix::Add { path: home.clone().unwrap_or_else(|| default_path.to_path_buf()), line: format!("{atom} {token}") },
            };
            out.push((p.key.clone(), fix));
        }
        out
    }
}

/// Enabled entries of the given keyword files: (file, line index, entry).
fn keyword_entries<'a>(cfg: &'a PortageConfig, kinds: &'a [ConfKind]) -> impl Iterator<Item = (PathBuf, usize, &'a Entry)> + 'a {
    kinds
        .iter()
        .filter_map(|k| cfg.set(*k))
        .flat_map(|s| s.files.iter().filter(|f| !f.ignored))
        .flat_map(|f| f.entries().filter(|(_, e)| !e.disabled).map(move |(i, e)| (f.path.clone(), i, e)))
}

/// Build the report. `versions` returns every version of a package (`cat/pn`),
/// newest first, as the package page shows them; `testing` is whether the
/// global ACCEPT_KEYWORDS takes ~arch.
pub fn analyze(
    slot_key: &str,
    candidate: &str,
    required: &[(String, String)],
    testing: bool,
    versions: impl Fn(&str) -> Option<Vec<VersionInfo>>,
) -> SkipReport {
    let reqs = classify_requirements(slot_key, candidate, required);
    let target = Target::parse(slot_key, candidate);
    let mut pinned: Vec<Pinned> = Vec::new();
    for r in reqs.iter().filter(|r| r.kind == ReqKind::Pin) {
        let Some((cat, pn, installed)) = r.requirer.as_deref().and_then(split_cpv) else { continue };
        let key = format!("{cat}/{pn}");
        if let Some(p) = pinned.iter_mut().find(|p| p.key == key) {
            if !p.atoms.contains(&r.atom) {
                p.atoms.push(r.atom.clone());
            }
            continue;
        }
        let outcome = match (&target, versions(&key)) {
            (Some(t), Some(vs)) => outcome(t, &installed, &vs, testing, None),
            _ => Outcome::Unknown,
        };
        pinned.push(Pinned { key, installed: installed.raw, atoms: vec![r.atom.clone()], outcome });
    }
    pinned.sort_by(|a, b| (a.outcome.rank(), &a.key).cmp(&(b.outcome.rank(), &b.key)));
    SkipReport { slot_key: slot_key.to_string(), candidate: candidate.to_string(), reqs, pinned }
}

/// The same mismatch as emerge reports it without `--complete-graph`: the new
/// version (`blocked`, e.g. `dev-qt/qtbase-6.11.3`) soft-blocks older versions
/// of its siblings (`<dev-qt/qtgraphs-6.11.3:6`) that emerge cannot replace.
/// `pulled_in` is emerge's list of what keeps each of them installed; packages
/// pinning a sibling's old version there have to move as well (pyside's
/// `=dev-qt/qtwebchannel-6.11.2*`).
pub fn analyze_blocked(
    blocked: &str,
    slot: &str,
    blockers: &[String],
    pulled_in: &[(String, Vec<(String, String)>)],
    testing: bool,
    versions: impl Fn(&str) -> Option<Vec<VersionInfo>>,
) -> SkipReport {
    let own_key = split_cpv(blocked).map(|(c, p, _)| format!("{c}/{p}")).unwrap_or_default();
    let slot_key = format!("{own_key}:{slot}");
    let target = Target::parse(&slot_key, blocked);
    let key_of = |cpv: &str| split_cpv(cpv).map(|(c, p, _)| format!("{c}/{p}"));
    let mut pinned: Vec<Pinned> = Vec::new();
    // Packages that move to a new version: (key, version, slot).
    let mut moving: Vec<(String, String, String)> = Vec::new();
    let moves = |p: &Pinned, vs: &[VersionInfo]| match &p.outcome {
        Outcome::Keyword { version, .. } | Outcome::Follows(version) => {
            let slot = vs.iter().find(|v| v.ver.raw == *version).map(|v| v.meta.slot().split('/').next().unwrap_or("0").to_string());
            Some((p.key.clone(), version.clone(), slot.unwrap_or_else(|| "0".into())))
        }
        _ => None,
    };
    for b in blockers {
        let Some(a) = Atom::parse(b) else { continue };
        let key = a.key();
        if key == own_key || pinned.iter().any(|p| p.key == key) {
            continue;
        }
        let vs = versions(&key).unwrap_or_default();
        let installed = vs
            .iter()
            .find(|v| v.installed && a.matches(&a.cat, &a.pn, &v.ver, None, None))
            .map(|v| v.ver.clone())
            .or_else(|| pulled_in.iter().filter_map(|(c, _)| split_cpv(c)).find(|(c, p, _)| *c == a.cat && *p == a.pn).map(|(_, _, v)| v));
        let Some(installed) = installed else { continue };
        let outcome = match &target {
            Some(t) if !vs.is_empty() => outcome(t, &installed, &vs, testing, Some(&a)),
            _ => Outcome::Unknown,
        };
        let p = Pinned { key, installed: installed.raw, atoms: vec![b.clone()], outcome };
        moving.extend(moves(&p, &vs));
        pinned.push(p);
    }
    let mut i = 0;
    while i < moving.len() {
        let (key, version, slot) = moving[i].clone();
        i += 1;
        let Some((_, reqs)) = pulled_in.iter().find(|(c, _)| key_of(c).as_deref() == Some(key.as_str())) else { continue };
        let (sk, cand) = (format!("{key}:{slot}"), format!("{key}-{version}"));
        let Some(t) = Target::parse(&sk, &cand) else { continue };
        for r in classify_requirements(&sk, &cand, reqs).into_iter().filter(|r| r.kind == ReqKind::Pin) {
            let Some((c, pn, installed)) = r.requirer.as_deref().and_then(split_cpv) else { continue };
            let rk = format!("{c}/{pn}");
            if rk == own_key {
                continue;
            }
            if let Some(x) = pinned.iter_mut().find(|x| x.key == rk) {
                if !x.atoms.contains(&r.atom) {
                    x.atoms.push(r.atom.clone());
                }
                continue;
            }
            let vs = versions(&rk).unwrap_or_default();
            let outcome = if vs.is_empty() { Outcome::Unknown } else { outcome(&t, &installed, &vs, testing, None) };
            let p = Pinned { key: rk, installed: installed.raw, atoms: vec![r.atom.clone()], outcome };
            moving.extend(moves(&p, &vs));
            pinned.push(p);
        }
    }
    pinned.sort_by(|a, b| (a.outcome.rank(), &a.key).cmp(&(b.outcome.rank(), &b.key)));
    let reqs = blockers.iter().map(|b| Requirement { atom: b.clone(), by: format!("blocks {blocked}"), kind: ReqKind::Pin, requirer: None }).collect();
    SkipReport { slot_key, candidate: blocked.to_string(), reqs, pinned }
}

/// Does this version's dependency on the target's package allow the new version?
/// `||` groups are left out: another alternative may satisfy them.
fn accepts(v: &VersionInfo, t: &Target) -> bool {
    for var in ["DEPEND", "RDEPEND", "BDEPEND", "PDEPEND", "IDEPEND"] {
        let text = v.meta.get(var);
        if !text.contains(&t.pn) {
            continue;
        }
        let tree = dep::parse(text);
        let mut atoms = Vec::new();
        dep::collect_required_atoms(&tree, &mut atoms);
        for a in atoms {
            if a.blocker != 0 || !a.matches_key(&t.cat, &t.pn) || a.slot.as_deref().is_some_and(|s| s != t.slot) {
                continue;
            }
            if !a.matches(&t.cat, &t.pn, &t.ver, Some(&t.slot), None) {
                return false;
            }
        }
    }
    true
}

/// What it takes to move a package that holds `t` back: its newest version that
/// accepts `t` (and that `blocker`, if any, no longer blocks).
fn outcome(t: &Target, installed: &Version, versions: &[VersionInfo], testing: bool, blocker: Option<&Atom>) -> Outcome {
    let unblocked = |v: &VersionInfo| !blocker.is_some_and(|b| b.matches(&b.cat, &b.pn, &v.ver, Some(v.meta.slot()), None));
    let accepting = || versions.iter().filter(|v| !v.ver.is_live() && v.ver > *installed && unblocked(v) && accepts(v, t));
    let Some(v) = accepting().find(|v| !v.masked()).or_else(|| accepting().next()) else { return Outcome::NoVersion };
    let version = v.ver.raw.clone();
    if v.visible(testing) {
        return Outcome::Follows(version);
    }
    if v.masked() {
        let why = match &v.profile_mask {
            Some((src, _)) => format!("masked in {}", src.display()),
            None => "masked in your package.mask".to_string(),
        };
        return Outcome::Blocked { version, why };
    }
    match v.kw {
        KwState::Testing | KwState::Missing => {
            let newer = versions.iter().any(|x| x.ver > v.ver && !x.ver.is_live() && !x.masked() && matches!(x.kw, KwState::Stable | KwState::Testing));
            Outcome::Keyword { version, kw: v.kw, newer }
        }
        KwState::Broken => Outcome::Blocked { version, why: "keyworded as broken for this architecture".into() },
        KwState::Stable => Outcome::Follows(version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portage::config::{ConfFile, ConfSet, parse_line};
    use crate::portage::metadata::{EbuildMeta, MetaSource};

    fn ver(v: &str, kw: KwState, rdepend: &str) -> VersionInfo {
        VersionInfo {
            ver: Version::parse(v).unwrap(),
            repo: "gentoo".into(),
            meta: EbuildMeta {
                vars: [("RDEPEND".to_string(), rdepend.to_string()), ("SLOT".into(), "6/".to_string() + v)].into(),
                source: MetaSource::Md5Cache,
            },
            kw,
            ebuild_path: PathBuf::new(),
            user_masked: false,
            user_unmasked: false,
            user_keyworded: false,
            installed: false,
            profile_mask: None,
        }
    }

    /// dev-python/pyside on 2026-11-04: 9999 live, 6.12.0 masked in the profile, 6.11.3 ~amd64, 6.11.2 stable.
    fn pyside(mask_612: bool) -> Vec<VersionInfo> {
        let mut v612 = ver("6.12.0", KwState::Testing, "=dev-qt/qtbase-6.12.0*:6[gui] gui? ( =dev-qt/qtbase-6.12.0*:6[widgets] )");
        if mask_612 {
            v612.profile_mask = Some(("/var/db/repos/gentoo/profiles/package.mask".into(), vec![]));
        }
        vec![
            ver("6.9999", KwState::Missing, "~dev-qt/qtbase-6.9999:6"),
            v612,
            ver("6.11.3", KwState::Testing, ">=dev-lang/python-3.12 =dev-qt/qtbase-6.11.3*:6[gui] gui? ( =dev-qt/qtbase-6.11.3*:6[widgets] )"),
            ver("6.11.2", KwState::Stable, "=dev-qt/qtbase-6.11.2*:6[gui]"),
        ]
    }

    fn req(atom: &str, by: &str) -> (String, String) {
        (atom.into(), by.into())
    }

    fn qt_report(testing: bool, mask_612: bool) -> SkipReport {
        let required = vec![
            req("=dev-qt/qtbase-6.11.2*:6[gui]", r#"(dev-python/pyside-6.11.2:6/6.11.2::gentoo, installed) USE="gui""#),
            req("=dev-qt/qtbase-6.11.2*:6[ssl]", r#"(dev-python/pyside-6.11.2:6/6.11.2::gentoo, installed) USE="gui""#),
            req("~dev-qt/qtbase-6.11.2:6[gui,widgets]", "(dev-qt/qtgraphs-6.11.2:6/6.11.2::gentoo, installed)"),
            req("~dev-qt/qtbase-6.11.2:6[widgets]", "(dev-qt/qttools-6.11.2:6/6.11.2::gentoo, installed)"),
            req("~dev-qt/qtbase-6.11.2:6", "(dev-qt/qtfuture-6.11.2:6/6.11.2::gentoo, installed)"),
            req(">=dev-qt/qtbase-6.11.2:6/6.11.2=[X]", "(kde-apps/gwenview-26.08.2:6/6::gentoo, installed)"),
            req("dev-qt/qtbase:6/6.11.2=", "(media-video/vlc-4.0.0_pre20260625:0/12-9::gentoo, installed)"),
        ];
        analyze("dev-qt/qtbase:6", "dev-qt/qtbase-6.11.3", &required, testing, |k| match k {
            "dev-python/pyside" => Some(pyside(mask_612)),
            "dev-qt/qtgraphs" => Some(vec![
                ver("6.11.3", KwState::Testing, "~dev-qt/qtbase-6.11.3:6[gui,widgets]"),
                ver("6.11.2", KwState::Stable, "~dev-qt/qtbase-6.11.2:6[gui,widgets]"),
            ]),
            "dev-qt/qttools" => Some(vec![
                VersionInfo { user_keyworded: true, ..ver("6.11.3", KwState::Testing, "~dev-qt/qtbase-6.11.3:6") },
                ver("6.11.2", KwState::Stable, "~dev-qt/qtbase-6.11.2:6"),
            ]),
            "dev-qt/qtfuture" => Some(vec![ver("6.11.2", KwState::Stable, "~dev-qt/qtbase-6.11.2:6")]),
            _ => None,
        })
    }

    #[test]
    fn sorts_out_the_qt_skip() {
        let r = qt_report(false, true);
        let by = |k: &str| r.pinned.iter().find(|p| p.key == k).unwrap().outcome.clone();
        assert_eq!(by("dev-python/pyside"), Outcome::Keyword { version: "6.11.3".into(), kw: KwState::Testing, newer: false });
        assert_eq!(by("dev-python/pyside").rank(), 0);
        assert_eq!(by("dev-qt/qtgraphs"), Outcome::Keyword { version: "6.11.3".into(), kw: KwState::Testing, newer: false });
        assert_eq!(by("dev-qt/qttools"), Outcome::Follows("6.11.3".into()));
        assert_eq!(by("dev-qt/qtfuture"), Outcome::NoVersion);
        assert_eq!(r.pinned.iter().find(|p| p.key == "dev-python/pyside").unwrap().atoms.len(), 2);
        assert_eq!(r.causes().count(), 2);
        assert_eq!(r.pinned.last().unwrap().key, "dev-qt/qttools");
        assert_eq!(r.rebuilds(), ["kde-apps/gwenview-26.08.2", "media-video/vlc-4.0.0_pre20260625"]);
        // ~amd64 accepted globally: nothing to fix.
        let r = qt_report(true, true);
        assert_eq!(r.causes().count(), 0);
        // With 6.12.0 unmasked an unversioned pyside line could pull it in.
        let r = qt_report(false, false);
        let p = r.pinned.iter().find(|p| p.key == "dev-python/pyside").unwrap();
        assert_eq!(p.outcome, Outcome::Keyword { version: "6.11.3".into(), kw: KwState::Testing, newer: true });
    }

    #[test]
    fn masked_and_broken_need_more_than_keywords() {
        let t = Target::parse("dev-qt/qtbase:6", "dev-qt/qtbase-6.11.3").unwrap();
        let old = Version::parse("6.11.2").unwrap();
        let mut v = ver("6.11.3", KwState::Testing, "~dev-qt/qtbase-6.11.3:6");
        v.user_masked = true;
        assert!(matches!(outcome(&t, &old, &[v], false, None), Outcome::Blocked { why, .. } if why.contains("your package.mask")));
        let v = ver("6.11.3", KwState::Broken, "~dev-qt/qtbase-6.11.3:6");
        assert!(matches!(outcome(&t, &old, &[v], false, None), Outcome::Blocked { .. }));
        // A dependency on another slot of the package does not count.
        let v = ver("6.11.3", KwState::Testing, "dev-qt/qtbase:5 ~dev-qt/qtbase-6.11.3:6");
        assert!(matches!(outcome(&t, &old, &[v], false, None), Outcome::Keyword { .. }));
        // `||` alternatives are not treated as pins.
        let v = ver("6.11.3", KwState::Testing, "|| ( ~dev-qt/qtbase-6.11.2:6 dev-libs/other )");
        assert!(matches!(outcome(&t, &old, &[v], false, None), Outcome::Keyword { .. }));
    }

    #[test]
    fn sorts_out_the_qt_blockers() {
        use crate::portage::pretend::{self, Problem};
        let plan = pretend::parse(include_str!("../../tests/fixtures/qt-6.11.3-keyword-skip/emerge-recipe-before.txt"), Some(1));
        let blockers: Vec<String> = plan
            .problems
            .iter()
            .filter_map(|p| match p {
                Problem::Blocker { blocker, resolved: false, .. } => Some(blocker.clone()),
                _ => None,
            })
            .collect();
        // Their 6.11.3 is accepted; the other five Qt modules and pyside are not.
        let accepted = [
            "dev-qt/qtmultimedia",
            "dev-qt/qtshadertools",
            "dev-qt/qtquick3d",
            "dev-qt/qtdeclarative",
            "dev-qt/qtsvg",
            "dev-qt/qtquicktimeline",
            "dev-qt/qttools",
        ];
        let r = analyze_blocked("dev-qt/qtbase-6.11.3", "6", &blockers, &plan.pulled_in, false, |k| {
            if k == "dev-python/pyside" {
                let mut v = pyside(true);
                v[3].installed = true;
                return Some(v);
            }
            if !k.starts_with("dev-qt/") {
                return None;
            }
            let mut new = ver("6.11.3", KwState::Testing, "~dev-qt/qtbase-6.11.3:6");
            new.user_keyworded = accepted.contains(&k);
            let mut old = ver("6.11.2", KwState::Stable, "~dev-qt/qtbase-6.11.2:6");
            old.installed = true;
            Some(vec![new, old])
        });
        assert_eq!(r.slot_key, "dev-qt/qtbase:6");
        let mut causes: Vec<&str> = r.causes().map(|p| p.key.as_str()).collect();
        causes.sort();
        // pyside is not among the blockers: it comes in through `=dev-qt/qtwebchannel-6.11.2*` in the pulled-in list.
        assert_eq!(causes, ["dev-python/pyside", "dev-qt/qtcharts", "dev-qt/qtconnectivity", "dev-qt/qtgraphs", "dev-qt/qtspeech", "dev-qt/qtwebchannel"]);
        let follows: Vec<&str> = r.pinned.iter().filter(|p| matches!(p.outcome, Outcome::Follows(_))).map(|p| p.key.as_str()).collect();
        assert_eq!(follows.len(), accepted.len(), "{follows:?}");
        let cfg = kw_cfg(&[("kde", "dev-qt/qtbase ~amd64\n=dev-python/pyside-6.11.2 ~amd64\n")]);
        let f = r.fixes(&cfg, "amd64", false, Path::new("/etc/portage/package.accept_keywords/zz"));
        assert_eq!(f.len(), 6);
        assert!(f.iter().any(|(k, fix)| k == "dev-python/pyside" && matches!(fix, KeywordFix::Replace { .. })));
        assert!(f.iter().any(|(k, fix)| k == "dev-qt/qtgraphs" && matches!(fix, KeywordFix::Add { line, .. } if line == "dev-qt/qtgraphs ~amd64")));
    }

    fn kw_cfg(files: &[(&str, &str)]) -> PortageConfig {
        let files = files
            .iter()
            .map(|(name, raw)| ConfFile {
                path: format!("/etc/portage/package.accept_keywords/{name}").into(),
                rel: name.to_string(),
                ignored: false,
                lines: raw.lines().map(parse_line).collect(),
                raw: raw.to_string(),
            })
            .collect();
        PortageConfig {
            sets: vec![ConfSet { kind: ConfKind::AcceptKeywords, base: "/etc/portage/package.accept_keywords".into(), exists: true, is_dir: true, files }],
            ..Default::default()
        }
    }

    #[test]
    fn fixes_unpin_or_add_next_to_the_update() {
        let cfg = kw_cfg(&[
            ("kde", "dev-qt/qtbase ~amd64\ndev-qt/qttools ~amd64\n=dev-python/pyside-6.11.2 ~amd64 # pinned\n"),
            ("zz-portagehub", "app-misc/foo ~amd64\n"),
        ]);
        let default = PathBuf::from("/etc/portage/package.accept_keywords/zz-portagehub");
        let r = qt_report(false, true);
        let f = r.fixes(&cfg, "amd64", false, &default);
        assert_eq!(f.len(), 2);
        let KeywordFix::Replace { path, idx, old, new } = &f[0].1 else { panic!("{f:?}") };
        assert_eq!(f[0].0, "dev-python/pyside");
        assert_eq!(path, &PathBuf::from("/etc/portage/package.accept_keywords/kde"));
        assert_eq!(*idx, 2);
        assert_eq!(old, "=dev-python/pyside-6.11.2 ~amd64 # pinned");
        assert_eq!(new.render(), "dev-python/pyside ~amd64 # pinned");
        let KeywordFix::Add { path, line } = &f[1].1 else { panic!("{f:?}") };
        assert_eq!(path, &PathBuf::from("/etc/portage/package.accept_keywords/kde"));
        assert_eq!(line, "dev-qt/qtgraphs ~amd64");
        // Exact mode, and no keyword file mentions qtbase: the default file.
        let cfg = kw_cfg(&[("zz-portagehub", "app-misc/foo ~amd64\n")]);
        let f = r.fixes(&cfg, "amd64", true, &default);
        let KeywordFix::Add { path, line } = &f[0].1 else { panic!("{f:?}") };
        assert_eq!(path, &default);
        assert_eq!(line, "~dev-python/pyside-6.11.3 ~amd64");
    }
}
