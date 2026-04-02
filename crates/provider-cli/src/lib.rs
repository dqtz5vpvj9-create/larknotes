use larknotes_core::*;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

pub struct CliProvider {
    cli_path: String,
    /// On Windows, resolved node.exe + script path to bypass cmd.exe arg mangling
    #[cfg(windows)]
    resolved: Option<(PathBuf, PathBuf)>,
}

#[cfg(windows)]
fn resolve_cmd_shim(cli_path: &str) -> Option<(PathBuf, PathBuf)> {
    // Find the .cmd file on PATH
    let which_output = std::process::Command::new("cmd")
        .args(["/C", "where", cli_path])
        .creation_flags(0x08000000)
        .output()
        .ok()?;
    let where_out = String::from_utf8_lossy(&which_output.stdout);
    let cmd_path = where_out.lines().find(|l| l.ends_with(".cmd"))?;
    let cmd_path = std::path::Path::new(cmd_path.trim());

    // Read the .cmd file content to find the node script
    let content = std::fs::read_to_string(cmd_path).ok()?;

    // Look for pattern: node "basedir/node_modules/...run.js"
    // npm .cmd files typically have: "%dp0%\node.exe" "%dp0%\node_modules\...\run.js" %*
    for line in content.lines() {
        if let Some(idx) = line.find("node_modules") {
            // Extract the script path portion
            let rest = &line[idx..];
            let script_rel = rest
                .trim_end_matches('"')
                .trim_end_matches(" %*")
                .trim_end_matches('"');
            let base_dir = cmd_path.parent()?;
            let script_path = base_dir.join(script_rel);
            if script_path.exists() {
                // Find node.exe — either in same dir or on PATH
                let node_path = base_dir.join("node.exe");
                let node_path = if node_path.exists() {
                    node_path
                } else {
                    PathBuf::from("node")
                };
                tracing::info!(
                    "resolved lark-cli shim: node={} script={}",
                    node_path.display(),
                    script_path.display()
                );
                return Some((node_path, script_path));
            }
        }
    }
    None
}

impl CliProvider {
    pub fn new(cli_path: &str) -> Self {
        #[cfg(windows)]
        let resolved = resolve_cmd_shim(cli_path);

        Self {
            cli_path: cli_path.to_string(),
            #[cfg(windows)]
            resolved,
        }
    }

    async fn run_cli(&self, args: &[&str]) -> Result<serde_json::Value, LarkNotesError> {
        tracing::debug!("lark-cli {}", args.join(" "));

        // On Windows, call node directly to avoid cmd.exe mangling multiline args
        #[cfg(windows)]
        let output = {
            if let Some((node_path, script_path)) = &self.resolved {
                let mut cmd = Command::new(node_path);
                cmd.arg(script_path)
                    .args(args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(0x08000000);
                cmd.output()
                    .await
                    .map_err(|e| LarkNotesError::Cli(format!("启动lark-cli失败: {e}")))?
            } else {
                // Fallback to cmd /C for simple commands
                let mut cmd_args = vec!["/C", &self.cli_path as &str];
                cmd_args.extend_from_slice(args);
                Command::new("cmd")
                    .args(&cmd_args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(0x08000000)
                    .output()
                    .await
                    .map_err(|e| LarkNotesError::Cli(format!("启动lark-cli失败: {e}")))?
            }
        };

        #[cfg(not(windows))]
        let output = Command::new(&self.cli_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| LarkNotesError::Cli(format!("启动lark-cli失败: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            tracing::warn!("lark-cli stderr: {stderr}");
        }

        let json: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| LarkNotesError::Cli(format!("解析输出失败: {e}\nstdout: {stdout}")))?;

        // Check the "ok" field if present
        if let Some(ok) = json.get("ok") {
            if ok == false {
                let msg = json
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                return Err(LarkNotesError::Cli(msg.to_string()));
            }
        }

        Ok(json)
    }
}

fn strip_highlight(s: &str) -> String {
    s.replace("<h>", "").replace("</h>", "")
}

/// Unescape markdown that lark-cli returns with backslash-escaped syntax.
/// e.g. `\*\*bold\*\*` → `**bold**`, `\~\~strike\~\~` → `~~strike~~`
fn unescape_markdown(s: &str) -> String {
    s.replace("\\*", "*")
        .replace("\\~", "~")
        .replace("\\`", "`")
        .replace("\\[", "[")
        .replace("\\]", "]")
        .replace("\\#", "#")
        .replace("\\>", ">")
        .replace("\\-", "-")
        .replace("\\_", "_")
}

/// Parse auth status from CLI JSON output.
pub fn parse_auth_status(json: &serde_json::Value) -> AuthStatus {
    let token_status = json
        .get("tokenStatus")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let logged_in = token_status == "valid" || token_status == "needs_refresh";

    AuthStatus {
        logged_in,
        user_name: json.get("userName").and_then(|v| v.as_str()).map(String::from),
        expires_at: json.get("expiresAt").and_then(|v| v.as_str()).map(String::from),
        needs_refresh: token_status == "needs_refresh",
    }
}

/// Parse search results from CLI JSON output.
pub fn parse_search_results(json: &serde_json::Value) -> Vec<DocMeta> {
    let results = json
        .pointer("/data/results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut docs = Vec::new();
    for item in &results {
        let meta = item.get("result_meta").unwrap_or(item);
        let doc_id = meta
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = item
            .get("title_highlighted")
            .and_then(|v| v.as_str())
            .map(strip_highlight)
            .unwrap_or_else(|| "Untitled".to_string());
        let url = meta
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let owner_name = meta
            .get("owner_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let doc_type = meta
            .get("doc_types")
            .and_then(|v| v.as_str())
            .unwrap_or("DOCX")
            .to_string();
        let created_at = meta
            .get("create_time_iso")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let updated_at = meta
            .get("update_time_iso")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        docs.push(DocMeta {
            doc_id,
            title,
            doc_type,
            url,
            owner_name,
            created_at,
            updated_at,
            local_path: None,
            content_hash: None,
            sync_status: SyncStatus::New,
        });
    }

    docs
}

/// Parse create doc response from CLI JSON output.
pub fn parse_create_response(
    json: &serde_json::Value,
    title: &str,
) -> Result<DocMeta, LarkNotesError> {
    let doc_id = json
        .pointer("/data/doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LarkNotesError::Cli("返回中缺少doc_id".to_string()))?
        .to_string();
    let url = json
        .pointer("/data/doc_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let now = chrono::Local::now().to_rfc3339();
    Ok(DocMeta {
        doc_id,
        title: title.to_string(),
        doc_type: "DOCX".to_string(),
        url,
        owner_name: String::new(),
        created_at: now.clone(),
        updated_at: now,
        local_path: None,
        content_hash: None,
        sync_status: SyncStatus::Synced,
    })
}

/// Parse fetch doc response — returns markdown content with unescaped syntax.
pub fn parse_fetch_response(json: &serde_json::Value) -> String {
    let raw = json.pointer("/data/markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    unescape_markdown(raw)
}

#[async_trait::async_trait]
impl DocProvider for CliProvider {
    async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError> {
        let json = self.run_cli(&["auth", "status"]).await?;
        Ok(parse_auth_status(&json))
    }

    async fn search_docs(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+search", "--query", query, "--format", "json"])
            .await?;
        Ok(parse_search_results(&json))
    }

    async fn create_doc(&self, title: &str, markdown: &str) -> Result<DocMeta, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+create", "--title", title, "--markdown", markdown])
            .await?;
        parse_create_response(&json, title)
    }

    async fn fetch_doc(&self, doc_id: &str) -> Result<String, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+fetch", "--doc", doc_id, "--format", "json"])
            .await?;
        Ok(parse_fetch_response(&json))
    }

    async fn update_doc(&self, doc_id: &str, markdown: &str) -> Result<(), LarkNotesError> {
        self.run_cli(&[
            "docs",
            "+update",
            "--doc",
            doc_id,
            "--mode",
            "overwrite",
            "--markdown",
            markdown,
        ])
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Unit tests: parse functions with fixture JSON ==========

    #[test]
    fn test_strip_highlight() {
        assert_eq!(strip_highlight("<h>LarkNotes</h>测试"), "LarkNotes测试");
        assert_eq!(strip_highlight("no tags"), "no tags");
        assert_eq!(strip_highlight("<h>a</h> <h>b</h>"), "a b");
        assert_eq!(strip_highlight(""), "");
    }

    #[test]
    fn test_parse_auth_status_valid() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "appId": "cli_a9449b2804615bd1",
            "brand": "feishu",
            "tokenStatus": "valid",
            "userName": "李新锐",
            "expiresAt": "2026-04-02T21:10:54+08:00",
            "userOpenId": "ou_507c656d960a2b496a0a63d436bb205e"
        }"#).unwrap();

        let status = parse_auth_status(&json);
        assert!(status.logged_in);
        assert_eq!(status.user_name.as_deref(), Some("李新锐"));
        assert_eq!(status.expires_at.as_deref(), Some("2026-04-02T21:10:54+08:00"));
        assert!(!status.needs_refresh);
    }

    #[test]
    fn test_parse_auth_status_needs_refresh() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "tokenStatus": "needs_refresh",
            "userName": "test"
        }"#).unwrap();

        let status = parse_auth_status(&json);
        assert!(status.logged_in);
        assert!(status.needs_refresh);
    }

    #[test]
    fn test_parse_auth_status_expired() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "tokenStatus": "expired"
        }"#).unwrap();

        let status = parse_auth_status(&json);
        assert!(!status.logged_in);
        assert!(status.user_name.is_none());
    }

    #[test]
    fn test_parse_auth_status_empty() {
        let json: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        let status = parse_auth_status(&json);
        assert!(!status.logged_in);
    }

    #[test]
    fn test_parse_search_results_with_results() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {
                "has_more": false,
                "results": [
                    {
                        "entity_type": "DOC",
                        "result_meta": {
                            "token": "ENX3dkjCjoSbIRxPtRYcONgmnGh",
                            "url": "https://feishu.cn/docx/ENX3dkjCjoSbIRxPtRYcONgmnGh",
                            "owner_name": "李新锐",
                            "doc_types": "DOCX",
                            "create_time_iso": "2026-04-02T19:32:00+08:00",
                            "update_time_iso": "2026-04-02T19:32:00+08:00"
                        },
                        "title_highlighted": "\u003ch\u003eLarkNotes\u003c/h\u003e测试"
                    }
                ],
                "total": 1
            }
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].doc_id, "ENX3dkjCjoSbIRxPtRYcONgmnGh");
        assert_eq!(docs[0].title, "LarkNotes测试");
        assert_eq!(docs[0].owner_name, "李新锐");
        assert_eq!(docs[0].doc_type, "DOCX");
        assert_eq!(docs[0].url, "https://feishu.cn/docx/ENX3dkjCjoSbIRxPtRYcONgmnGh");
        assert_eq!(docs[0].created_at, "2026-04-02T19:32:00+08:00");
    }

    #[test]
    fn test_parse_search_results_empty() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": { "results": [], "total": 0 }
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert!(docs.is_empty());
    }

    #[test]
    fn test_parse_search_results_no_results_key() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {}
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert!(docs.is_empty());
    }

    #[test]
    fn test_parse_search_results_multiple() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {
                "results": [
                    {
                        "result_meta": { "token": "doc1", "url": "", "owner_name": "", "doc_types": "DOCX", "create_time_iso": "", "update_time_iso": "" },
                        "title_highlighted": "First"
                    },
                    {
                        "result_meta": { "token": "doc2", "url": "", "owner_name": "", "doc_types": "DOCX", "create_time_iso": "", "update_time_iso": "" },
                        "title_highlighted": "<h>Second</h>"
                    }
                ]
            }
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].doc_id, "doc1");
        assert_eq!(docs[0].title, "First");
        assert_eq!(docs[1].doc_id, "doc2");
        assert_eq!(docs[1].title, "Second");
    }

    #[test]
    fn test_parse_create_response_success() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {
                "doc_id": "VT5rd9n3WoAVKkxkT5Kc3XyQnJh",
                "doc_url": "https://www.feishu.cn/docx/VT5rd9n3WoAVKkxkT5Kc3XyQnJh",
                "message": "文档创建成功"
            }
        }"#).unwrap();

        let meta = parse_create_response(&json, "IntegrationTest").unwrap();
        assert_eq!(meta.doc_id, "VT5rd9n3WoAVKkxkT5Kc3XyQnJh");
        assert_eq!(meta.title, "IntegrationTest");
        assert_eq!(meta.url, "https://www.feishu.cn/docx/VT5rd9n3WoAVKkxkT5Kc3XyQnJh");
        assert_eq!(meta.doc_type, "DOCX");
        assert_eq!(meta.sync_status, SyncStatus::Synced);
        assert!(!meta.created_at.is_empty());
    }

    #[test]
    fn test_parse_create_response_missing_doc_id() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {}
        }"#).unwrap();

        let result = parse_create_response(&json, "Test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("doc_id"));
    }

    #[test]
    fn test_parse_fetch_response_with_content() {
        let mut data = serde_json::Map::new();
        data.insert("markdown".to_string(), serde_json::Value::String("# Hello\n\nWorld".to_string()));
        data.insert("title".to_string(), serde_json::Value::String("Hello".to_string()));
        let json = serde_json::json!({ "ok": true, "data": data });

        let md = parse_fetch_response(&json);
        assert_eq!(md, "# Hello\n\nWorld");
    }

    #[test]
    fn test_parse_fetch_response_empty_content() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": { "markdown": "", "title": "Empty" }
        }"#).unwrap();

        let md = parse_fetch_response(&json);
        assert_eq!(md, "");
    }

    #[test]
    fn test_parse_fetch_response_missing_markdown() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {}
        }"#).unwrap();

        let md = parse_fetch_response(&json);
        assert_eq!(md, "");
    }

    // ========== Live integration tests (require lark-cli auth) ==========

    #[tokio::test]
    #[ignore] // Run with: cargo test -p larknotes-provider-cli -- --ignored
    async fn test_live_auth_status() {
        let provider = CliProvider::new("lark-cli");
        let status = provider.auth_status().await.unwrap();
        assert!(status.logged_in, "Expected logged in, got {:?}", status);
        assert!(status.user_name.is_some());
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_create_fetch_update_cycle() {
        let provider = CliProvider::new("lark-cli");

        // Create
        let title = format!("TestDoc-{}", chrono::Local::now().format("%H%M%S"));
        let markdown = format!("# {title}\n\nCreated by integration test.");
        let meta = provider.create_doc(&title, &markdown).await.unwrap();
        assert!(!meta.doc_id.is_empty(), "doc_id should not be empty");
        assert!(meta.url.contains(&meta.doc_id), "url should contain doc_id");

        // Fetch
        let fetched = provider.fetch_doc(&meta.doc_id).await.unwrap();
        // Note: fetched content may be empty initially (async indexing)
        // Just verify the call succeeds
        let _ = fetched;

        // Update
        let updated_md = format!("# {title}\n\nUpdated by integration test.");
        provider.update_doc(&meta.doc_id, &updated_md).await.unwrap();

        // Search
        let results = provider.search_docs(&title).await.unwrap();
        // Search indexing is async so may not find it immediately
        // Just verify the call succeeds
        let _ = results;
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_search_docs() {
        let provider = CliProvider::new("lark-cli");
        let results = provider.search_docs("测试").await.unwrap();
        // Should return at least one result (from our earlier test docs)
        assert!(!results.is_empty(), "Expected search results for '测试'");
        // Each result should have a doc_id
        for doc in &results {
            assert!(!doc.doc_id.is_empty());
        }
    }
}
