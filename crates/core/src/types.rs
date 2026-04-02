use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMeta {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "message")]
pub enum SyncStatus {
    Synced,
    LocalModified,
    Syncing,
    Conflict,
    Error(String),
    New,
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::New
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
    pub lark_cli_path: String,
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
            lark_cli_path: "lark-cli".to_string(),
            sync_debounce_ms: 2000,
            auto_sync: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncHistoryEntry {
    pub id: i64,
    pub doc_id: String,
    pub action: String,
    pub content_hash: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSnapshot {
    pub id: i64,
    pub doc_id: String,
    pub content: String,
    pub content_hash: String,
    pub created_at: String,
}

// ─── Flat file layout ───────────────────────────────────────
// workspace/
//   docs/          ← user-visible: <title>.md files live here
//   .meta/         ← hidden: <doc_id>.json mapping files
//   app.db         ← SQLite metadata

/// Returns the docs directory: `workspace/docs/`
pub fn docs_dir(workspace: &Path) -> PathBuf {
    workspace.join("docs")
}

/// Returns the content file path: `workspace/docs/<title>.md`
/// This is a pure mapping — does NOT check for duplicates.
pub fn titled_content_path(workspace: &Path, title: &str) -> PathBuf {
    let safe_name = sanitize_filename(title);
    docs_dir(workspace).join(format!("{safe_name}.md"))
}

/// Returns a unique content file path, appending ` (2)`, ` (3)` etc. if the
/// file already exists — matching Windows Explorer behaviour.
pub fn unique_content_path(workspace: &Path, title: &str) -> PathBuf {
    let safe_name = sanitize_filename(title);
    let dir = docs_dir(workspace);
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

/// Returns the meta file path: `workspace/.meta/<doc_id>.json`
pub fn meta_path(workspace: &Path, doc_id: &str) -> PathBuf {
    meta_dir(workspace).join(format!("{doc_id}.json"))
}

/// Extract title from markdown content (first H1 line)
pub fn extract_title(content: &str) -> String {
    content
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string())
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
}
