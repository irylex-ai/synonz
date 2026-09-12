//! Read-only primitive tools: the `#[derive(Tool)]` extension example.
//!
//! Every tool is sandboxed to the process working directory: resolved
//! paths must stay inside it, and sizes / entry counts are capped.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;
use synonz::{JsonSchema, Tool, ToolContent, ToolError, ToolResult};

/// Read cap for `read_file` (bytes; larger files are truncated visibly).
const MAX_READ_BYTES: usize = 64 * 1024;
/// Entry cap for `list_dir`.
const MAX_DIR_ENTRIES: usize = 200;

/// The sandbox root: the process working directory.
fn sandbox_root() -> Result<PathBuf, ToolError> {
    std::env::current_dir().map_err(|error| ToolError::Execution {
        message: format!("cannot determine the working directory: {error}"),
    })
}

/// Resolves `path` under `root`, rejecting anything that escapes it.
fn resolve(root: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|error| ToolError::Execution {
            message: format!("cannot resolve {path:?}: {error}"),
        })?;
    let root = root.canonicalize().map_err(|error| ToolError::Execution {
        message: format!("cannot resolve the sandbox root: {error}"),
    })?;
    if !canonical.starts_with(&root) {
        return Err(ToolError::Execution {
            message: format!("{path:?} escapes the sandbox ({})", root.display()),
        });
    }
    Ok(canonical)
}

/// Reads a UTF-8 text file from the working directory.
#[derive(Tool, Deserialize, JsonSchema)]
pub struct ReadFile {
    /// Path relative to the working directory (or absolute inside it).
    pub path: String,
}

impl ReadFile {
    async fn run(&self) -> Result<ToolResult, ToolError> {
        read_file_at(&sandbox_root()?, &self.path).await
    }
}

async fn read_file_at(root: &Path, path: &str) -> Result<ToolResult, ToolError> {
    let resolved = resolve(root, path)?;
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|error| ToolError::Execution {
            message: format!("cannot stat {path:?}: {error}"),
        })?;
    if metadata.is_dir() {
        return Err(ToolError::Execution {
            message: format!("{path:?} is a directory; use list_dir"),
        });
    }
    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|error| ToolError::Execution {
            message: format!("cannot read {path:?}: {error}"),
        })?;
    let mut text = String::from_utf8(bytes).map_err(|_| ToolError::Execution {
        message: format!("{path:?} is not valid UTF-8 text"),
    })?;
    if text.len() > MAX_READ_BYTES {
        let mut cut = MAX_READ_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[... truncated at 64 KiB]");
    }
    Ok(ToolResult::Ok {
        content: ToolContent::Text { text },
    })
}

/// Lists a directory in the working directory.
#[derive(Tool, Deserialize, JsonSchema)]
pub struct ListDir {
    /// Directory relative to the working directory (default ".").
    pub path: Option<String>,
}

impl ListDir {
    async fn run(&self) -> Result<ToolResult, ToolError> {
        list_dir_at(&sandbox_root()?, self.path.as_deref()).await
    }
}

async fn list_dir_at(root: &Path, path: Option<&str>) -> Result<ToolResult, ToolError> {
    let resolved = resolve(root, path.unwrap_or("."))?;
    let mut reader =
        tokio::fs::read_dir(&resolved)
            .await
            .map_err(|error| ToolError::Execution {
                message: format!("cannot list {resolved:?}: {error}"),
            })?;
    let mut names = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| ToolError::Execution {
            message: format!("cannot read a directory entry: {error}"),
        })?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| ToolError::Execution {
                message: format!("cannot inspect a directory entry: {error}"),
            })?;
        let kind = if file_type.is_dir() {
            "dir "
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        names.push(format!("{kind} {}", entry.file_name().to_string_lossy()));
    }
    names.sort();
    let truncated = names.len() > MAX_DIR_ENTRIES;
    names.truncate(MAX_DIR_ENTRIES);
    let mut text = if names.is_empty() {
        "(empty directory)".to_string()
    } else {
        names.join("\n")
    };
    if truncated {
        text.push_str("\n[... truncated at 200 entries]");
    }
    Ok(ToolResult::Ok {
        content: ToolContent::Text { text },
    })
}

/// Reports metadata (size, kind, modification time) for a path.
#[derive(Tool, Deserialize, JsonSchema)]
pub struct FileInfo {
    /// Path relative to the working directory (or absolute inside it).
    pub path: String,
}

impl FileInfo {
    async fn run(&self) -> Result<ToolResult, ToolError> {
        file_info_at(&sandbox_root()?, &self.path).await
    }
}

async fn file_info_at(root: &Path, path: &str) -> Result<ToolResult, ToolError> {
    let resolved = resolve(root, path)?;
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|error| ToolError::Execution {
            message: format!("cannot stat {path:?}: {error}"),
        })?;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Ok(ToolResult::Ok {
        content: ToolContent::Json {
            value: json!({
                "path": resolved.display().to_string(),
                "is_dir": metadata.is_dir(),
                "size_bytes": metadata.len(),
                "modified_unix": modified_unix,
            }),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("synonz-chat-tui-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn derived_names_are_snake_case() {
        assert_eq!(ReadFile { path: ".".into() }.name(), "read_file");
        assert_eq!(ListDir { path: None }.name(), "list_dir");
        assert_eq!(FileInfo { path: ".".into() }.name(), "file_info");
    }

    #[tokio::test]
    async fn resolve_rejects_escapes() {
        let root = temp_dir("escape");
        let outside = root
            .parent()
            .expect("parent")
            .join("synonz-chat-tui-outside.txt");
        std::fs::write(&outside, "x").expect("write outside");
        let error = resolve(&root, outside.to_str().expect("utf-8")).unwrap_err();
        assert!(error.to_string().contains("escapes the sandbox"), "{error}");
        let _ = std::fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn read_file_truncates_large_files() {
        let root = temp_dir("read-large");
        std::fs::write(root.join("big.txt"), "a".repeat(70 * 1024)).expect("write");
        let result = read_file_at(&root, "big.txt").await.expect("read");
        let ToolResult::Ok {
            content: ToolContent::Text { text },
        } = result
        else {
            panic!("expected text content");
        };
        assert!(text.contains("truncated at 64 KiB"));
        assert!(text.len() < 66 * 1024);
    }

    #[tokio::test]
    async fn read_file_rejects_non_utf8() {
        let root = temp_dir("read-utf8");
        std::fs::write(root.join("bin"), [0xff, 0xfe, 0x00]).expect("write");
        let error = read_file_at(&root, "bin").await.unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"), "{error}");
    }

    #[tokio::test]
    async fn list_dir_sorts_and_caps() {
        let root = temp_dir("list");
        for index in 0..MAX_DIR_ENTRIES + 5 {
            std::fs::write(root.join(format!("f{index:03}")), "").expect("write");
        }
        let result = list_dir_at(&root, None).await.expect("list");
        let ToolResult::Ok {
            content: ToolContent::Text { text },
        } = result
        else {
            panic!("expected text content");
        };
        assert!(text.contains("f000"), "first entry missing");
        assert!(text.contains("truncated at 200 entries"));
        assert!(!text.contains("f204"), "cap must drop the overflow");
    }

    #[tokio::test]
    async fn file_info_reports_metadata() {
        let root = temp_dir("info");
        std::fs::write(root.join("note.txt"), "hello").expect("write");
        let result = file_info_at(&root, "note.txt").await.expect("info");
        let ToolResult::Ok {
            content: ToolContent::Json { value },
        } = result
        else {
            panic!("expected json content");
        };
        assert_eq!(value["is_dir"], false);
        assert_eq!(value["size_bytes"], 5);
    }
}
