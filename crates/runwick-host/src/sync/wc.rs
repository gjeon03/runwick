//! The git working copy of the data repository, read and written without ever following a
//! link: every path is checked component by component, so a symbolic link (or anything that is
//! not a plain folder/file) in the repository cannot make Runwick read or write outside it.

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

pub struct WorkingCopy {
    pub dir: PathBuf,
    pub branch: String,
}

/// A repository path we are willing to touch: relative, no `..`, not inside `.git`.
pub fn safe_path(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !path.components().any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
}

fn refused(what: &str, rel: &str) -> io::Error {
    io::Error::other(format!("{rel}: {what} (링크나 특수 파일은 따라가지 않아요)"))
}

impl WorkingCopy {
    /// Where `rel` is, after checking that nothing on the way is a link and every existing
    /// parent is a folder. The file itself may not exist yet.
    pub fn file(&self, rel: &str) -> io::Result<PathBuf> {
        if !safe_path(rel) {
            return Err(refused("허용되지 않는 경로", rel));
        }
        let parts: Vec<Component> = Path::new(rel).components().collect();
        let mut cur = self.dir.clone();
        for (i, part) in parts.iter().enumerate() {
            cur.push(part);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => return Err(refused("링크", rel)),
                Ok(m) if i + 1 < parts.len() && !m.is_dir() => {
                    return Err(refused("폴더가 아닌 경로", rel));
                }
                Ok(m) if i + 1 == parts.len() && !m.is_file() => {
                    return Err(refused("일반 파일이 아님", rel));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                Err(e) => return Err(e),
            }
        }
        Ok(self.dir.join(rel))
    }

    /// Contents of a plain file (None when it does not exist or is not a plain file).
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        let p = self.file(rel).ok()?;
        std::fs::symlink_metadata(&p).ok().filter(|m| m.is_file())?;
        std::fs::read(p).ok()
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.file(rel).ok().and_then(|p| std::fs::symlink_metadata(p).ok()).is_some_and(|m| m.is_file())
    }

    /// Write `bytes` at `rel` when they differ (through a temporary file and a rename, so a
    /// crash never leaves half a file). Returns true when it wrote.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<bool> {
        if self.read(rel).as_deref() == Some(bytes) {
            return Ok(false);
        }
        let file = self.file(rel)?;
        let dir = file.parent().ok_or_else(|| io::Error::other("no parent folder"))?;
        std::fs::create_dir_all(dir)?;
        // The folder must resolve inside the working copy (no link anywhere on the way).
        if !dir.canonicalize()?.starts_with(self.dir.canonicalize()?) {
            return Err(refused("작업 사본 밖의 경로", rel));
        }
        let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &file).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(true)
    }

    /// Remove the plain file at `rel`. Returns true when there was one.
    pub fn remove(&self, rel: &str) -> io::Result<bool> {
        let file = self.file(rel)?;
        if !std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            return Ok(false);
        }
        std::fs::remove_file(&file)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_inside_the_working_copy_are_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        let wc = WorkingCopy { dir: d.path().join("wc"), branch: "main".into() };
        std::fs::create_dir_all(&wc.dir).unwrap();
        std::os::unix::fs::symlink(&outside, wc.dir.join("pins")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), wc.dir.join("manifest.json")).unwrap();
        assert!(wc.write("pins/x.json", b"{}").is_err());
        assert!(!outside.join("x.json").exists());
        assert_eq!(wc.read("manifest.json"), None);
        assert!(wc.write("manifest.json", b"{}").is_err());
        assert_eq!(std::fs::read_to_string(outside.join("secret.txt")).unwrap(), "outside");
        assert!(wc.remove("pins/secret.txt").is_err());
        assert!(outside.join("secret.txt").exists());
        assert!(wc.write("../escape.json", b"{}").is_err());
        assert!(wc.write("roles/a.json", b"{}").unwrap());
        assert!(!wc.write("roles/a.json", b"{}").unwrap());
        assert_eq!(wc.read("roles/a.json").as_deref(), Some(&b"{}"[..]));
    }
}
