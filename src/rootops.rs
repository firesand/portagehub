//! Privileged operations with arguments fixed or validated here, never taken
//! verbatim from the GUI: boot scan, bootloader updates, known-good kernel
//! marking, kernel leftover removal and build-dir cleanup.

use crate::protocol::{BootAction, BootScan, KERNEL_STATE_DIRS, ListEntry, Listing, PROGRAM_DIRS, ScannedFile, valid_kver};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_FILE: u64 = 1 << 20;
const MAX_TOTAL: usize = 4 << 20;

const TEXT_FILES: &[&str] = &[
    "/boot/grub/grub.cfg",
    "/boot/grub2/grub.cfg",
    "/boot/grub/grubenv",
    "/boot/grub2/grubenv",
    "/etc/default/grub",
    "/etc/lilo.conf",
    "/boot/loader/loader.conf",
    "/boot/efi/loader/loader.conf",
    "/efi/loader/loader.conf",
    "/boot/refind_linux.conf",
    "/boot/efi/EFI/refind/refind.conf",
    "/efi/EFI/refind/refind.conf",
    "/boot/limine.conf",
    "/boot/limine/limine.conf",
    "/boot/efi/limine.conf",
];

const ENTRY_DIRS: &[&str] = &["/boot/loader/entries", "/boot/efi/loader/entries", "/efi/loader/entries"];
const EFI_DIRS: &[&str] = &["/boot/efi/EFI", "/efi/EFI", "/boot/EFI"];
const LIST_DIRS: &[&str] = &["/boot", "/boot/kernel-backup", "/boot/efi/EFI/Linux", "/efi/EFI/Linux", "/boot/EFI/Linux", "/boot/efi/EFI", "/efi/EFI"];

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn list(dir: &Path) -> Option<Listing> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut entries: Vec<ListEntry> = rd
        .flatten()
        .map(|e| {
            let md = e.metadata().ok();
            ListEntry {
                name: e.file_name().to_string_lossy().to_string(),
                size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                mtime: md.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0),
                is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Some(Listing { dir: dir.to_string_lossy().to_string(), entries })
}

/// Read-only: bootloader configuration files and boot directory listings.
pub fn boot_scan() -> BootScan {
    let mut scan = BootScan { root: unsafe { libc::geteuid() } == 0, ..Default::default() };
    let mut total = 0usize;
    let mut paths: Vec<PathBuf> = TEXT_FILES.iter().map(PathBuf::from).collect();
    for d in ENTRY_DIRS {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            if e.path().extension().is_some_and(|x| x == "conf") {
                paths.push(e.path());
            }
        }
    }
    for d in EFI_DIRS {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let g = e.path().join("grub.cfg");
            if g.is_file() {
                paths.push(g);
            }
        }
    }
    for p in paths {
        let md = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                scan.errors.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        if !md.is_file() {
            continue;
        }
        if md.len() > MAX_FILE || total > MAX_TOTAL {
            scan.errors.push(format!("{} skipped (too large)", p.display()));
            continue;
        }
        match std::fs::read(&p) {
            Ok(b) => {
                total += b.len();
                scan.files.push(ScannedFile { path: p.to_string_lossy().to_string(), content: String::from_utf8_lossy(&b).to_string(), mtime: mtime(&p) });
            }
            Err(e) => scan.errors.push(format!("{}: {e}", p.display())),
        }
    }
    let mut dirs: Vec<PathBuf> = LIST_DIRS.iter().map(PathBuf::from).collect();
    for e in std::fs::read_dir("/boot/kernel-backup").into_iter().flatten().flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(e.path());
        }
    }
    for d in dirs {
        if let Some(l) = list(&d) {
            scan.listings.push(l);
        }
    }
    scan
}

fn find_program(names: &[&str]) -> Option<String> {
    for n in names {
        for d in PROGRAM_DIRS {
            let p = Path::new(d).join(n);
            if p.is_file() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Program and arguments for a bootloader/initramfs action.
pub fn boot_action_argv(action: BootAction, kver: Option<&str>) -> Result<(String, Vec<String>), String> {
    match action {
        BootAction::GrubMkconfig => {
            let prog = find_program(&["grub-mkconfig", "grub2-mkconfig"]).ok_or("grub-mkconfig is not installed")?;
            let target = ["/boot/grub/grub.cfg", "/boot/grub2/grub.cfg"]
                .iter()
                .find(|p| Path::new(p).is_file())
                .or_else(|| ["/boot/grub/grub.cfg", "/boot/grub2/grub.cfg"].iter().find(|p| Path::new(p).parent().is_some_and(|d| d.is_dir())))
                .ok_or("no /boot/grub directory")?;
            Ok((prog, vec!["-o".into(), target.to_string()]))
        }
        BootAction::Lilo => Ok((find_program(&["lilo"]).ok_or("lilo is not installed")?, vec![])),
        BootAction::BootctlUpdate => Ok((find_program(&["bootctl"]).ok_or("bootctl is not installed")?, vec!["update".into()])),
        BootAction::DracutRegen => {
            let k = kver.ok_or("kernel version required")?;
            if !valid_kver(k) || !Path::new("/lib/modules").join(k).is_dir() {
                return Err(format!("unknown kernel {k}"));
            }
            Ok((find_program(&["dracut"]).ok_or("dracut is not installed")?, vec!["--force".into(), "--kver".into(), k.to_string()]))
        }
    }
}

fn read_trim(p: &Path) -> String {
    std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn boot_image_exists(kver: &str) -> bool {
    ["vmlinuz", "kernel", "bzImage"].iter().any(|p| Path::new("/boot").join(format!("{p}-{kver}")).is_file())
}

/// Record a known-good kernel, rotating last-good into previous-good.
pub fn mark_good(kver: &str, state_dir: &str, backup: bool, log: &dyn Fn(String)) -> Result<(), String> {
    if !valid_kver(kver) {
        return Err(format!("invalid kernel version {kver}"));
    }
    if !KERNEL_STATE_DIRS.contains(&state_dir) {
        return Err(format!("state directory not allowed: {state_dir}"));
    }
    if !boot_image_exists(kver) {
        return Err(format!("no /boot/vmlinuz-{kver}"));
    }
    let dir = Path::new(state_dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let last = read_trim(&dir.join("last-good"));
    if !last.is_empty() && last != kver {
        std::fs::write(dir.join("previous-good"), format!("{last}\n")).map_err(|e| e.to_string())?;
        log(format!("previous-good = {last}"));
    }
    std::fs::write(dir.join("last-good"), format!("{kver}\n")).map_err(|e| e.to_string())?;
    log(format!("last-good = {kver}"));
    if backup {
        let dst = Path::new("/boot/kernel-backup").join(kver);
        std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        for f in [format!("vmlinuz-{kver}"), format!("kernel-{kver}"), format!("initramfs-{kver}.img"), format!("config-{kver}"), format!("System.map-{kver}")] {
            let src = Path::new("/boot").join(&f);
            if src.is_file() {
                std::fs::copy(&src, dst.join(&f)).map_err(|e| format!("{}: {e}", src.display()))?;
                log(format!("backed up {} → {}", src.display(), dst.display()));
            }
        }
    }
    Ok(())
}

/// Packages whose CONTENTS reference a kernel's module or source tree.
pub fn kernel_owners(kver: &str) -> Vec<String> {
    let needles = [format!(" /lib/modules/{kver}/"), format!(" /usr/src/linux-{kver}/")];
    let exact = [format!("dir /lib/modules/{kver}"), format!("dir /usr/src/linux-{kver}")];
    let mut owners = Vec::new();
    for c in std::fs::read_dir("/var/db/pkg").into_iter().flatten().flatten() {
        for p in std::fs::read_dir(c.path()).into_iter().flatten().flatten() {
            let Ok(text) = std::fs::read_to_string(p.path().join("CONTENTS")) else { continue };
            if needles.iter().any(|n| text.contains(n.as_str())) || text.lines().any(|l| exact.iter().any(|e| l == e)) {
                owners.push(format!("{}/{}", c.file_name().to_string_lossy(), p.file_name().to_string_lossy()));
            }
        }
    }
    owners
}

/// Remove a no-longer-installed kernel's files (or only its `.old` copies).
pub fn remove_kernel_leftovers(kver: &str, old_only: bool, log: &dyn Fn(String)) -> Result<(), String> {
    if !valid_kver(kver) {
        return Err(format!("invalid kernel version {kver}"));
    }
    let mut removed = 0;
    let prefixes = ["vmlinuz", "kernel", "bzImage", "initramfs", "initrd", "System.map", "config"];
    if old_only {
        for p in prefixes {
            for suf in [".old", ".img.old"] {
                let f = Path::new("/boot").join(format!("{p}-{kver}{suf}"));
                if f.symlink_metadata().is_ok_and(|m| m.is_file()) {
                    std::fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
                    log(format!("removed {}", f.display()));
                    removed += 1;
                }
            }
        }
        return if removed == 0 { Err("nothing to remove".into()) } else { Ok(()) };
    }
    let running = read_trim(Path::new("/proc/sys/kernel/osrelease"));
    if kver == running {
        return Err(format!("{kver} is the running kernel"));
    }
    for d in KERNEL_STATE_DIRS {
        for f in ["last-good", "previous-good"] {
            if read_trim(&Path::new(d).join(f)) == kver {
                return Err(format!("{kver} is marked known-good ({d}/{f})"));
            }
        }
    }
    if std::fs::read_link("/usr/src/linux").is_ok_and(|t| t.to_string_lossy().trim_end_matches('/').ends_with(&format!("linux-{kver}"))) {
        return Err(format!("/usr/src/linux points to linux-{kver} (eselect kernel); select another kernel first"));
    }
    let owners = kernel_owners(kver);
    if !owners.is_empty() {
        return Err(format!("still owned by {} — remove the package with depclean first", owners.join(", ")));
    }
    for p in prefixes {
        for suf in ["", ".img", ".old", ".img.old"] {
            let f = Path::new("/boot").join(format!("{p}-{kver}{suf}"));
            if f.symlink_metadata().is_ok_and(|m| m.is_file() || m.file_type().is_symlink()) {
                std::fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
                log(format!("removed {}", f.display()));
                removed += 1;
            }
        }
    }
    for d in [Path::new("/lib/modules").join(kver), Path::new("/usr/src").join(format!("linux-{kver}"))] {
        if d.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            std::fs::remove_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
            log(format!("removed {}/", d.display()));
            removed += 1;
        }
    }
    if removed == 0 { Err("nothing to remove".into()) } else { Ok(()) }
}

/// Is an emerge or ebuild process running (other than this one)?
pub fn emerge_running() -> bool {
    let me = std::process::id().to_string();
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.bytes().all(|b| b.is_ascii_digit()) || name == me {
            continue;
        }
        let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else { continue };
        let args: Vec<String> = cmd.split(|b| *b == 0).take(3).map(|a| String::from_utf8_lossy(a).to_string()).collect();
        if args.iter().any(|a| {
            let base = a.rsplit('/').next().unwrap_or("");
            base == "emerge" || base == "ebuild"
        }) {
            return true;
        }
    }
    false
}

/// PORTAGE_TMPDIR values: the global one and those set in /etc/portage/env/*.
pub fn tmpdirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(prog) = find_program(&["portageq"])
        && let Ok(o) = std::process::Command::new(prog).args(["envvar", "PORTAGE_TMPDIR"]).output()
    {
        let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if v.starts_with('/') {
            out.push(PathBuf::from(v));
        }
    }
    for e in std::fs::read_dir("/etc/portage/env").into_iter().flatten().flatten() {
        let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
        for line in t.lines() {
            if let Some(v) = line.trim().strip_prefix("PORTAGE_TMPDIR=") {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if v.starts_with('/') && !v.contains('$') {
                    out.push(PathBuf::from(v));
                }
            }
        }
    }
    if out.is_empty() {
        out.push(PathBuf::from("/var/tmp"));
    }
    out.sort();
    out.dedup();
    out
}

/// Build directories (`<tmpdir>/portage/<cat>/<pf>`) left behind by failed or interrupted builds.
pub fn build_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for t in tmpdirs() {
        let base = t.join("portage");
        for c in std::fs::read_dir(&base).into_iter().flatten().flatten() {
            let cat = c.file_name().to_string_lossy().to_string();
            if !(cat.contains('-') || cat == "virtual") || !c.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            for p in std::fs::read_dir(c.path()).into_iter().flatten().flatten() {
                let pf = p.file_name().to_string_lossy().to_string();
                if pf.contains('-') && p.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    out.push(p.path());
                }
            }
        }
    }
    out
}

pub fn clean_build_dirs(log: &dyn Fn(String)) -> Result<(), String> {
    if emerge_running() {
        return Err("an emerge is running — try again when it has finished".into());
    }
    let dirs = build_dirs();
    if dirs.is_empty() {
        return Err("no leftover build directories".into());
    }
    for d in dirs {
        std::fs::remove_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        log(format!("removed {}", d.display()));
        if let Some(cat) = d.parent() {
            let _ = std::fs::remove_dir(cat);
        }
    }
    Ok(())
}
