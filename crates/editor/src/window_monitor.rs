use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Grace period after tracking before we start checking.
/// Editors like Typora are launchers — the spawned child exits immediately,
/// and the actual editor window takes a few seconds to appear.
const GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Checks whether a file is currently open in any visible window.
pub trait FileOpenChecker: Send + Sync {
    fn is_file_open(&self, filename: &str) -> bool;
}

/// Production implementation: Win32 EnumWindows + GetWindowTextW.
#[cfg(windows)]
pub struct Win32FileOpenChecker;

#[cfg(windows)]
impl FileOpenChecker for Win32FileOpenChecker {
    fn is_file_open(&self, filename: &str) -> bool {
        is_file_in_any_window(filename)
    }
}

/// Enumerate all visible windows, return true if any title contains `filename`.
#[cfg(windows)]
pub fn is_file_in_any_window(filename: &str) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    let filename_lower = filename.to_lowercase();

    // We use a thread-local to pass the filename into the callback since
    // EnumWindows only gives us an LPARAM (integer).  AtomicBool is simpler.
    thread_local! {
        static FOUND: AtomicBool = AtomicBool::new(false);
        static NEEDLE: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());
    }

    FOUND.with(|f| f.store(false, Ordering::SeqCst));
    NEEDLE.with(|n| *n.borrow_mut() = filename_lower);

    unsafe extern "system" fn enum_callback(hwnd: HWND, _: LPARAM) -> windows::core::BOOL {
        use windows::Win32::Foundation::{FALSE, TRUE};
        use windows::Win32::UI::WindowsAndMessaging::*;

        let visible = IsWindowVisible(hwnd).as_bool();
        if !visible {
            return TRUE;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
            NEEDLE.with(|n| {
                if title.contains(n.borrow().as_str()) {
                    FOUND.with(|f| f.store(true, Ordering::SeqCst));
                }
            });
            if FOUND.with(|f| f.load(Ordering::SeqCst)) {
                return FALSE; // stop enumeration
            }
        }
        TRUE
    }

    unsafe {
        let _ = EnumWindows(Some(enum_callback), LPARAM(0));
    }

    FOUND.with(|f| f.load(Ordering::SeqCst))
}

/// A tracked file entry with its tracking timestamp.
struct TrackedFile {
    filename: String,
    tracked_at: Instant,
    /// Keep editor child handle alive to prevent launcher-spawned editors
    /// from misbehaving when the handle is dropped immediately.
    _child: Option<std::process::Child>,
}

/// Monitors tracked files and notifies when their editor windows close.
///
/// Generic over `FileOpenChecker` so that tests can inject a mock.
pub struct WindowMonitor<C: FileOpenChecker = DefaultChecker> {
    tracked: Arc<Mutex<HashMap<String, TrackedFile>>>, // doc_id → tracked file
    checker: Arc<C>,
    rename_tx: mpsc::UnboundedSender<Vec<String>>,
    grace_period: Duration,
}

/// Default checker picks the platform implementation.
#[cfg(windows)]
pub type DefaultChecker = Win32FileOpenChecker;
#[cfg(not(windows))]
pub type DefaultChecker = AlwaysClosedChecker;

/// Fallback for non-Windows: always reports file as closed.
#[cfg(not(windows))]
pub struct AlwaysClosedChecker;
#[cfg(not(windows))]
impl FileOpenChecker for AlwaysClosedChecker {
    fn is_file_open(&self, _filename: &str) -> bool {
        false
    }
}

impl WindowMonitor<DefaultChecker> {
    /// Create a monitor using the platform-native checker.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Vec<String>>) {
        #[cfg(windows)]
        let checker = Arc::new(Win32FileOpenChecker);
        #[cfg(not(windows))]
        let checker = Arc::new(AlwaysClosedChecker);

        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tracked: Arc::new(Mutex::new(HashMap::new())),
                checker,
                rename_tx: tx,
                grace_period: GRACE_PERIOD,
            },
            rx,
        )
    }
}

impl<C: FileOpenChecker> WindowMonitor<C> {
    /// Create a monitor with a custom checker (for testing).
    /// Uses zero grace period so tests don't need to wait.
    pub fn with_checker(checker: C) -> (Self, mpsc::UnboundedReceiver<Vec<String>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tracked: Arc::new(Mutex::new(HashMap::new())),
                checker: Arc::new(checker),
                rename_tx: tx,
                grace_period: Duration::ZERO,
            },
            rx,
        )
    }

    /// Start tracking a file opened in an editor.
    pub fn track(&self, doc_id: &str, filename: &str) {
        self.track_with_child(doc_id, filename, None);
    }

    /// Track a file and keep the editor's Child handle alive.
    /// Some editors (Typora) misbehave if the Child handle is dropped immediately.
    pub fn track_with_child(
        &self,
        doc_id: &str,
        filename: &str,
        child: Option<std::process::Child>,
    ) {
        if let Ok(mut map) = self.tracked.lock() {
            map.insert(
                doc_id.to_string(),
                TrackedFile {
                    filename: filename.to_string(),
                    tracked_at: Instant::now(),
                    _child: child,
                },
            );
        }
    }

    /// Stop tracking a file (e.g., on delete).
    pub fn untrack(&self, doc_id: &str) {
        if let Ok(mut map) = self.tracked.lock() {
            map.remove(doc_id);
        }
    }

    /// Run one check cycle: for each tracked file that has passed the grace
    /// period, ask the checker if it's still open. Files that are no longer
    /// open are removed from tracking and their doc_ids are sent through
    /// the channel.
    pub fn check_once(&self) {
        let closed: Vec<String> = {
            let map = match self.tracked.lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            map.iter()
                .filter(|(_, tf)| {
                    tf.tracked_at.elapsed() >= self.grace_period
                        && !self.checker.is_file_open(&tf.filename)
                })
                .map(|(doc_id, _)| doc_id.clone())
                .collect()
        };

        if closed.is_empty() {
            return;
        }

        // Remove closed entries
        if let Ok(mut map) = self.tracked.lock() {
            for id in &closed {
                map.remove(id);
            }
        }

        let _ = self.rename_tx.send(closed);
    }

    /// Spawn a background task that calls `check_once` at a fixed interval.
    pub fn spawn_polling(self: Arc<Self>, interval: std::time::Duration)
    where
        C: 'static,
    {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                self.check_once();
            }
        });
    }
}

// ─── Mock for tests ──────────────────────────────────

/// Mock checker for unit tests. Allows programmatic control of which files
/// are "open" in a window.
#[derive(Clone)]
pub struct MockFileOpenChecker {
    open_files: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl MockFileOpenChecker {
    pub fn new() -> Self {
        Self {
            open_files: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub fn mark_open(&self, filename: &str) {
        self.open_files.lock().unwrap().insert(filename.to_string());
    }

    pub fn mark_closed(&self, filename: &str) {
        self.open_files.lock().unwrap().remove(filename);
    }
}

impl FileOpenChecker for MockFileOpenChecker {
    fn is_file_open(&self, filename: &str) -> bool {
        self.open_files.lock().unwrap().contains(filename)
    }
}

// ─── Tests ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_and_detect_close() {
        let mock = MockFileOpenChecker::new();
        mock.mark_open("test.md");

        let (monitor, mut rx) = WindowMonitor::with_checker(mock.clone());
        monitor.track("doc1", "test.md");

        // File is open — should NOT trigger
        monitor.check_once();
        assert!(rx.try_recv().is_err(), "Should not fire while file is open");

        // Simulate close
        mock.mark_closed("test.md");
        monitor.check_once();

        let closed = rx.try_recv().expect("Should receive close event");
        assert_eq!(closed, vec!["doc1"]);

        // Should not fire again (already removed from tracking)
        monitor.check_once();
        assert!(rx.try_recv().is_err(), "Should not fire twice");
    }

    #[test]
    fn test_untrack_prevents_rename() {
        let mock = MockFileOpenChecker::new();
        mock.mark_open("test.md");

        let (monitor, mut rx) = WindowMonitor::with_checker(mock.clone());
        monitor.track("doc1", "test.md");
        monitor.untrack("doc1");

        mock.mark_closed("test.md");
        monitor.check_once();

        assert!(
            rx.try_recv().is_err(),
            "Untracked file should not trigger rename"
        );
    }

    #[test]
    fn test_multiple_files_independent() {
        let mock = MockFileOpenChecker::new();
        mock.mark_open("a.md");
        mock.mark_open("b.md");

        let (monitor, mut rx) = WindowMonitor::with_checker(mock.clone());
        monitor.track("doc_a", "a.md");
        monitor.track("doc_b", "b.md");

        // Close only a
        mock.mark_closed("a.md");
        monitor.check_once();

        let closed = rx.try_recv().expect("Should detect a.md close");
        assert_eq!(closed, vec!["doc_a"]);

        // b is still open
        monitor.check_once();
        assert!(rx.try_recv().is_err(), "b.md still open");

        // Now close b
        mock.mark_closed("b.md");
        monitor.check_once();

        let closed = rx.try_recv().expect("Should detect b.md close");
        assert_eq!(closed, vec!["doc_b"]);
    }

    #[test]
    fn test_track_overwrites_previous() {
        let mock = MockFileOpenChecker::new();
        mock.mark_open("old.md");
        mock.mark_open("new.md");

        let (monitor, mut rx) = WindowMonitor::with_checker(mock.clone());
        monitor.track("doc1", "old.md");
        monitor.track("doc1", "new.md"); // overwrite

        // Closing old.md should NOT trigger (no longer tracked)
        mock.mark_closed("old.md");
        monitor.check_once();
        assert!(rx.try_recv().is_err());

        // Closing new.md should trigger
        mock.mark_closed("new.md");
        monitor.check_once();
        let closed = rx.try_recv().expect("Should detect new.md close");
        assert_eq!(closed, vec!["doc1"]);
    }

    #[test]
    fn test_empty_tracking_no_events() {
        let mock = MockFileOpenChecker::new();
        let (monitor, mut rx) = WindowMonitor::with_checker(mock);
        monitor.check_once();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_file_never_opened_triggers_immediately() {
        // File is tracked but was never marked open → checker returns false → triggers
        // (with_checker uses zero grace period for tests)
        let mock = MockFileOpenChecker::new();
        let (monitor, mut rx) = WindowMonitor::with_checker(mock);
        monitor.track("doc1", "never_opened.md");

        monitor.check_once();
        let closed = rx.try_recv().expect("Should trigger for file not in any window");
        assert_eq!(closed, vec!["doc1"]);
    }

    #[test]
    fn test_grace_period_prevents_premature_detection() {
        // Simulate the Typora scenario: file tracked, but no window yet
        // (editor launcher exits before window appears)
        let mock = MockFileOpenChecker::new();
        // Don't mark_open — simulates the gap between launcher exit and window appearing

        let (tx, rx) = mpsc::unbounded_channel();
        let monitor = WindowMonitor {
            tracked: Arc::new(Mutex::new(HashMap::new())),
            checker: Arc::new(mock.clone()),
            rename_tx: tx,
            grace_period: Duration::from_secs(10), // 10 second grace period
        };
        let mut rx = rx;

        monitor.track("doc1", "test.md");

        // Immediately check — should NOT trigger because grace period hasn't elapsed
        monitor.check_once();
        assert!(
            rx.try_recv().is_err(),
            "Should not fire during grace period even if file not in any window"
        );
    }

    // ─── Integration test with real window (Windows only) ───

    #[cfg(windows)]
    #[test]
    #[ignore] // Requires GUI environment, skip in headless CI
    fn test_real_window_detection() {
        use std::process::Command;
        use std::time::Duration;

        // Use a unique filename to avoid interference from other tests
        let unique = format!("larknotes_wintest_{}.md", std::process::id());
        let tmp = std::env::temp_dir().join(&unique);
        std::fs::write(&tmp, "# Test").unwrap();

        // Open in notepad (on modern Windows, notepad is a launcher too —
        // the spawned child may exit immediately, real notepad is a separate PID)
        let _ = Command::new("notepad.exe").arg(&tmp).spawn().unwrap();
        std::thread::sleep(Duration::from_secs(3));

        let checker = Win32FileOpenChecker;
        assert!(
            checker.is_file_open(&unique),
            "Should detect notepad has the file open"
        );

        // Kill ALL notepad instances whose window title contains our unique filename.
        // We must use taskkill /IM because the spawned PID is a launcher, not the real notepad.
        let _ = Command::new("taskkill")
            .args(["/IM", "notepad.exe", "/F"])
            .output();
        std::thread::sleep(Duration::from_secs(2));

        assert!(
            !checker.is_file_open(&unique),
            "Should detect file is no longer open after notepad killed"
        );

        let _ = std::fs::remove_file(&tmp);
    }
}
