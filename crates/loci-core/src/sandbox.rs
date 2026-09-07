use crate::error::{LociError, Result};
use std::path::{Component, Path, PathBuf};

/// A canonicalised project root that every file read must go through.
///
/// The engine never reads source outside the root the user chose. Symlinks that
/// point out of the root are rejected rather than followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let canonical = root
            .canonicalize()
            .map_err(|source| LociError::io(root, source))?;
        if !canonical.is_dir() {
            return Err(LociError::InvalidArgument(format!(
                "project root '{}' is not a directory",
                canonical.display()
            )));
        }
        Ok(Self { root: canonical })
    }

    /// Trusts an already-canonical path. Used when reopening a stored project
    /// root whose directory may have been removed since indexing.
    pub fn from_canonical(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a repository-relative path to an absolute one inside the root.
    ///
    /// Rejects absolute inputs, `..` traversal, and symlinks whose target escapes.
    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        if relative.is_absolute() {
            return Err(LociError::PathOutOfRoot(relative.to_path_buf()));
        }
        for component in relative.components() {
            match component {
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(LociError::PathOutOfRoot(relative.to_path_buf()));
                }
                _ => {}
            }
        }

        let joined = self.root.join(relative);
        // A missing file cannot be canonicalised; fall back to the lexical join,
        // which is already free of traversal by the check above.
        let resolved = match joined.canonicalize() {
            Ok(p) => p,
            Err(_) => return Ok(joined),
        };
        if !resolved.starts_with(&self.root) {
            return Err(LociError::PathOutOfRoot(relative.to_path_buf()));
        }
        Ok(resolved)
    }

    /// Convert an absolute path under the root into a repo-relative path.
    pub fn relativize(&self, absolute: impl AsRef<Path>) -> Result<PathBuf> {
        let absolute = absolute.as_ref();
        absolute
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| LociError::PathOutOfRoot(absolute.to_path_buf()))
    }

    /// True when an already-resolved absolute path lies inside the root.
    pub fn contains(&self, absolute: impl AsRef<Path>) -> bool {
        absolute.as_ref().starts_with(&self.root)
    }

    pub fn read_to_string(&self, relative: impl AsRef<Path>) -> Result<String> {
        let path = self.resolve(relative)?;
        std::fs::read_to_string(&path).map_err(|source| LociError::io(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn resolves_paths_inside_root() {
        let dir = temp_root();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let resolved = sandbox.resolve("src/main.rs").unwrap();
        assert!(sandbox.contains(&resolved));
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = temp_root();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = sandbox.resolve("../outside.txt").unwrap_err();
        assert_eq!(err.code(), "path_out_of_root");
    }

    #[test]
    fn rejects_absolute_paths() {
        let dir = temp_root();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = sandbox.resolve("/etc/passwd").unwrap_err();
        assert_eq!(err.code(), "path_out_of_root");
    }

    #[test]
    fn rejects_symlink_escaping_root() {
        let dir = temp_root();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = sandbox.resolve("link.txt").unwrap_err();
        assert_eq!(err.code(), "path_out_of_root");
    }

    #[test]
    fn relativizes_absolute_paths() {
        let dir = temp_root();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let absolute = sandbox.resolve("src/main.rs").unwrap();
        assert_eq!(
            sandbox.relativize(absolute).unwrap(),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn reads_file_contents_through_sandbox() {
        let dir = temp_root();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert_eq!(
            sandbox.read_to_string("src/main.rs").unwrap(),
            "fn main() {}\n"
        );
    }
}
