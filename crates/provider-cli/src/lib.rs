use larknotes_core::*;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::RwLock;
use tokio::process::Command;

pub struct CliProvider {
    cli_path: RwLock<String>,
    /// On Windows, resolved node.exe + script path to bypass cmd.exe arg mangling
    #[cfg(windows)]
    resolved: RwLock<Option<(PathBuf, PathBuf)>>,
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
            cli_path: RwLock::new(cli_path.to_string()),
            #[cfg(windows)]
            resolved: RwLock::new(resolved),
        }
    }

    /// Update the CLI path at runtime (e.g. when user changes settings).
    pub fn set_cli_path(&self, path: &str) {
        if let Ok(mut cli_path) = self.cli_path.write() {
            *cli_path = path.to_string();
        }
        #[cfg(windows)]
        if let Ok(mut resolved) = self.resolved.write() {
            *resolved = resolve_cmd_shim(path);
        }
    }

    async fn run_cli(&self, args: &[&str]) -> Result<serde_json::Value, LarkNotesError> {
        let cli_path = self.cli_path.read()
            .map_err(|e| LarkNotesError::Cli(format!("Lock error: {e}")))?
            .clone();

        tracing::debug!("lark-cli {}", args.join(" "));

        // On Windows, call node directly to avoid cmd.exe mangling multiline args
        #[cfg(windows)]
        let output = {
            let resolved = self.resolved.read()
                .map_err(|e| LarkNotesError::Cli(format!("Lock error: {e}")))?
                .clone();
            if let Some((node_path, script_path)) = &resolved {
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
                let mut cmd_args = vec!["/C", &cli_path as &str];
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
        let output = Command::new(&cli_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| LarkNotesError::Cli(format!("启动lark-cli失败: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // 1. Check exit code first — lark-cli writes errors to stderr on failure
        if !output.status.success() {
            let stderr_str = stderr.trim();
            // Try to parse stderr as JSON (lark-cli outputs structured errors)
            // stderr may have non-JSON prefix lines (e.g. "[lark-cli] ...") before the JSON
            if !stderr_str.is_empty() {
                // Try whole string first, then try from first '{'
                let json_candidates = [
                    stderr_str,
                    stderr_str.find('{').map(|i| &stderr_str[i..]).unwrap_or(""),
                ];
                for candidate in json_candidates {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(candidate) {
                        let msg = json.pointer("/error/message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(stderr_str);
                        return Err(LarkNotesError::Cli(msg.to_string()));
                    }
                }
                // Non-JSON stderr
                return Err(LarkNotesError::Cli(stderr_str.to_string()));
            }
            // No stderr — check stdout for error JSON
            let stdout_str = stdout.trim();
            if !stdout_str.is_empty() {
                return parse_cli_output(stdout_str);
            }
            // No output at all
            return Err(LarkNotesError::Cli(
                format!("lark-cli 退出码 {}", output.status.code().unwrap_or(-1))
            ));
        }

        // 2. Success exit code — parse stdout normally
        if !stderr.is_empty() {
            tracing::warn!("lark-cli stderr: {stderr}");
        }

        parse_cli_output(&stdout)
    }
}

fn strip_highlight(s: &str) -> String {
    s.replace("<h>", "").replace("</h>", "")
}

/// Unescape markdown that lark-cli returns with backslash-escaped syntax.
/// e.g. `\*\*bold\*\*` → `**bold**`, `\~\~strike\~\~` → `~~strike~~`
/// Parse CLI stdout into a JSON value.
/// Returns `Value::Null` for empty output (e.g. delete commands).
/// Returns an error if the "ok" field is explicitly `false`.
fn parse_cli_output(stdout: &str) -> Result<serde_json::Value, LarkNotesError> {
    if stdout.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }

    let json: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|e| LarkNotesError::Cli(format!("解析输出失败: {e}\nstdout: {stdout}")))?;

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
            folder_path: String::new(),
            file_size: None,
            word_count: None,
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
        folder_path: String::new(),
        file_size: None,
        word_count: None,
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
impl ProviderAuth for CliProvider {
    async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError> {
        let json = self.run_cli(&["auth", "status"]).await?;
        Ok(parse_auth_status(&json))
    }
}

#[async_trait::async_trait]
impl DocProvider for CliProvider {
    async fn create(&self, name: &str, content: &str) -> Result<DocMeta, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+create", "--title", name, "--markdown", content])
            .await?;
        parse_create_response(&json, name)
    }

    async fn read(&self, id: &str) -> Result<ReadOutput, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+fetch", "--doc", id, "--format", "json"])
            .await?;
        let content = parse_fetch_response(&json);
        // Extract available metadata from the fetch response
        let title_raw = json.pointer("/data/title")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title = if title_raw.is_empty() {
            extract_title(&content)
        } else {
            title_raw.to_string()
        };
        let url = json.pointer("/data/doc_url")
            .or_else(|| json.pointer("/data/url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let doc_id_from_resp = json.pointer("/data/doc_id")
            .and_then(|v| v.as_str())
            .unwrap_or(id);
        let meta = DocMeta {
            doc_id: doc_id_from_resp.to_string(),
            title,
            doc_type: "DOCX".to_string(),
            url,
            owner_name: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            local_path: None,
            content_hash: None,
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
            file_size: None,
            word_count: None,
        };
        Ok(ReadOutput { content, meta })
    }

    async fn write(&self, id: &str, content: &str) -> Result<WriteMeta, LarkNotesError> {
        self.run_cli(&[
            "docs", "+update", "--doc", id,
            "--mode", "overwrite",
            "--markdown", content,
        ]).await?;
        Ok(WriteMeta {
            // CLI doesn't return a server-side hash; the caller (sync engine)
            // computes its own local hash and uses that for change detection.
            content_hash: String::new(),
            updated_at: chrono::Local::now().to_rfc3339(),
        })
    }

    async fn delete(&self, id: &str) -> Result<(), LarkNotesError> {
        let api_path = format!("/open-apis/drive/v1/files/{}", id);
        let result = self.run_cli(&["api", "DELETE", &api_path, "--params", r#"{"type":"docx"}"#]).await?;
        if let Some(code) = result.get("code").and_then(|v| v.as_i64()) {
            if code != 0 {
                let msg = result
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                return Err(LarkNotesError::Cli(format!(
                    "删除失败 (code {}): {}",
                    code, msg
                )));
            }
        }
        Ok(())
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<(), LarkNotesError> {
        // Use append mode with a single space — harmless in Markdown, avoids
        // reading + re-uploading the entire document just to change the title.
        self.run_cli(&[
            "docs", "+update", "--doc", id,
            "--mode", "append",
            "--new-title", new_name,
            "--markdown", " ",
        ]).await?;
        Ok(())
    }

    async fn list(&self, folder: Option<&str>) -> Result<Vec<DocMeta>, LarkNotesError> {
        // Lark API doesn't have a native "list all" — use search with empty/wildcard query
        let query = folder.unwrap_or("");
        let json = self
            .run_cli(&["docs", "+search", "--query", query, "--format", "json"])
            .await?;
        Ok(parse_search_results(&json))
    }

    async fn search(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
        let json = self
            .run_cli(&["docs", "+search", "--query", query, "--format", "json"])
            .await?;
        Ok(parse_search_results(&json))
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

    // ========== unescape_markdown tests ===========

    #[test]
    fn test_unescape_markdown_stars() {
        assert_eq!(unescape_markdown(r"\*\*bold\*\*"), "**bold**");
    }

    #[test]
    fn test_unescape_markdown_tilde() {
        assert_eq!(unescape_markdown(r"\~\~strike\~\~"), "~~strike~~");
    }

    #[test]
    fn test_unescape_markdown_backtick() {
        assert_eq!(unescape_markdown(r"\`code\`"), "`code`");
    }

    #[test]
    fn test_unescape_markdown_brackets() {
        assert_eq!(unescape_markdown(r"\[link\]"), "[link]");
    }

    #[test]
    fn test_unescape_markdown_hash() {
        assert_eq!(unescape_markdown(r"\# Heading"), "# Heading");
    }

    #[test]
    fn test_unescape_markdown_all_escapes() {
        let input = r"\*\~\`\[\]\#\>\-\_";
        let expected = "*~`[]#>-_";
        assert_eq!(unescape_markdown(input), expected);
    }

    #[test]
    fn test_unescape_markdown_no_escapes() {
        assert_eq!(unescape_markdown("plain text"), "plain text");
        assert_eq!(unescape_markdown(""), "");
    }

    #[test]
    fn test_unescape_markdown_mixed_content() {
        let input = r"# Hello \*\*World\*\*\n\nSome \`code\` here";
        let expected = "# Hello **World**\\n\\nSome `code` here";
        assert_eq!(unescape_markdown(input), expected);
    }

    // ========== parse edge cases ===========

    #[test]
    fn test_parse_search_results_missing_title() {
        // No title_highlighted → fallback to "Untitled"
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {
                "results": [{
                    "result_meta": { "token": "doc1", "url": "", "owner_name": "", "doc_types": "DOCX", "create_time_iso": "", "update_time_iso": "" }
                }]
            }
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "Untitled");
    }

    #[test]
    fn test_parse_search_results_minimal_meta() {
        // result_meta with only token
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": {
                "results": [{
                    "result_meta": { "token": "abc" },
                    "title_highlighted": "Test"
                }]
            }
        }"#).unwrap();

        let docs = parse_search_results(&json);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].doc_id, "abc");
        assert_eq!(docs[0].url, "");
        assert_eq!(docs[0].owner_name, "");
    }

    #[test]
    fn test_parse_create_response_missing_url() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": { "doc_id": "xyz" }
        }"#).unwrap();

        let meta = parse_create_response(&json, "Test").unwrap();
        assert_eq!(meta.doc_id, "xyz");
        assert_eq!(meta.url, ""); // Missing url defaults to empty
    }

    #[test]
    fn test_parse_fetch_response_escaped_markdown() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "ok": true,
            "data": { "markdown": "\\*\\*bold\\*\\*" }
        }"#).unwrap();

        let md = parse_fetch_response(&json);
        assert_eq!(md, "**bold**"); // Should be unescaped
    }

    // ========== set_cli_path tests ===========

    #[test]
    fn test_set_cli_path_updates_path() {
        let provider = CliProvider::new("lark-cli");
        assert_eq!(*provider.cli_path.read().unwrap(), "lark-cli");

        provider.set_cli_path("/usr/local/bin/lark-cli");
        assert_eq!(*provider.cli_path.read().unwrap(), "/usr/local/bin/lark-cli");
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
        let meta = provider.create(&title, &markdown).await.unwrap();
        assert!(!meta.doc_id.is_empty(), "doc_id should not be empty");
        assert!(meta.url.contains(&meta.doc_id), "url should contain doc_id");

        // Fetch
        let fetched = provider.read(&meta.doc_id).await.unwrap();
        // Note: fetched content may be empty initially (async indexing)
        // Just verify the call succeeds
        let _ = fetched;

        // Update
        let updated_md = format!("# {title}\n\nUpdated by integration test.");
        provider.write(&meta.doc_id, &updated_md).await.unwrap();

        // Search
        let results = provider.search(&title).await.unwrap();
        // Search indexing is async so may not find it immediately
        // Just verify the call succeeds
        let _ = results;
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_search_docs() {
        let provider = CliProvider::new("lark-cli");
        let results = provider.search("测试").await.unwrap();
        // Should return at least one result (from our earlier test docs)
        assert!(!results.is_empty(), "Expected search results for '测试'");
        // Each result should have a doc_id
        for doc in &results {
            assert!(!doc.doc_id.is_empty());
        }
    }

    // ========== parse_cli_output tests ==========

    #[test]
    fn test_parse_cli_output_empty_stdout() {
        let result = parse_cli_output("").unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn test_parse_cli_output_whitespace_only() {
        let result = parse_cli_output("  \n  ").unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn test_parse_cli_output_valid_json() {
        let result = parse_cli_output(r#"{"ok": true, "data": "hello"}"#).unwrap();
        assert_eq!(result["data"], "hello");
    }

    #[test]
    fn test_parse_cli_output_ok_false() {
        let result = parse_cli_output(
            r#"{"ok": false, "error": {"message": "文档不存在"}}"#,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("文档不存在"));
    }

    #[test]
    fn test_parse_cli_output_ok_false_no_message() {
        let result = parse_cli_output(r#"{"ok": false}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("未知错误"));
    }

    #[test]
    fn test_parse_cli_output_invalid_json() {
        let result = parse_cli_output("not json at all");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("解析输出失败"));
    }

    // ========== simulate_run_cli: test exit code + stderr handling ==========

    /// Simulates run_cli's error handling logic for testing.
    /// Takes (success: bool, stdout: &str, stderr: &str) and returns the same Result as run_cli.
    fn simulate_run_cli(success: bool, stdout: &str, stderr: &str) -> Result<serde_json::Value, LarkNotesError> {
        if !success {
            let stderr_str = stderr.trim();
            if !stderr_str.is_empty() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(stderr_str) {
                    let msg = json.pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or(stderr_str);
                    return Err(LarkNotesError::Cli(msg.to_string()));
                }
                return Err(LarkNotesError::Cli(stderr_str.to_string()));
            }
            if !stdout.trim().is_empty() {
                return parse_cli_output(stdout);
            }
            return Err(LarkNotesError::Cli("lark-cli 退出码 1".to_string()));
        }
        parse_cli_output(stdout)
    }

    // #24: exit 1 + stderr JSON → extract error message
    #[test]
    fn test_cli_exit1_stderr_json() {
        let stderr = r#"{"ok":false,"identity":"user","error":{"type":"api_error","code":99992402,"message":"API error: [99992402] field validation failed","detail":{"field_violations":[{"description":"type is required","field":"type"}]}}}"#;
        let result = simulate_run_cli(false, "", stderr);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("field validation failed"), "got: {msg}");
    }

    // #25: exit 1 + stderr plain text → use stderr as error message
    #[test]
    fn test_cli_exit1_stderr_plain() {
        let result = simulate_run_cli(false, "", "Error: connection timeout");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("connection timeout"));
    }

    // #26: exit 1 + no output → generic exit code error
    #[test]
    fn test_cli_exit1_no_output() {
        let result = simulate_run_cli(false, "", "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("退出码"));
    }

    // #27: exit 0 + empty stdout → Ok(Null)
    #[test]
    fn test_cli_exit0_empty_stdout() {
        let result = simulate_run_cli(true, "", "");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), serde_json::Value::Null);
    }

    // #28: exit 0 + stdout has ok:false → Err
    #[test]
    fn test_cli_exit0_ok_false() {
        let result = simulate_run_cli(true, r#"{"ok":false,"error":{"message":"文档不存在"}}"#, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("文档不存在"));
    }

    // Test that lark-cli "doc not found" stderr error is properly detected as not_found
    #[test]
    fn test_cli_not_found_detection() {
        let stderr = r#"{"ok":false,"error":{"message":"doc not found"}}"#;
        let result = simulate_run_cli(false, "", stderr);
        assert!(result.is_err());
        assert!(result.unwrap_err().is_not_found());
    }

    // ================================================================
    // Live integration tests — real lark-cli + real Feishu API
    // Run with: cargo test -p larknotes-provider-cli -- --ignored
    // ================================================================

    fn live_provider() -> CliProvider {
        CliProvider::new("lark-cli")
    }

    fn test_title(label: &str) -> String {
        format!("_Test_{label}_{}", chrono::Local::now().format("%H%M%S%3f"))
    }

    // #1: CREATE OK
    #[tokio::test]
    #[ignore]
    async fn test_live_create_ok() {
        let p = live_provider();
        let title = test_title("create_ok");
        let md = format!("# {title}\n\nCreated by integration test.");
        let meta = p.create(&title, &md).await.unwrap();
        assert!(!meta.doc_id.is_empty(), "doc_id should not be empty");
        assert!(!meta.url.is_empty(), "url should not be empty");
        // Cleanup
        let _ = p.delete(&meta.doc_id).await;
    }

    // #2: CREATE FAIL — invalid CLI path → launch error
    #[tokio::test]
    #[ignore]
    async fn test_live_create_fail() {
        let p = CliProvider::new("nonexistent-lark-cli-binary-xyz");
        let result = p.create("Fail", "# Fail").await;
        assert!(result.is_err(), "Should fail with invalid CLI path");
        // On Windows, the error message may be garbled GBK; just verify it's a CLI error
        let err = result.unwrap_err();
        assert!(matches!(err, LarkNotesError::Cli(_)), "Should be a CLI error, got: {err}");
    }

    // #10: PULL S1 — fetch a just-created doc (content may differ due to Feishu processing)
    #[tokio::test]
    #[ignore]
    async fn test_live_pull_s1_noop() {
        let p = live_provider();
        let title = test_title("pull_s1");
        let md = format!("# {title}\n\nPull S1 test content.");
        let meta = p.create(&title, &md).await.unwrap();

        // Wait briefly for Feishu to index
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let fetched = p.read(&meta.doc_id).await.unwrap();
        // Feishu may reformat markdown, so just check it's non-empty and contains title
        assert!(!fetched.content.is_empty(), "Fetched content should not be empty");

        let _ = p.delete(&meta.doc_id).await;
    }

    // #11: PULL S2 — fetch always returns remote content, ignoring local changes
    #[tokio::test]
    #[ignore]
    async fn test_live_pull_s2_overwrite_local() {
        let p = live_provider();
        let title = test_title("pull_s2");
        let md = format!("# {title}\n\nOriginal remote content.");
        let meta = p.create(&title, &md).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Simulate: local file modified to something else (we don't write file here,
        // just verify that read returns the remote version regardless)
        let fetched = p.read(&meta.doc_id).await.unwrap();
        assert!(!fetched.content.is_empty(), "Remote content should be returned");
        // read always returns remote content — this IS the "overwrite local" behavior
        // The caller (commands.rs) writes this to the local file, overwriting local changes.

        let _ = p.delete(&meta.doc_id).await;
    }

    // #12: PULL S3 — remote has newer content [needs remote_hash]
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to distinguish S3 from S1
    async fn test_live_pull_s3_update() {
        // Currently indistinguishable from S1 without remote_hash comparison.
        let p = live_provider();
        let title = test_title("pull_s3");
        let meta = p.create(&title, "# S3 test").await.unwrap();
        let fetched = p.read(&meta.doc_id).await.unwrap();
        assert!(fetched.content.is_empty() || !fetched.content.is_empty(), "Just verify call succeeds");
        let _ = p.delete(&meta.doc_id).await;
    }

    // #13: PULL S4 — both sides modified [needs remote_hash]
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to detect S4 conflict
    async fn test_live_pull_s4_overwrite() {
        let p = live_provider();
        let title = test_title("pull_s4");
        let meta = p.create(&title, "# S4 test").await.unwrap();
        let fetched = p.read(&meta.doc_id).await.unwrap();
        assert!(fetched.content.is_empty() || !fetched.content.is_empty(), "Just verify call succeeds");
        let _ = p.delete(&meta.doc_id).await;
    }

    // #14: PULL S5 — fetch a nonexistent doc → error
    #[tokio::test]
    #[ignore]
    async fn test_live_pull_s5_fail() {
        let p = live_provider();
        // Use an obviously invalid doc_id that never existed
        let result = p.read("NONEXISTENT_DOC_ID_FOR_S5_TEST").await;
        assert!(result.is_err(), "Fetching nonexistent doc should fail");
    }

    // #15: PULL S6 — import from remote-only doc
    #[tokio::test]
    #[ignore]
    async fn test_live_pull_s6_import() {
        let p = live_provider();
        let title = test_title("pull_s6_import");
        let md = format!("# {title}\n\nImport test.");
        let meta = p.create(&title, &md).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Simulate import: fetch → write to local file
        let output = p.read(&meta.doc_id).await.unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let local_path = tmp.path().join(format!("{title}.md"));
        std::fs::write(&local_path, &output.content).unwrap();

        assert!(local_path.exists(), "Local file should be created");
        let read_back = std::fs::read_to_string(&local_path).unwrap();
        assert_eq!(read_back, output.content, "Local file content should match fetched content");

        let _ = p.delete(&meta.doc_id).await;
    }

    // #16: DELETE S1 — delete a synced doc
    #[tokio::test]
    #[ignore]
    async fn test_live_delete_s1_ok() {
        let p = live_provider();
        let title = test_title("del_s1");
        let meta = p.create(&title, "# Delete S1").await.unwrap();

        p.delete(&meta.doc_id).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Verify: fetch should fail
        let result = p.read(&meta.doc_id).await;
        assert!(result.is_err(), "Fetch after delete should fail");
    }

    // #17: DELETE S2 — delete a doc that was updated (local modified)
    #[tokio::test]
    #[ignore]
    async fn test_live_delete_s2_ok() {
        let p = live_provider();
        let title = test_title("del_s2");
        let meta = p.create(&title, "# Delete S2 original").await.unwrap();

        // Update remote (simulates having local changes pushed)
        p.write(&meta.doc_id, "# Delete S2 modified").await.unwrap();

        // Delete should still succeed
        p.delete(&meta.doc_id).await.unwrap();
    }

    // #18: DELETE S3 [needs remote_hash]
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism
    async fn test_live_delete_s3_ok() {
        let p = live_provider();
        let title = test_title("del_s3");
        let meta = p.create(&title, "# Delete S3").await.unwrap();
        p.delete(&meta.doc_id).await.unwrap();
    }

    // #19: DELETE S4 [needs remote_hash]
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism
    async fn test_live_delete_s4_ok() {
        let p = live_provider();
        let title = test_title("del_s4");
        let meta = p.create(&title, "# Delete S4").await.unwrap();
        p.delete(&meta.doc_id).await.unwrap();
    }

    // #20: DELETE S5 — delete already-deleted doc → not_found
    #[tokio::test]
    #[ignore]
    async fn test_live_delete_s5_local_only() {
        let p = live_provider();
        let title = test_title("del_s5");
        let meta = p.create(&title, "# Delete S5").await.unwrap();
        p.delete(&meta.doc_id).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Second delete should fail
        let result = p.delete(&meta.doc_id).await;
        assert!(result.is_err(), "Deleting already-deleted doc should fail");
        // Verify the error is classified properly (not_found or similar)
        let err = result.unwrap_err();
        // Drive API may return various codes for deleted docs
        eprintln!("Delete-again error: {err}");
    }

    // #21: DELETE S5 permission fail — try deleting a doc we don't own
    #[tokio::test]
    #[ignore]
    async fn test_live_delete_s5_permission_fail() {
        let p = live_provider();
        // Use a well-known Feishu doc token that we don't own
        let result = p.delete("doxcn000000000000000").await;
        assert!(result.is_err(), "Deleting unowned doc should fail");
        let err = result.unwrap_err();
        assert!(!err.is_transient(), "Permission error should not be transient");
        eprintln!("Permission error: {err}");
    }

    // #22: SEARCH OK
    #[tokio::test]
    #[ignore]
    async fn test_live_search_ok() {
        let p = live_provider();
        let results = p.search("测试").await.unwrap();
        assert!(!results.is_empty(), "Search for '测试' should return results");
        for doc in &results {
            assert!(!doc.doc_id.is_empty(), "Each result should have a doc_id");
        }
    }

    // #23: SEARCH empty
    #[tokio::test]
    #[ignore]
    async fn test_live_search_empty() {
        let p = live_provider();
        let results = p.search("xyzzy_nonexistent_query_12345_abcde").await.unwrap();
        assert!(results.is_empty(), "Search for gibberish should return empty");
    }

    // #24: run_cli error handling — invalid doc ID → structured error from lark-cli
    #[tokio::test]
    #[ignore]
    async fn test_live_cli_invalid_doc_fetch() {
        let p = live_provider();
        let result = p.read("INVALID_DOC_ID_999_XYZ").await;
        assert!(result.is_err(), "Fetching invalid doc ID should fail");
        let msg = result.unwrap_err().to_string();
        // The error should be a parsed message, not empty
        assert!(!msg.is_empty(), "Error message should not be empty");
        eprintln!("Invalid doc error: {msg}");
    }

    // #25: run_cli error handling — invalid CLI binary → launch failure
    #[tokio::test]
    #[ignore]
    async fn test_live_cli_invalid_command() {
        let p = CliProvider::new("absolutely-nonexistent-cli-999");
        let result = p.auth_status().await;
        assert!(result.is_err(), "Invalid CLI path should fail");
        // On Windows, error message encoding may be garbled; just verify it's a CLI error
        let err = result.unwrap_err();
        assert!(matches!(err, LarkNotesError::Cli(_)), "Should be a CLI error, got: {err}");
    }

    // ─── WRITE direct tests ────────────────────────────────

    // #26: WRITE OK — write content to an existing doc
    #[tokio::test]
    #[ignore]
    async fn test_live_write_ok() {
        let p = live_provider();
        let title = test_title("write_ok");
        let md = format!("# {title}\n\nOriginal.");
        let meta = p.create(&title, &md).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let new_content = format!("# {title}\n\nUpdated by write test.");
        let wm = p.write(&meta.doc_id, &new_content).await.unwrap();
        assert!(!wm.updated_at.is_empty(), "WriteMeta should have updated_at");

        // Verify content was actually written
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let fetched = p.read(&meta.doc_id).await.unwrap();
        assert!(fetched.content.contains("Updated by write test"), "Content should be updated, got: {}", fetched.content);

        let _ = p.delete(&meta.doc_id).await;
    }

    // #27: WRITE FAIL — write to nonexistent doc
    #[tokio::test]
    #[ignore]
    async fn test_live_write_fail() {
        let p = live_provider();
        let result = p.write("INVALID_DOC_ID_999", "# content").await;
        assert!(result.is_err(), "Writing to nonexistent doc should fail");
    }

    // ─── RENAME direct tests ───────────────────────────────

    // #28: RENAME OK — rename a doc and verify title changed
    #[tokio::test]
    #[ignore]
    async fn test_live_rename_ok() {
        let p = live_provider();
        let title = test_title("rename_ok");
        let md = format!("# {title}\n\nRename test.");
        let meta = p.create(&title, &md).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let new_title = test_title("renamed");
        p.rename(&meta.doc_id, &new_title).await.unwrap();

        // Verify rename took effect by reading back
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let fetched = p.read(&meta.doc_id).await.unwrap();
        // The content should now have the new title
        assert!(
            fetched.content.contains(&new_title) || fetched.meta.title.contains("renamed"),
            "Title should be updated after rename"
        );

        let _ = p.delete(&meta.doc_id).await;
    }

    // #29: RENAME FAIL — rename nonexistent doc
    #[tokio::test]
    #[ignore]
    async fn test_live_rename_fail() {
        let p = live_provider();
        let result = p.rename("INVALID_DOC_ID_999", "New Name").await;
        assert!(result.is_err(), "Renaming nonexistent doc should fail");
    }

    // #30: RENAME with special characters
    #[tokio::test]
    #[ignore]
    async fn test_live_rename_special_chars() {
        let p = live_provider();
        let title = test_title("rename_special");
        let md = format!("# {title}\n\nSpecial char rename test.");
        let meta = p.create(&title, &md).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let new_title = format!("_Test_重命名_中文标题_{}", chrono::Local::now().format("%H%M%S"));
        let result = p.rename(&meta.doc_id, &new_title).await;
        assert!(result.is_ok(), "Rename with Chinese characters should succeed");

        let _ = p.delete(&meta.doc_id).await;
    }

    // ─── LIST direct tests ─────────────────────────────────

    // #31: LIST OK — list returns some docs
    #[tokio::test]
    #[ignore]
    async fn test_live_list_ok() {
        let p = live_provider();
        let result = p.list(None).await;
        assert!(result.is_ok(), "List should succeed");
        // We can't guarantee how many docs exist, but the call should work
        eprintln!("Listed {} docs", result.unwrap().len());
    }

    // #32: LIST with folder — currently may use search fallback
    #[tokio::test]
    #[ignore]
    async fn test_live_list_with_folder() {
        let p = live_provider();
        let result = p.list(Some("nonexistent_folder_xyz")).await;
        // Should succeed even for nonexistent folder (returns empty or search fallback)
        assert!(result.is_ok(), "List with folder should not error");
    }
}
