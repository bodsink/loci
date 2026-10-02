//! Keep indexed graphs aligned with the working tree without a manual re-index.
//!
//! The MCP process watches each project root with inotify. The first tool call
//! for a project refreshes it, which is what brings a graph that went stale
//! while Cursor was closed back up to date. Later calls refresh only after a
//! filesystem event.

use loci_core::Sandbox;
use loci_graph::catalog::Catalog;
use loci_index::walk;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Watched {
    id: String,
    root: PathBuf,
    /// False until this process has refreshed the project once.
    checked: bool,
    dirty: bool,
}

struct State {
    projects: Vec<Watched>,
    /// inotify watch descriptor to project id.
    wd_owner: HashMap<i32, String>,
}

pub struct Freshness {
    state: Arc<Mutex<State>>,
    /// `None` when inotify could not be created. Tool calls then refresh every time.
    fd: Option<i32>,
}

impl Freshness {
    /// Watch every catalogued project. A failure to watch still leaves the
    /// first tool call refreshing, so a missing watcher cannot serve a stale graph.
    pub fn start() -> Self {
        let projects = Catalog::load()
            .map(|catalog| {
                catalog
                    .projects
                    .iter()
                    .filter_map(|entry| {
                        let root = Path::new(&entry.root).canonicalize().ok()?;
                        Some(Watched {
                            id: entry.id.clone(),
                            root,
                            checked: false,
                            dirty: false,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let state = Arc::new(Mutex::new(State {
            projects,
            wd_owner: HashMap::new(),
        }));
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        let fd = if fd >= 0 { Some(fd) } else { None };
        let freshness = Self {
            state: Arc::clone(&state),
            fd,
        };
        if let Some(fd) = freshness.fd {
            spawn_reader(fd, Arc::clone(&state));
            let roots: Vec<(String, PathBuf)> = freshness
                .state
                .lock()
                .map(|guard| {
                    guard
                        .projects
                        .iter()
                        .map(|project| (project.id.clone(), project.root.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for (id, root) in roots {
                freshness.watch_tree(&id, &root);
            }
        } else {
            eprintln!("loci: inotify unavailable, each project tool will check the tree");
        }
        freshness
    }

    /// Non-recursive watches on directories the indexer would visit, so a
    /// `node_modules` tree is not added to the inotify set.
    fn watch_tree(&self, id: &str, root: &Path) {
        let Some(fd) = self.fd else {
            return;
        };
        let Ok(sandbox) = Sandbox::new(root) else {
            return;
        };
        let mut directories = HashSet::new();
        directories.insert(sandbox.root().to_path_buf());
        for candidate in walk::collect(&sandbox) {
            if let Some(parent) = candidate.absolute_path.parent() {
                directories.insert(parent.to_path_buf());
            }
        }
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB;
        for directory in directories {
            let Ok(name) = CString::new(directory.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: `fd` is an open inotify descriptor, `name` is a nul-terminated path.
            let wd = unsafe { libc::inotify_add_watch(fd, name.as_ptr(), mask) };
            if wd < 0 {
                eprintln!(
                    "loci: not watching {} ({id}): {}",
                    directory.display(),
                    std::io::Error::last_os_error()
                );
                continue;
            }
            if let Ok(mut guard) = self.state.lock() {
                guard.wd_owner.insert(wd, id.to_string());
            }
        }
    }

    /// Refresh before a project tool when this process has not checked yet, or
    /// a watched file has changed since the last refresh. Without inotify every
    /// call checks, because there is no event to trust.
    pub fn before_tool(&self, name: &str, args: &Value) {
        if matches!(
            name,
            "list_projects" | "get_graph_schema" | "index_repository" | "delete_project"
        ) {
            return;
        }
        let Some(requested) = args.get("project").and_then(Value::as_str) else {
            return;
        };
        let Ok(catalog) = Catalog::load() else {
            return;
        };
        let Ok(entry) = catalog.resolve(requested) else {
            return;
        };
        let root = Path::new(&entry.root)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&entry.root));
        let watching = self.fd.is_some();

        {
            let Ok(mut guard) = self.state.lock() else {
                return;
            };
            if !guard.projects.iter().any(|project| project.id == entry.id) {
                guard.projects.push(Watched {
                    id: entry.id.clone(),
                    root: root.clone(),
                    checked: false,
                    dirty: true,
                });
            }
            let Some(project) = guard
                .projects
                .iter_mut()
                .find(|project| project.id == entry.id)
            else {
                return;
            };
            if watching && project.checked && !project.dirty {
                return;
            }
            // Events that arrive while the index holds the write lock stay dirty.
            project.dirty = false;
            project.checked = true;
        }

        match loci_index::refresh_if_stale(&entry.id) {
            Ok(true) => self.watch_tree(&entry.id, &root),
            Ok(false) => {}
            Err(error) => {
                eprintln!("loci: incremental refresh of {} failed: {error}", entry.id);
                if let Ok(mut guard) = self.state.lock() {
                    if let Some(project) = guard
                        .projects
                        .iter_mut()
                        .find(|project| project.id == entry.id)
                    {
                        project.checked = false;
                        project.dirty = true;
                    }
                }
            }
        }
    }
}

fn spawn_reader(fd: i32, state: Arc<Mutex<State>>) {
    let _ = std::thread::Builder::new()
        .name("loci-watch".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                // SAFETY: `fd` stays open for the process lifetime and `buf` is writable.
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if n == 0 {
                    break;
                }
                let mut offset = 0usize;
                let n = n as usize;
                let header = std::mem::size_of::<libc::inotify_event>();
                while offset + header <= n {
                    // SAFETY: the kernel aligns each event and `offset` walks them in order.
                    let event = unsafe {
                        std::ptr::read_unaligned(
                            buf.as_ptr().add(offset) as *const libc::inotify_event
                        )
                    };
                    let wd = event.wd;
                    let len = event.len as usize;
                    if let Ok(mut guard) = state.lock() {
                        if let Some(id) = guard.wd_owner.get(&wd).cloned() {
                            if let Some(project) = guard.projects.iter_mut().find(|p| p.id == id) {
                                project.dirty = true;
                            }
                        }
                    }
                    offset += header + len;
                }
            }
        });
}
