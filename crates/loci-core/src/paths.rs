use crate::error::{LociError, Result};
use std::path::{Path, PathBuf};

/// Root of everything loci writes. Never inside the indexed repository.
///
/// Resolution order: `LOCI_DATA_DIR`, then `$XDG_DATA_HOME/loci`, then
/// `$HOME/.local/share/loci`.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("LOCI_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        let dir = PathBuf::from(dir);
        if dir.is_absolute() {
            return Ok(dir.join("loci"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        LociError::InvalidArgument(
            "cannot locate a data directory: set LOCI_DATA_DIR or HOME".to_string(),
        )
    })?;
    Ok(PathBuf::from(home).join(".local/share/loci"))
}

pub fn projects_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("projects"))
}

pub fn catalog_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("catalog.json"))
}

/// Append-only journal of MCP tool calls. Tool names and argument keys only —
/// never source text. Used by `loci status --agent-usage`.
pub fn agent_journal_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("agent_calls.jsonl"))
}

/// PID of the `loci ui` process that last bound successfully.
pub fn ui_pid_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("ui.pid"))
}

pub fn project_store_dir(project_id: &str) -> Result<PathBuf> {
    Ok(projects_dir()?.join(project_id))
}

pub fn graph_db_path(project_id: &str) -> Result<PathBuf> {
    Ok(project_store_dir(project_id)?.join("graph.redb"))
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|source| LociError::io(path, source))
}

/// Derive a filesystem-safe project id from a display name.
///
/// Collisions between different roots are the caller's problem to detect; see
/// `disambiguate`.
pub fn sanitize_project_id(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

/// Suffix a project id with a short hash of its root so two different
/// directories with the same basename never share a store.
pub fn disambiguate(name: &str, root: &Path) -> String {
    let hash = blake3::hash(root.as_os_str().as_encoded_bytes());
    let short = &hash.to_hex()[..8];
    format!("{}-{}", sanitize_project_id(name), short)
}

/// Basename of a path, suitable as a default project name.
pub fn default_project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_project_id)
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honours_explicit_data_dir_override() {
        temp_env("LOCI_DATA_DIR", Some("/tmp/loci-test-data"), || {
            assert_eq!(data_dir().unwrap(), PathBuf::from("/tmp/loci-test-data"));
        });
    }

    #[test]
    fn sanitizes_unsafe_characters() {
        assert_eq!(sanitize_project_id("my repo/v2"), "my-repo-v2");
        assert_eq!(sanitize_project_id("///"), "project");
        assert_eq!(sanitize_project_id("ok_name-1.2"), "ok_name-1.2");
    }

    #[test]
    fn disambiguation_is_stable_and_root_specific() {
        let a = disambiguate("sample", Path::new("/a/sample"));
        let b = disambiguate("sample", Path::new("/b/sample"));
        assert_ne!(a, b);
        assert_eq!(a, disambiguate("sample", Path::new("/a/sample")));
        assert!(a.starts_with("sample-"));
    }

    #[test]
    fn default_name_uses_basename() {
        assert_eq!(
            default_project_name(Path::new("/home/x/Project/mcp")),
            "mcp"
        );
    }

    /// Serialises env mutation; Rust runs tests in threads sharing one env.
    fn temp_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os(key);
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
