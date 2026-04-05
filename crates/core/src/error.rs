use serde::Serialize;

#[derive(thiserror::Error, Debug)]
pub enum LarkNotesError {
    #[error("CLI执行失败: {0}")]
    Cli(String),
    #[error("存储错误: {0}")]
    Storage(String),
    #[error("同步错误: {0}")]
    Sync(String),
    #[error("编辑器错误: {0}")]
    Editor(String),
    #[error("认证失败: {0}")]
    Auth(String),
    #[error("{0}")]
    Other(String),
}

impl LarkNotesError {
    /// Returns true for transient errors that may succeed on retry
    /// (network timeouts, 5xx server errors, CLI launch failures).
    /// Returns false for permanent errors (401, 404, auth failures).
    /// Returns true if the error indicates the remote resource was not found
    /// (deleted or never existed). Used to trigger re-creation.
    pub fn is_not_found(&self) -> bool {
        let msg = self.to_string().to_lowercase();
        msg.contains("404")
            || msg.contains("not found")
            || msg.contains("不存在")
            || msg.contains("deleted")
            || msg.contains("已删除")
            || msg.contains("no such")
    }

    pub fn is_transient(&self) -> bool {
        let msg = self.to_string().to_lowercase();
        // Permanent errors
        if msg.contains("401") || msg.contains("403") || msg.contains("404")
            || msg.contains("unauthorized") || msg.contains("forbidden")
            || msg.contains("not found") || msg.contains("permission")
            || msg.contains("deleted") || msg.contains("已删除")
        {
            return false;
        }
        // Transient patterns
        matches!(self, LarkNotesError::Cli(_) | LarkNotesError::Sync(_))
            || msg.contains("timeout")
            || msg.contains("network")
            || msg.contains("connection")
            || msg.contains("500")
            || msg.contains("502")
            || msg.contains("503")
            || msg.contains("504")
    }
}

impl Serialize for LarkNotesError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("LarkNotesError", 2)?;
        let kind = match self {
            LarkNotesError::Cli(_) => "cli",
            LarkNotesError::Storage(_) => "storage",
            LarkNotesError::Sync(_) => "sync",
            LarkNotesError::Editor(_) => "editor",
            LarkNotesError::Auth(_) => "auth",
            LarkNotesError::Other(_) => "other",
        };
        s.serialize_field("kind", kind)?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── is_transient() ──────────────────────────────────

    #[test]
    fn test_transient_cli_error() {
        assert!(LarkNotesError::Cli("connection reset".into()).is_transient());
    }

    #[test]
    fn test_transient_sync_error() {
        assert!(LarkNotesError::Sync("write failed".into()).is_transient());
    }

    #[test]
    fn test_transient_timeout() {
        assert!(LarkNotesError::Other("request timeout".into()).is_transient());
    }

    #[test]
    fn test_transient_network() {
        assert!(LarkNotesError::Other("network unreachable".into()).is_transient());
    }

    #[test]
    fn test_transient_connection() {
        assert!(LarkNotesError::Other("connection refused".into()).is_transient());
    }

    #[test]
    fn test_transient_5xx() {
        assert!(LarkNotesError::Other("server returned 500".into()).is_transient());
        assert!(LarkNotesError::Other("502 bad gateway".into()).is_transient());
        assert!(LarkNotesError::Other("503 service unavailable".into()).is_transient());
        assert!(LarkNotesError::Other("504 gateway timeout".into()).is_transient());
    }

    #[test]
    fn test_permanent_401() {
        assert!(!LarkNotesError::Cli("401 unauthorized".into()).is_transient());
    }

    #[test]
    fn test_permanent_403() {
        assert!(!LarkNotesError::Other("403 forbidden".into()).is_transient());
    }

    #[test]
    fn test_permanent_404() {
        assert!(!LarkNotesError::Auth("404 not found".into()).is_transient());
    }

    #[test]
    fn test_permanent_auth_variant() {
        // Auth variant with generic message → not transient (Auth is not Cli/Sync)
        assert!(!LarkNotesError::Auth("token expired".into()).is_transient());
    }

    #[test]
    fn test_permanent_overrides_variant() {
        // Even though Cli is transient by variant, "404" keyword overrides
        assert!(!LarkNotesError::Cli("404 not found".into()).is_transient());
    }

    #[test]
    fn test_permanent_permission() {
        assert!(!LarkNotesError::Other("permission denied".into()).is_transient());
    }

    #[test]
    fn test_storage_not_transient() {
        // Storage errors are not Cli/Sync variant, so not transient unless keyword match
        assert!(!LarkNotesError::Storage("disk full".into()).is_transient());
    }

    #[test]
    fn test_editor_not_transient() {
        assert!(!LarkNotesError::Editor("editor not found".into()).is_transient());
    }

    // ─── Serialize ───────────────────────────────────────

    #[test]
    fn test_serialize_cli() {
        let err = LarkNotesError::Cli("test error".into());
        let json: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "cli");
        assert_eq!(json["message"], "CLI执行失败: test error");
    }

    #[test]
    fn test_serialize_storage() {
        let err = LarkNotesError::Storage("db locked".into());
        let json: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "storage");
        assert!(json["message"].as_str().unwrap().contains("db locked"));
    }

    #[test]
    fn test_serialize_all_variants() {
        let variants: Vec<(&str, LarkNotesError)> = vec![
            ("cli", LarkNotesError::Cli("a".into())),
            ("storage", LarkNotesError::Storage("b".into())),
            ("sync", LarkNotesError::Sync("c".into())),
            ("editor", LarkNotesError::Editor("d".into())),
            ("auth", LarkNotesError::Auth("e".into())),
            ("other", LarkNotesError::Other("f".into())),
        ];
        for (expected_kind, err) in variants {
            let json: serde_json::Value = serde_json::to_value(&err).unwrap();
            assert_eq!(json["kind"], expected_kind, "wrong kind for {err}");
            assert!(json["message"].is_string(), "message not string for {err}");
        }
    }

    // ─── is_not_found() comprehensive ───────────────────

    // #29: is_not_found covers all expected patterns
    #[test]
    fn test_is_not_found_comprehensive() {
        // Should match
        assert!(LarkNotesError::Cli("404 not found".into()).is_not_found());
        assert!(LarkNotesError::Cli("document not found".into()).is_not_found());
        assert!(LarkNotesError::Cli("文档不存在".into()).is_not_found());
        assert!(LarkNotesError::Cli("file_token不存在".into()).is_not_found());
        assert!(LarkNotesError::Cli("document has been deleted".into()).is_not_found());
        assert!(LarkNotesError::Cli("文档已删除".into()).is_not_found());
        assert!(LarkNotesError::Cli("no such document".into()).is_not_found());

        // Should NOT match
        assert!(!LarkNotesError::Cli("permission denied".into()).is_not_found());
        assert!(!LarkNotesError::Cli("connection timeout".into()).is_not_found());
        assert!(!LarkNotesError::Cli("rate limit exceeded".into()).is_not_found());
        assert!(!LarkNotesError::Cli("internal server error".into()).is_not_found());
    }

    // #30: is_transient covers expected patterns
    #[test]
    fn test_is_transient_comprehensive() {
        // Transient (should retry)
        assert!(LarkNotesError::Cli("connection timeout".into()).is_transient());
        assert!(LarkNotesError::Cli("network unreachable".into()).is_transient());
        assert!(LarkNotesError::Other("502 bad gateway".into()).is_transient());
        assert!(LarkNotesError::Other("503 service unavailable".into()).is_transient());

        // NOT transient (permanent errors)
        assert!(!LarkNotesError::Cli("401 unauthorized".into()).is_transient());
        assert!(!LarkNotesError::Cli("403 forbidden".into()).is_transient());
        assert!(!LarkNotesError::Cli("404 not found".into()).is_transient());
        assert!(!LarkNotesError::Cli("permission denied".into()).is_transient());
        assert!(!LarkNotesError::Auth("token expired".into()).is_transient());
    }
}
