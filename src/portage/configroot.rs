//! Private copies of /etc/portage, for asking Portage "what if" through
//! PORTAGE_CONFIGROOT without touching the real configuration.

use super::config::ETC_PORTAGE;
use std::io;
use std::path::{Path, PathBuf};

/// A private scratch directory, removed on drop.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> io::Result<Scratch> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("portagehub-{tag}-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&p)?;
        Ok(Scratch(p))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A config root holding a copy of the real /etc/portage with `changed` applied
/// (real /etc/portage paths; `None` removes the file). Pass its path as
/// PORTAGE_CONFIGROOT; ROOT stays `/`, so the installed packages and the world
/// file are the real ones.
pub fn stage(changed: &[(PathBuf, Option<String>)]) -> io::Result<Scratch> {
    stage_from(Path::new(ETC_PORTAGE), changed)
}

fn stage_from(src: &Path, changed: &[(PathBuf, Option<String>)]) -> io::Result<Scratch> {
    let scratch = Scratch::new("configroot")?;
    let etc = scratch.0.join("etc/portage");
    std::fs::create_dir_all(&etc)?;
    // /etc/portage itself may be a symlink.
    copy_tree(&std::fs::canonicalize(src)?, &etc, 0)?;
    for (path, content) in changed {
        let rel =
            path.strip_prefix(src).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not below {}", path.display(), src.display())))?;
        let dst = etc.join(rel);
        // The copy has no symlinks besides make.profile; never write through one.
        if dst.ancestors().take_while(|a| *a != scratch.0).any(|a| a.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} goes through a symlink", path.display())));
        }
        match content {
            Some(text) => {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dst, text)?;
            }
            None => {
                let _ = std::fs::remove_file(&dst);
            }
        }
    }
    Ok(scratch)
}

/// Copy a configuration tree. The top-level make.profile stays a symlink, made
/// absolute so the profile chain resolves from the copy; every other symlink is
/// copied as what it points to, so nothing written into the copy can reach the
/// real files. Dangling symlinks are skipped, as Portage cannot read them either.
fn copy_tree(src: &Path, dst: &Path, depth: usize) -> io::Result<()> {
    if depth > 16 {
        return Err(io::Error::other(format!("{}: directories nested too deep (symlink loop?)", src.display())));
    }
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        let is_link = e.file_type()?.is_symlink();
        if depth == 0 && e.file_name() == "make.profile" {
            std::os::unix::fs::symlink(std::fs::canonicalize(&from)?, &to)?;
            continue;
        }
        let meta = match std::fs::metadata(&from) {
            Ok(m) => m,
            Err(_) if is_link => continue,
            Err(err) => return Err(err),
        };
        if meta.is_dir() {
            std::fs::create_dir(&to)?;
            copy_tree(&from, &to, depth + 1)?;
        } else if meta.is_file() {
            std::fs::copy(&from, &to).map_err(|err| io::Error::new(err.kind(), format!("cannot copy {}: {err}", from.display())))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_and_applies_without_touching_the_source() {
        let src = Scratch::new("test-src").unwrap();
        let s = src.path();
        let profiles = s.join("profiles/default");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(profiles.join("parent"), "..\n").unwrap();
        let etc = s.join("etc/portage");
        std::fs::create_dir_all(etc.join("package.accept_keywords")).unwrap();
        std::fs::create_dir_all(s.join("elsewhere")).unwrap();
        std::fs::write(s.join("elsewhere/shared"), "app-misc/shared ~amd64\n").unwrap();
        std::fs::write(etc.join("make.conf"), "USE=\"x\"\n").unwrap();
        std::fs::write(etc.join("package.accept_keywords/kde"), "=dev-python/pyside-6.11.2 ~amd64\n").unwrap();
        std::fs::write(etc.join("package.accept_keywords/old"), "app-misc/old ~amd64\n").unwrap();
        // Relative symlinks, as on a real system.
        std::os::unix::fs::symlink("../../profiles/default", etc.join("make.profile")).unwrap();
        std::os::unix::fs::symlink("../../../elsewhere/shared", etc.join("package.accept_keywords/shared")).unwrap();
        std::os::unix::fs::symlink("missing", etc.join("package.accept_keywords/dangling")).unwrap();

        let changed = vec![
            (etc.join("package.accept_keywords/kde"), Some("dev-python/pyside ~amd64\n".to_string())),
            (etc.join("package.accept_keywords/shared"), Some("app-misc/shared **\n".to_string())),
            (etc.join("package.accept_keywords/old"), None),
        ];
        let root = stage_from(&etc, &changed).unwrap();
        let copy = root.path().join("etc/portage");
        assert_eq!(std::fs::read_to_string(copy.join("make.conf")).unwrap(), "USE=\"x\"\n");
        assert_eq!(std::fs::read_to_string(copy.join("package.accept_keywords/kde")).unwrap(), "dev-python/pyside ~amd64\n");
        assert!(!copy.join("package.accept_keywords/old").exists());
        assert!(!copy.join("package.accept_keywords/dangling").exists());
        let profile = std::fs::read_link(copy.join("make.profile")).unwrap();
        assert!(profile.is_absolute());
        assert_eq!(std::fs::read_to_string(copy.join("make.profile/parent")).unwrap(), "..\n");
        // The symlinked file was copied, so the change stayed in the copy.
        assert!(!copy.join("package.accept_keywords/shared").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(s.join("elsewhere/shared")).unwrap(), "app-misc/shared ~amd64\n");
        assert_eq!(std::fs::read_to_string(etc.join("package.accept_keywords/kde")).unwrap(), "=dev-python/pyside-6.11.2 ~amd64\n");
        assert!(etc.join("package.accept_keywords/old").exists());
        // Paths outside the source and writes through make.profile are refused.
        assert!(stage_from(&etc, &[("/tmp/elsewhere".into(), Some(String::new()))]).is_err());
        assert!(stage_from(&etc, &[(etc.join("make.profile/parent"), Some(String::new()))]).is_err());
        let dir = root.path().to_path_buf();
        drop(root);
        assert!(!dir.exists());
    }
}
