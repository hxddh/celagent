//! Reading and editing `lenslet.lock` from the window.
//!
//! Reads go through `lenslet_core::lock`. Every write the window makes goes
//! through exactly two functions, [`accept`] and [`set_test_mark`], whose
//! signatures match the `lenslet_core::lock` writers so they can be swapped
//! for them. All of it is blocking IO: call from the background executor.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

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
/// were not already in it. Same signature as `lenslet_core::lock::accept`.
pub fn accept(path: &Path, entries: &[(Access, bool)]) -> anyhow::Result<usize> {
    fsguard::assert_io_allowed();
    let mut live = LiveLock::read(path)?;
    let mut added = 0;
    for (access, test) in entries {
        if live.access.insert(access.clone()) {
            added += 1;
        }
        if *test {
            live.test.insert(access.clone());
        }
    }
    live.write(path)?;
    Ok(added)
}

/// Mark or unmark a permission `# test` (adding it to the lock if missing).
/// Returns whether the file changed. Same signature as
/// `lenslet_core::lock::set_test_mark`.
pub fn set_test_mark(path: &Path, access: &Access, test: bool) -> anyhow::Result<bool> {
    fsguard::assert_io_allowed();
    let mut live = LiveLock::read(path)?;
    let mut changed = live.access.insert(access.clone());
    changed |= if test {
        live.test.insert(access.clone())
    } else {
        live.test.remove(access)
    };
    if changed {
        live.write(path)?;
    }
    Ok(changed)
}

/// The lock with the `implied` notes the core `Lock` does not keep.
#[derive(Default)]
struct LiveLock {
    access: BTreeSet<Access>,
    test: BTreeSet<Access>,
    implied: BTreeSet<Access>,
}

impl LiveLock {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let lock = lenslet_core::lock::parse(&text);
        let mut implied = BTreeSet::new();
        for line in text.lines() {
            let Some((entry, comment)) = line.split_once('#') else {
                continue;
            };
            if comment
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word == "implied")
            {
                let mut fields = entry.split_whitespace();
                if let Some(action) = fields.next() {
                    implied.insert(Access::new(action, fields.next().unwrap_or("*")));
                }
            }
        }
        Ok(LiveLock {
            access: lock.access,
            test: lock.test,
            implied,
        })
    }

    fn write(&self, path: &Path) -> anyhow::Result<()> {
        let width = self
            .access
            .iter()
            .map(|a| a.action.len())
            .max()
            .unwrap_or(0);
        let mut out = String::from(
            "# AWS access observed by `lenslet run`: action, resource.\n\
             # Regenerate with `lenslet run --update -- <tests>`; review changes like code.\n\
             # Mark a line `# test` when only the tests need it; `lenslet policy` leaves it out.\n",
        );
        for entry in &self.access {
            let _ = write!(out, "{:<width$}  {}", entry.action, entry.resource);
            let mut notes = Vec::new();
            if self.test.contains(entry) {
                notes.push("test");
            }
            if self.implied.contains(entry) {
                notes.push("implied");
            }
            if !notes.is_empty() {
                let _ = write!(out, "  # {}", notes.join(", "));
            }
            out.push('\n');
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("lock.tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
