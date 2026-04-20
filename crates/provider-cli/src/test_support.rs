//! Test helpers for live integration tests against Feishu Lark.
//!
//! Tests creating remote docs should call [`test_folder_token`] to scope all
//! created docs into a dedicated folder, so leaked docs from crashed tests
//! never pollute the user's drive root.
//!
//! Resolution order:
//!   1. `LARKNOTES_TEST_FOLDER_TOKEN` env var, if set and non-empty.
//!   2. Otherwise, create a fresh `LarkNotes-Tests-YYYYMMDD-HHMMSS` folder
//!      via `lark-cli drive files create_folder` and cache for the process.

use serde_json::json;
use tokio::process::Command;
use tokio::sync::OnceCell;

static TEST_FOLDER: OnceCell<String> = OnceCell::const_new();

pub async fn test_folder_token() -> String {
    TEST_FOLDER
        .get_or_init(|| async {
            if let Ok(tok) = std::env::var("LARKNOTES_TEST_FOLDER_TOKEN") {
                if !tok.is_empty() {
                    return tok;
                }
            }
            let name = format!(
                "LarkNotes-Tests-{}",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            );
            create_folder(&name)
                .await
                .unwrap_or_else(|e| panic!("test_folder_token: create folder failed: {e}"))
        })
        .await
        .clone()
}

async fn create_folder(name: &str) -> Result<String, String> {
    let body = json!({ "folder_token": "", "name": name }).to_string();
    let out = Command::new("lark-cli")
        .args(["drive", "files", "create_folder", "--data", &body])
        .output()
        .await
        .map_err(|e| format!("spawn lark-cli: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("parse lark-cli output: {e}\n--- stdout ---\n{stdout}"))?;
    v.pointer("/data/token")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("no token in response: {stdout}"))
}
