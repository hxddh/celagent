//! Reading and editing `lenslet.lock` from the window.
//!
//! Reads go through `lenslet_core::lock`. Every write the window makes goes
//! through exactly two functions, [`accept`] and [`set_test_mark`], whose
//! signatures match the `lenslet_core::lock` writers so they can be swapped
//! for them. All of it is blocking IO: call from the background executor.

use std::path::{Path, PathBuf};

use lenslet_core::{access::Access, lock::Lock};

use crate::{fsguard, model::RunHeader};

/// The lock a run is compared with: its header's `lock`, else `cwd/lenslet.lock`.
/// `None` when the run is gated by an IAM policy instead.
pub fn lock_path(header: &RunHeader) -> Option<PathBuf> {
    if header.policy.is_some() {
        return None;
    }
    if let Some(lock) = header.lock.as_deref().filter(|s| !s.is_empty()) {
        let path = Path::new(lock);
        return Some(if path.is_absolute() || header.cwd.is_empty() {
            path.to_path_buf()
        } else {
            Path::new(&header.cwd).join(path)
        });
    }
    (!header.cwd.is_empty()).then(|| lenslet_core::lock::path_in(Path::new(&header.cwd)))
}

/// The lock file, or `None` when there is none.
pub fn read(path: &Path) -> anyhow::Result<Option<Lock>> {
    fsguard::assert_io_allowed();
    lenslet_core::lock::read(path)
}

/// Add permissions to the lock (`test` marks them `# test`). Returns how many
/// were not already in it. The write is `lenslet_core::lock`'s: atomic and
/// canonical, the same file `lenslet run --update` writes.
pub fn accept(path: &Path, entries: &[(Access, bool)]) -> anyhow::Result<usize> {
    fsguard::assert_io_allowed();
    lenslet_core::lock::accept(path, entries)
}

/// Mark or unmark a permission `# test`. Marking one the lock does not have
/// yet accepts it as a test line. Returns whether the file changed.
pub fn set_test_mark(path: &Path, access: &Access, test: bool) -> anyhow::Result<bool> {
    fsguard::assert_io_allowed();
    if lenslet_core::lock::set_test_mark(path, access, test)? {
        return Ok(true);
    }
    let missing = read(path)?.is_none_or(|lock| !lock.access.contains(access));
    if test && missing {
        return Ok(lenslet_core::lock::accept(path, &[(access.clone(), true)])? > 0);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn accepts_and_marks_permissions_keeping_notes() {
        let dir = std::env::temp_dir().join(format!("lenslet-ui-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lenslet.lock");
        fs::write(&path, "kms:Decrypt  arn:aws:kms:*:*:key/*  # implied\n").unwrap();

        let get = Access::new("s3:GetObject", "arn:aws:s3:::b/*");
        let put = Access::new("s3:PutObject", "arn:aws:s3:::b/*");
        assert_eq!(
            accept(&path, &[(get.clone(), false), (put.clone(), true)]).unwrap(),
            2
        );
        assert_eq!(accept(&path, &[(get.clone(), false)]).unwrap(), 0);
        let lock = read(&path).unwrap().unwrap();
        assert!(lock.access.contains(&get) && lock.test.contains(&put));
        assert!(fs::read_to_string(&path).unwrap().contains("# implied"));

        assert!(set_test_mark(&path, &get, true).unwrap());
        assert!(!set_test_mark(&path, &get, true).unwrap(), "already marked");
        assert!(set_test_mark(&path, &put, false).unwrap());
        let lock = read(&path).unwrap().unwrap();
        assert!(lock.test.contains(&get) && !lock.test.contains(&put));

        let header = RunHeader {
            cwd: dir.display().to_string(),
            ..RunHeader::default()
        };
        assert_eq!(lock_path(&header).unwrap(), path);
        let gated = RunHeader {
            policy: Some("p.json".into()),
            ..header
        };
        assert!(lock_path(&gated).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
