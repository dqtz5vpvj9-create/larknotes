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
    pub fn is_transient(&self) -> bool {
        let msg = self.to_string().to_lowercase();
        // Permanent errors
        if msg.contains("401") || msg.contains("403") || msg.contains("404")
            || msg.contains("unauthorized") || msg.contains("forbidden")
            || msg.contains("not found") || msg.contains("permission")
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
        serializer.serialize_str(&self.to_string())
    }
}
