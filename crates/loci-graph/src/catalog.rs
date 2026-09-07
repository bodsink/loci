use loci_core::{paths, LociError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One indexed project as recorded in `catalog.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Stable id used by every MCP tool's `project` argument.
    pub id: String,
    /// Display name; equals `id` unless a collision forced disambiguation.
    pub name: String,
    /// Absolute, canonicalised project root.
    pub root: String,
    pub store_path: String,
    pub indexed_at_unix: i64,
}

/// The list of projects loci knows about. Small enough to rewrite atomically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

impl Catalog {
    pub fn load() -> Result<Self> {
        let path = paths::catalog_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                LociError::Storage(format!("catalog at {} is corrupt: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(LociError::io(path, e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::catalog_path()?;
        self.save_to(&path)
    }

    /// Writes via a temporary file then renames, so a crash cannot leave a
    /// half-written catalog behind.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| LociError::Storage(format!("cannot serialise catalog: {e}")))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| LociError::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| LociError::io(path, e))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ProjectEntry> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn require(&self, id: &str) -> Result<&ProjectEntry> {
        self.get(id)
            .ok_or_else(|| LociError::ProjectNotFound(id.to_string()))
    }

    pub fn find_by_root(&self, root: &Path) -> Option<&ProjectEntry> {
        let root = root.to_string_lossy();
        self.projects.iter().find(|p| p.root == root)
    }

    pub fn upsert(&mut self, entry: ProjectEntry) {
        match self.projects.iter_mut().find(|p| p.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.projects.push(entry),
        }
        self.projects.sort_by(|a, b| a.id.cmp(&b.id));
    }

    pub fn remove(&mut self, id: &str) -> Option<ProjectEntry> {
        let position = self.projects.iter().position(|p| p.id == id)?;
        Some(self.projects.remove(position))
    }

    /// Pick an id for a root: the plain name if free, otherwise the name with a
    /// short hash of the root appended. Re-indexing the same root keeps its id.
    pub fn allocate_id(&self, name: &str, root: &Path) -> String {
        if let Some(existing) = self.find_by_root(root) {
            return existing.id.clone();
        }
        let base = paths::sanitize_project_id(name);
        if self.get(&base).is_none() {
            return base;
        }
        paths::disambiguate(name, root)
    }
}

pub fn store_path_for(id: &str) -> Result<PathBuf> {
    paths::graph_db_path(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, root: &str) -> ProjectEntry {
        ProjectEntry {
            id: id.to_string(),
            name: id.to_string(),
            root: root.to_string(),
            store_path: format!("/data/{id}/graph.redb"),
            indexed_at_unix: 0,
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");

        let mut catalog = Catalog::default();
        catalog.upsert(entry("sample", "/home/x/sample"));
        catalog.save_to(&path).unwrap();

        let loaded = Catalog::load_from(&path).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.get("sample").unwrap().root, "/home/x/sample");
    }

    #[test]
    fn missing_catalog_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::load_from(&dir.path().join("absent.json")).unwrap();
        assert!(catalog.projects.is_empty());
    }

    #[test]
    fn allocates_hashed_id_on_name_collision() {
        let mut catalog = Catalog::default();
        catalog.upsert(entry("sample", "/home/a/sample"));

        let id = catalog.allocate_id("sample", Path::new("/home/b/sample"));
        assert_ne!(id, "sample");
        assert!(id.starts_with("sample-"));
    }

    #[test]
    fn reindexing_the_same_root_keeps_the_id() {
        let mut catalog = Catalog::default();
        catalog.upsert(entry("sample", "/home/a/sample"));
        assert_eq!(
            catalog.allocate_id("sample", Path::new("/home/a/sample")),
            "sample"
        );
    }

    #[test]
    fn requiring_an_unknown_project_is_explicit() {
        let catalog = Catalog::default();
        let err = catalog.require("nope").unwrap_err();
        assert_eq!(err.code(), "project_not_found");
    }
}
