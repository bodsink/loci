use loci_core::{LociError, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The only client milestone 1 installs into.
pub const CURSOR_CONFIG: &str = ".cursor/mcp.json";
pub const SERVER_KEY: &str = "loci";

#[derive(Debug)]
pub struct InstallOutcome {
    pub config_path: PathBuf,
    pub binary_path: PathBuf,
    pub created_config: bool,
    pub replaced_existing_entry: bool,
    pub other_servers: Vec<String>,
    pub backup_path: Option<PathBuf>,
    /// What happened to the executable itself.
    pub binary: BinaryOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BinaryOutcome {
    /// Copied to a stable location on disk.
    Installed { from: PathBuf },
    /// Already running from where it belongs.
    AlreadyInPlace,
}

/// Where an installed `loci` lives.
///
/// A build directory is not a home for it: cargo target directories get wiped,
/// and pointing Cursor's config at one produces a server that works until the
/// next `cargo clean`. `~/.local/bin` is on the default PATH on the Linux
/// distributions this targets, and needs no privileges.
pub fn install_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        LociError::InvalidArgument("cannot locate HOME to choose an install directory".to_string())
    })?;
    Ok(PathBuf::from(home).join(".local/bin"))
}

/// Whether a directory is on the PATH this process inherited.
pub fn is_on_path(dir: &Path) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|entry| entry == dir)
}

/// Copy the running executable to `destination`.
///
/// Writes to a temporary file and renames, because a plain overwrite of a
/// running binary fails with `ETXTBSY`, and a half-copied `loci` on PATH is
/// worse than none.
fn install_binary(source: &Path, destination: &Path) -> Result<BinaryOutcome> {
    if source == destination {
        return Ok(BinaryOutcome::AlreadyInPlace);
    }

    if let Some(parent) = destination.parent() {
        loci_core::paths::ensure_dir(parent)?;
    }

    let temp = destination.with_extension("loci-tmp");
    std::fs::copy(source, &temp).map_err(|e| LociError::io(&temp, e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| LociError::io(&temp, e))?;
    }

    std::fs::rename(&temp, destination).map_err(|e| LociError::io(destination, e))?;
    Ok(BinaryOutcome::Installed {
        from: source.to_path_buf(),
    })
}

pub fn cursor_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        LociError::InvalidArgument("cannot locate HOME to find Cursor's MCP config".to_string())
    })?;
    Ok(PathBuf::from(home).join(CURSOR_CONFIG))
}

/// Absolute path to the running binary, which is what the config must point at.
fn current_binary() -> Result<PathBuf> {
    let path =
        std::env::current_exe().map_err(|e| LociError::io(PathBuf::from("<current_exe>"), e))?;
    path.canonicalize().map_err(|e| LociError::io(path, e))
}

/// Merge a `loci` entry into Cursor's MCP config.
///
/// Other servers are preserved untouched: this rewrites exactly one key. An
/// existing config is backed up before the first modification.
pub fn install_for_cursor(
    config_path: &Path,
    binary: &Path,
    binary_outcome: BinaryOutcome,
) -> Result<InstallOutcome> {
    let existed = config_path.exists();

    let mut root: Map<String, Value> = if existed {
        let bytes = std::fs::read(config_path).map_err(|e| LociError::io(config_path, e))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            LociError::Storage(format!(
                "{} is not valid JSON ({e}); fix or move it, then run loci install again",
                config_path.display()
            ))
        })?
    } else {
        Map::new()
    };

    let servers = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        LociError::Storage(format!(
            "'mcpServers' in {} is not an object",
            config_path.display()
        ))
    })?;

    let replaced = servers.contains_key(SERVER_KEY);
    let other_servers: Vec<String> = servers
        .keys()
        .filter(|k| k.as_str() != SERVER_KEY)
        .cloned()
        .collect();

    servers.insert(
        SERVER_KEY.to_string(),
        json!({
            "command": binary.to_string_lossy(),
            "args": ["mcp"],
        }),
    );

    let backup_path = if existed {
        let backup = config_path.with_extension("json.loci-backup");
        std::fs::copy(config_path, &backup).map_err(|e| LociError::io(&backup, e))?;
        Some(backup)
    } else {
        None
    };

    if let Some(parent) = config_path.parent() {
        loci_core::paths::ensure_dir(parent)?;
    }
    let mut serialised = serde_json::to_vec_pretty(&Value::Object(root))
        .map_err(|e| LociError::Storage(e.to_string()))?;
    serialised.push(b'\n');
    let temp = config_path.with_extension("json.loci-tmp");
    std::fs::write(&temp, &serialised).map_err(|e| LociError::io(&temp, e))?;
    std::fs::rename(&temp, config_path).map_err(|e| LociError::io(config_path, e))?;

    Ok(InstallOutcome {
        config_path: config_path.to_path_buf(),
        binary_path: binary.to_path_buf(),
        created_config: !existed,
        replaced_existing_entry: replaced,
        other_servers,
        backup_path,
        binary: binary_outcome,
    })
}

pub fn run() -> Result<InstallOutcome> {
    let running = current_binary()?;
    let destination = install_dir()?.join("loci");
    let binary_outcome = install_binary(&running, &destination)?;
    // The config must name the installed copy, so it keeps working after the
    // build directory this was run from is gone.
    install_for_cursor(&cursor_config_path()?, &destination, binary_outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_config_when_none_exists() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");

        let outcome = install_for_cursor(&config, Path::new("/usr/local/bin/loci"), BinaryOutcome::AlreadyInPlace).unwrap();
        assert!(outcome.created_config);

        let written: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["loci"]["command"],
            "/usr/local/bin/loci"
        );
        assert_eq!(written["mcpServers"]["loci"]["args"][0], "mcp");
    }

    #[test]
    fn preserves_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        std::fs::write(
            &config,
            r#"{"mcpServers":{"codebase-memory-mcp":{"command":"/opt/other","args":[]}}}"#,
        )
        .unwrap();

        let outcome = install_for_cursor(&config, Path::new("/usr/local/bin/loci"), BinaryOutcome::AlreadyInPlace).unwrap();
        assert_eq!(outcome.other_servers, vec!["codebase-memory-mcp"]);

        let written: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["codebase-memory-mcp"]["command"], "/opt/other",
            "an existing server must survive installation untouched"
        );
        assert!(written["mcpServers"]["loci"].is_object());
    }

    #[test]
    fn backs_up_before_modifying_an_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        std::fs::write(&config, r#"{"mcpServers":{}}"#).unwrap();

        let outcome = install_for_cursor(&config, Path::new("/bin/loci"), BinaryOutcome::AlreadyInPlace).unwrap();
        let backup = outcome.backup_path.expect("a backup must be written");
        assert!(backup.exists());
    }

    #[test]
    fn reinstalling_replaces_only_the_loci_entry() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");

        install_for_cursor(&config, Path::new("/old/loci"), BinaryOutcome::AlreadyInPlace).unwrap();
        let outcome = install_for_cursor(&config, Path::new("/new/loci"), BinaryOutcome::AlreadyInPlace).unwrap();

        assert!(outcome.replaced_existing_entry);
        let written: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["loci"]["command"], "/new/loci");
    }

    /// Installing has to leave a runnable `loci` behind. Registering the server
    /// in Cursor's config while `loci` is still only in a build directory is
    /// the difference between "download, install, done" and a tool the user
    /// cannot invoke.
    #[test]
    fn installing_puts_an_executable_binary_at_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("build/loci");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"#!/bin/sh\necho loci\n").unwrap();
        let destination = dir.path().join("bin/loci");

        let outcome = install_binary(&source, &destination).unwrap();

        assert_eq!(outcome, BinaryOutcome::Installed { from: source });
        assert!(destination.exists(), "the binary must exist after install");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&destination).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the installed binary must be executable");
        }
    }

    /// Replacing a binary that is currently executing fails with ETXTBSY on a
    /// straight overwrite, which is exactly what re-running `loci install`
    /// after an upgrade does.
    #[test]
    fn installing_over_an_existing_binary_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("new/loci");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"new version").unwrap();
        let destination = dir.path().join("bin/loci");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"old version").unwrap();

        install_binary(&source, &destination).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new version");
        assert!(
            !dir.path().join("bin/loci.loci-tmp").exists(),
            "the temporary file must not be left behind"
        );
    }

    #[test]
    fn installing_from_the_destination_is_not_a_copy_onto_itself() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("loci");
        std::fs::write(&binary, b"loci").unwrap();

        let outcome = install_binary(&binary, &binary).unwrap();

        assert_eq!(outcome, BinaryOutcome::AlreadyInPlace);
        assert_eq!(std::fs::read(&binary).unwrap(), b"loci");
    }

    /// The config has to name the installed copy. Pointing it at the build
    /// directory produces a server that stops working after `cargo clean`.
    #[test]
    fn the_config_points_at_the_installed_binary_not_the_build_output() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        let installed = dir.path().join(".local/bin/loci");

        install_for_cursor(
            &config,
            &installed,
            BinaryOutcome::Installed {
                from: PathBuf::from("/tmp/target/release/loci"),
            },
        )
        .unwrap();

        let written: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["loci"]["command"],
            installed.to_string_lossy().as_ref()
        );
    }

    #[test]
    fn refuses_to_clobber_a_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        std::fs::write(&config, "{ this is not json").unwrap();

        let err = install_for_cursor(&config, Path::new("/bin/loci"), BinaryOutcome::AlreadyInPlace).unwrap_err();
        assert_eq!(err.code(), "storage_error");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "{ this is not json",
            "a malformed config must be left exactly as it was"
        );
    }
}
