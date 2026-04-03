pub mod window_monitor;

use larknotes_core::LarkNotesError;
use std::path::Path;
use std::process::Command;

pub struct EditorLauncher {
    command: String,
}

impl EditorLauncher {
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
        }
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn set_command(&mut self, command: &str) {
        self.command = command.to_string();
    }

    pub fn open_file(&self, path: &Path) -> Result<std::process::Child, LarkNotesError> {
        Command::new(&self.command)
            .arg(path)
            .spawn()
            .map_err(|e| {
                LarkNotesError::Editor(format!(
                    "启动编辑器 '{}' 失败: {e}",
                    self.command
                ))
            })
    }

    pub fn open_in_explorer(path: &Path) -> Result<(), LarkNotesError> {
        #[cfg(windows)]
        {
            Command::new("explorer")
                .arg(path)
                .spawn()
                .map_err(|e| LarkNotesError::Editor(format!("打开文件管理器失败: {e}")))?;
        }
        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .arg("-R")
                .arg(path)
                .spawn()
                .map_err(|e| LarkNotesError::Editor(format!("打开Finder失败: {e}")))?;
        }
        #[cfg(target_os = "linux")]
        {
            Command::new("xdg-open")
                .arg(path)
                .spawn()
                .map_err(|e| LarkNotesError::Editor(format!("打开文件管理器失败: {e}")))?;
        }
        Ok(())
    }
}

/// Detect the best available editor on the system.
/// Checks environment variable, PATH, then common Windows install locations.
pub fn detect_editor() -> String {
    // 1. Check environment variable
    if let Ok(editor) = std::env::var("LARKNOTES_EDITOR") {
        if !editor.is_empty() {
            return editor;
        }
    }

    // 2. Probe common editors on PATH
    let path_candidates = ["typora", "code", "notepad++", "subl"];
    for candidate in &path_candidates {
        if which::which(candidate).is_ok() {
            return candidate.to_string();
        }
    }

    // 3. Check common Windows install locations
    #[cfg(target_os = "windows")]
    {
        if let Some(found) = find_windows_editor() {
            return found;
        }
    }

    // 4. Fallback
    "notepad".to_string()
}

#[cfg(target_os = "windows")]
fn find_windows_editor() -> Option<String> {
    let home = dirs::home_dir()?;
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let program_files_x86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        home.join("AppData")
            .join("Local")
            .to_string_lossy()
            .to_string()
    });

    // Ordered by preference: Typora > Obsidian > VS Code > Notepad++ > Sublime
    let candidates: Vec<(&str, Vec<std::path::PathBuf>)> = vec![
        (
            "Typora",
            vec![
                home.join("Applications").join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&program_files)
                    .join("Typora")
                    .join("Typora.exe"),
                std::path::PathBuf::from(&program_files_x86)
                    .join("Typora")
                    .join("Typora.exe"),
                std::path::PathBuf::from(&local_appdata)
                    .join("Programs")
                    .join("Typora")
                    .join("Typora.exe"),
            ],
        ),
        (
            "VS Code",
            vec![
                std::path::PathBuf::from(&local_appdata)
                    .join("Programs")
                    .join("Microsoft VS Code")
                    .join("Code.exe"),
                std::path::PathBuf::from(&program_files)
                    .join("Microsoft VS Code")
                    .join("Code.exe"),
            ],
        ),
        (
            "Notepad++",
            vec![
                std::path::PathBuf::from(&program_files)
                    .join("Notepad++")
                    .join("notepad++.exe"),
                std::path::PathBuf::from(&program_files_x86)
                    .join("Notepad++")
                    .join("notepad++.exe"),
            ],
        ),
    ];

    for (_label, paths) in candidates {
        for p in paths {
            if p.exists() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_editor_returns_something() {
        let editor = detect_editor();
        assert!(!editor.is_empty());
    }

    #[test]
    fn test_editor_launcher_command() {
        let mut launcher = EditorLauncher::new("typora");
        assert_eq!(launcher.command(), "typora");
        launcher.set_command("code");
        assert_eq!(launcher.command(), "code");
    }

    #[test]
    fn test_editor_launcher_command_with_spaces() {
        let launcher = EditorLauncher::new("open -a TextEdit");
        assert_eq!(launcher.command(), "open -a TextEdit");
    }

    #[test]
    fn test_open_file_nonexistent_command() {
        let launcher = EditorLauncher::new("__nonexistent_editor_12345__");
        let result = launcher.open_file(std::path::Path::new("test.md"));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("__nonexistent_editor_12345__"), "Error should mention the command: {err_msg}");
    }

    #[test]
    fn test_detect_editor_env_var() {
        // Set env var and verify it's picked up
        std::env::set_var("LARKNOTES_EDITOR", "custom-editor-test");
        let editor = detect_editor();
        assert_eq!(editor, "custom-editor-test");
        // Clean up
        std::env::remove_var("LARKNOTES_EDITOR");
    }

    #[test]
    fn test_detect_editor_env_var_empty() {
        std::env::set_var("LARKNOTES_EDITOR", "");
        let editor = detect_editor();
        // Empty env var should be ignored — falls through to PATH check
        assert_ne!(editor, "");
        std::env::remove_var("LARKNOTES_EDITOR");
    }

    #[test]
    fn test_detect_editor_always_returns_something() {
        // Even with no env var and no editors on PATH, should fallback to notepad
        std::env::remove_var("LARKNOTES_EDITOR");
        let editor = detect_editor();
        assert!(!editor.is_empty());
    }
}
