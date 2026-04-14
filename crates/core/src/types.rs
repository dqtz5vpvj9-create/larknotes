use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;

// ─── Hash newtypes (P5: never mix local/remote hash domains) ─────

/// SHA-256 hash of decoded local file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHash(pub String);

/// SHA-256 hash of remote content from provider.read().
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHash(pub String);

// ─── Note identity ──────────────────────────────────────────────

/// Generate a new local note identity (UUID v4).
pub fn new_note_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMeta {
    /// Local immutable identity (UUID). Primary key in notes table.
    pub note_id: String,
    /// Remote document ID (e.g. Lark doc_id). Nullable, replaceable.
    pub remote_id: Option<String>,
    /// Backward compat with frontend — always mirrors note_id.
    /// NEVER set to remote_id; use the `remote_id` field for remote identity.
    #[serde(default)]
    pub doc_id: String,
    pub title: String,
    pub doc_type: String,
    pub url: String,
    pub owner_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub local_path: Option<String>,
    pub content_hash: Option<String>,
    pub sync_status: SyncStatus,
    /// Relative folder path under docs/, e.g. "项目A/周报". Empty string = root.
    #[serde(default)]
    pub folder_path: String,
    /// File size in bytes (computed, not stored in DB).
    #[serde(default)]
    pub file_size: Option<u64>,
    /// Word count (computed, not stored in DB). Uses UAX#29 segmentation.
    #[serde(default)]
    pub word_count: Option<usize>,
    /// Sync state in the new architecture.
    #[serde(default)]
    pub sync_state: SyncState,
    /// Title derivation mode for quick notes.
    #[serde(default = "default_title_mode")]
    pub title_mode: String,
    /// Desired title for pending rename operations. None = no rename desired.
    #[serde(default)]
    pub desired_title: Option<String>,
    /// Desired local path for pending move operations. None = no move desired.
    #[serde(default)]
    pub desired_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInfo {
    pub folder_path: String,
    pub remote_id: Option<String>,
}

/// Content + metadata returned by `DocProvider::read()`.
#[derive(Debug, Clone)]
pub struct ReadOutput {
    pub content: String,
    pub meta: DocMeta,
}

/// Post-write metadata returned by `DocProvider::write()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteMeta {
    pub content_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "message")]
pub enum SyncStatus {
    Synced,
    LocalModified,
    RemoteModified,
    BothModified,
    Syncing,
    Pulling,
    Conflict,
    Error(String),
    #[default]
    New,
}

fn default_title_mode() -> String {
    "manual".to_string()
}

/// New sync state machine (P4 architecture).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum SyncState {
    #[default]
    Synced,
    LocalModified,
    RemoteModified,
    BothModified,
    Executing,
    Conflict,
    PendingCreate,
    PendingDelete,
    PendingRename,
    Error(String),
    FileMissing,
}

impl std::fmt::Display for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(msg) => write!(f, "Error:{msg}"),
            other => write!(f, "{:?}", other),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub user_name: Option<String>,
    pub expires_at: Option<String>,
    pub needs_refresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub workspace_dir: PathBuf,
    pub editor_command: String,
    pub provider_cli_path: String,
    pub sync_debounce_ms: u64,
    pub auto_sync: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let workspace_dir = dirs::document_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("LarkNotes");
        Self {
            workspace_dir,
            editor_command: "notepad".to_string(),
            provider_cli_path: "lark-cli".to_string(),
            sync_debounce_ms: 2000,
            auto_sync: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncHistoryEntry {
    pub id: i64,
    pub note_id: String,
    /// Legacy alias — mirrors note_id for frontend compat.
    #[serde(default)]
    pub doc_id: String,
    pub action: String,
    pub content_hash: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSnapshot {
    pub id: i64,
    pub note_id: String,
    /// Legacy alias — mirrors note_id for frontend compat.
    #[serde(default)]
    pub doc_id: String,
    pub content: String,
    pub content_hash: String,
    pub created_at: String,
}

// ─── File layout ─────────────────────────────────────────────
// workspace/
//   docs/              ← user-visible: <title>.md files, may contain subfolders
//     project-a/       ← subfolder (tracked in `folders` table)
//       note.md
//   .meta/             ← hidden: <doc_id>.json mapping files
//   app.db             ← SQLite metadata

/// Returns the docs directory: `workspace/docs/`
pub fn docs_dir(workspace: &Path) -> PathBuf {
    workspace.join("docs")
}

/// Returns the content file path: `workspace/docs/[folder/]<title>.md`
/// This is a pure mapping — does NOT check for duplicates.
/// Pass `folder = ""` for the root docs directory.
pub fn titled_content_path(workspace: &Path, title: &str) -> PathBuf {
    titled_content_path_in(workspace, "", title)
}

/// Like `titled_content_path` but within a specific subfolder.
pub fn titled_content_path_in(workspace: &Path, folder: &str, title: &str) -> PathBuf {
    let safe_name = sanitize_filename(title);
    let dir = if folder.is_empty() {
        docs_dir(workspace)
    } else {
        docs_dir(workspace).join(folder)
    };
    dir.join(format!("{safe_name}.md"))
}

/// Returns a unique content file path, appending ` (2)`, ` (3)` etc. if the
/// file already exists — matching Windows Explorer behaviour.
/// Pass `folder = ""` for the root docs directory.
pub fn unique_content_path(workspace: &Path, title: &str) -> PathBuf {
    unique_content_path_in(workspace, "", title)
}

/// Like `unique_content_path` but within a specific subfolder.
pub fn unique_content_path_in(workspace: &Path, folder: &str, title: &str) -> PathBuf {
    let safe_name = sanitize_filename(title);
    let dir = if folder.is_empty() {
        docs_dir(workspace)
    } else {
        docs_dir(workspace).join(folder)
    };
    let base = dir.join(format!("{safe_name}.md"));
    if !base.exists() {
        return base;
    }
    for n in 2..=999 {
        let candidate = dir.join(format!("{safe_name} ({n}).md"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Extremely unlikely fallback
    dir.join(format!(
        "{safe_name} ({}).md",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ))
}

/// Extract the relative folder path of a file under `docs/`.
/// Returns `""` if the file is directly in `docs/`.
/// Example: `docs/project-a/note.md` → `"project-a"`
pub fn folder_of(workspace: &Path, file_path: &Path) -> String {
    let docs = docs_dir(workspace);
    if let Ok(rel) = file_path.strip_prefix(&docs) {
        if let Some(parent) = rel.parent() {
            let s = parent.to_string_lossy().replace('\\', "/");
            return s.to_string();
        }
    }
    String::new()
}

/// Sanitize a string to be safe as a filename.
pub fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let sanitized = sanitized.trim().to_string();
    if sanitized.is_empty() {
        "Untitled".to_string()
    } else {
        sanitized
    }
}

/// Returns the meta directory: `workspace/.meta/`
pub fn meta_dir(workspace: &Path) -> PathBuf {
    workspace.join(".meta")
}

/// Returns the meta file path: `workspace/.meta/<id>.json`
/// Accepts either note_id or doc_id (legacy).
pub fn meta_path(workspace: &Path, id: &str) -> PathBuf {
    meta_dir(workspace).join(format!("{id}.json"))
}

/// Extract title from markdown content (first H1 line)
pub fn extract_title(content: &str) -> String {
    content
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string())
}

/// Count words in text using UAX#29 word segmentation.
/// Handles CJK (each ideograph = 1 word), Latin, Cyrillic, Arabic, etc.
pub fn count_words(text: &str) -> usize {
    text.unicode_words().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_titled_content_path() {
        let p = titled_content_path(Path::new("/workspace"), "Hello World");
        assert_eq!(p, PathBuf::from("/workspace/docs/Hello World.md"));
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("hello/world"), "hello_world");
        assert_eq!(sanitize_filename("test:doc"), "test_doc");
        assert_eq!(sanitize_filename(""), "Untitled");
        assert_eq!(sanitize_filename("  "), "Untitled");
        assert_eq!(sanitize_filename("正常标题"), "正常标题");
    }

    #[test]
    fn test_meta_path() {
        let p = meta_path(Path::new("/workspace"), "abc123");
        assert_eq!(p, PathBuf::from("/workspace/.meta/abc123.json"));
    }

    #[test]
    fn test_unique_content_path() {
        let tmp = std::env::temp_dir().join("larknotes_test_unique");
        let docs = tmp.join("docs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&docs).unwrap();

        // First: no collision
        let p1 = unique_content_path(&tmp, "未命名");
        assert_eq!(p1, docs.join("未命名.md"));

        // Create the file
        std::fs::write(&p1, "# 未命名").unwrap();

        // Second: should get (2)
        let p2 = unique_content_path(&tmp, "未命名");
        assert_eq!(p2, docs.join("未命名 (2).md"));

        // Create (2), third should get (3)
        std::fs::write(&p2, "# 未命名").unwrap();
        let p3 = unique_content_path(&tmp, "未命名");
        assert_eq!(p3, docs.join("未命名 (3).md"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_extract_title() {
        assert_eq!(extract_title("# Hello World\n\nBody"), "Hello World");
        assert_eq!(extract_title("No heading here"), "Untitled");
        assert_eq!(extract_title("## Not H1\n# Actual Title"), "Actual Title");
    }

    // ─── New tests ───────────────────────────────────────

    #[test]
    fn test_sync_status_default() {
        assert_eq!(SyncStatus::default(), SyncStatus::New);
    }

    #[test]
    fn test_sync_status_error_serde_roundtrip() {
        let status = SyncStatus::Error("网络异常".to_string());
        let json = serde_json::to_string(&status).unwrap();
        let parsed: SyncStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn test_sync_status_all_variants_serde() {
        let variants = vec![
            SyncStatus::Synced,
            SyncStatus::LocalModified,
            SyncStatus::RemoteModified,
            SyncStatus::BothModified,
            SyncStatus::Syncing,
            SyncStatus::Pulling,
            SyncStatus::Conflict,
            SyncStatus::Error("test".into()),
            SyncStatus::New,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: SyncStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn test_app_config_default() {
        let cfg = AppConfig::default();
        assert!(cfg.workspace_dir.to_string_lossy().contains("LarkNotes"));
        assert_eq!(cfg.editor_command, "notepad");
        assert_eq!(cfg.provider_cli_path, "lark-cli");
        assert_eq!(cfg.sync_debounce_ms, 2000);
        assert!(cfg.auto_sync);
    }

    #[test]
    fn test_count_words_english() {
        assert_eq!(count_words("Hello world"), 2);
        assert_eq!(count_words("one two three four five"), 5);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("   "), 0);
    }

    #[test]
    fn test_count_words_chinese() {
        // Each CJK ideograph counts as one word per UAX#29
        assert_eq!(count_words("你好世界"), 4);
        assert_eq!(count_words("测试文档内容"), 6);
    }

    #[test]
    fn test_count_words_mixed() {
        // Mixed CJK + Latin
        let count = count_words("Hello 你好 world 世界");
        assert_eq!(count, 6); // Hello, 你, 好, world, 世, 界
    }

    #[test]
    fn test_count_words_markdown() {
        let md = "# Title\n\nSome **bold** text and `code`.";
        // Words: Title, Some, bold, text, and, code = 6
        assert!(count_words(md) >= 6);
    }

    #[test]
    fn test_sanitize_filename_all_special() {
        // 8 special chars → 8 underscores
        assert_eq!(sanitize_filename("/:*?\"<>|"), "________");
    }

    #[test]
    fn test_sanitize_filename_chinese() {
        // Fullwidth colon ： is NOT ascii colon : — it passes through
        assert_eq!(sanitize_filename("飞书笔记：测试"), "飞书笔记：测试");
        // ASCII colon IS replaced
        assert_eq!(sanitize_filename("飞书笔记:测试"), "飞书笔记_测试");
    }

    #[test]
    fn test_sanitize_filename_emoji() {
        // Emoji should pass through
        assert_eq!(sanitize_filename("笔记📝"), "笔记📝");
    }

    #[test]
    fn test_sanitize_filename_long_name() {
        let long = "a".repeat(300);
        let result = sanitize_filename(&long);
        assert_eq!(result.len(), 300); // no truncation — filesystem will handle limits
    }

    #[test]
    fn test_sanitize_filename_leading_trailing_spaces() {
        assert_eq!(sanitize_filename("  hello  "), "hello");
    }

    #[test]
    fn test_sanitize_filename_dots() {
        assert_eq!(sanitize_filename("..."), "...");
        assert_eq!(sanitize_filename(".hidden"), ".hidden");
    }

    #[test]
    fn test_sanitize_filename_backslash() {
        assert_eq!(sanitize_filename("path\\file"), "path_file");
    }

    #[test]
    fn test_extract_title_empty() {
        assert_eq!(extract_title(""), "Untitled");
    }

    #[test]
    fn test_extract_title_whitespace_only() {
        assert_eq!(extract_title("   \n\n  "), "Untitled");
    }

    #[test]
    fn test_extract_title_h1_with_extra_spaces() {
        assert_eq!(extract_title("#   Spaced Title  "), "Spaced Title");
    }

    #[test]
    fn test_extract_title_multiple_h1() {
        // First H1 wins
        assert_eq!(extract_title("# First\n# Second"), "First");
    }

    #[test]
    fn test_extract_title_h1_with_special_chars() {
        assert_eq!(extract_title("# Title: 测试 & 验证"), "Title: 测试 & 验证");
    }

    #[test]
    fn test_extract_title_h1_no_space() {
        // "## " and "#x" don't match "# "
        assert_eq!(extract_title("#NoSpace"), "Untitled");
    }

    #[test]
    fn test_extract_title_h1_after_body() {
        assert_eq!(extract_title("some text\n\n# Late Title"), "Late Title");
    }

    #[test]
    fn test_docs_dir() {
        assert_eq!(docs_dir(Path::new("/w")), PathBuf::from("/w/docs"));
    }

    #[test]
    fn test_meta_dir() {
        assert_eq!(meta_dir(Path::new("/w")), PathBuf::from("/w/.meta"));
    }

    #[test]
    fn test_titled_content_path_special_chars() {
        let p = titled_content_path(Path::new("/w"), "test:doc");
        assert_eq!(p, PathBuf::from("/w/docs/test_doc.md"));
    }

    #[test]
    fn test_titled_content_path_empty_title() {
        let p = titled_content_path(Path::new("/w"), "");
        assert_eq!(p, PathBuf::from("/w/docs/Untitled.md"));
    }

    #[test]
    fn test_titled_content_path_in_subfolder() {
        let p = titled_content_path_in(Path::new("/w"), "project-a", "note");
        assert_eq!(p, PathBuf::from("/w/docs/project-a/note.md"));
    }

    #[test]
    fn test_titled_content_path_in_root() {
        let p = titled_content_path_in(Path::new("/w"), "", "note");
        assert_eq!(p, PathBuf::from("/w/docs/note.md"));
    }

    #[test]
    fn test_folder_of_root() {
        let f = folder_of(Path::new("/w"), Path::new("/w/docs/note.md"));
        assert_eq!(f, "");
    }

    #[test]
    fn test_folder_of_subfolder() {
        let f = folder_of(Path::new("/w"), Path::new("/w/docs/project-a/note.md"));
        assert_eq!(f, "project-a");
    }

    #[test]
    fn test_folder_of_nested() {
        let f = folder_of(Path::new("/w"), Path::new("/w/docs/a/b/c/note.md"));
        assert_eq!(f, "a/b/c");
    }
}
