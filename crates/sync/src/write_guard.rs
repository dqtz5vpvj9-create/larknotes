use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Tracks paths that the sync executor is currently writing to.
///
/// Shared between Executor (acquires guard) and Watcher (checks guard).
/// When executor writes a file, it guards the path first; the watcher
/// sees `is_guarded(path) == true` and ignores the FS event, preventing
/// a feedback loop.
#[derive(Debug, Clone)]
pub struct WriteGuard {
    guarded: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Default for WriteGuard {
    fn default() -> Self {
        Self {
            guarded: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl WriteGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire a guard for `path`. The returned token removes the path
    /// from the guarded set when dropped (RAII).
    pub fn guard(&self, path: &Path) -> WriteGuardToken {
        let canonical = path.to_path_buf();
        if let Ok(mut set) = self.guarded.lock() {
            set.insert(canonical.clone());
        }
        WriteGuardToken {
            path: canonical,
            guarded: Arc::clone(&self.guarded),
        }
    }

    /// Check whether `path` is currently guarded (i.e. being written by executor).
    pub fn is_guarded(&self, path: &Path) -> bool {
        self.guarded
            .lock()
            .map(|set| set.contains(path))
            .unwrap_or(false)
    }
}

/// RAII token: removes the path from the guarded set on drop.
pub struct WriteGuardToken {
    path: PathBuf,
    guarded: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Drop for WriteGuardToken {
    fn drop(&mut self) {
        if let Ok(mut set) = self.guarded.lock() {
            set.remove(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_and_release() {
        let wg = WriteGuard::new();
        let path = Path::new("/tmp/test.md");

        assert!(!wg.is_guarded(path));

        let token = wg.guard(path);
        assert!(wg.is_guarded(path));

        drop(token);
        assert!(!wg.is_guarded(path));
    }

    #[test]
    fn multiple_paths() {
        let wg = WriteGuard::new();
        let a = Path::new("/a.md");
        let b = Path::new("/b.md");

        let _ta = wg.guard(a);
        let _tb = wg.guard(b);

        assert!(wg.is_guarded(a));
        assert!(wg.is_guarded(b));
        assert!(!wg.is_guarded(Path::new("/c.md")));
    }

    #[test]
    fn clone_shares_state() {
        let wg = WriteGuard::new();
        let wg2 = wg.clone();
        let path = Path::new("/shared.md");

        let _token = wg.guard(path);
        assert!(wg2.is_guarded(path));
    }
}
