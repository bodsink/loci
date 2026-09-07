//! Measures indexing throughput and warm query latency.
//!
//! Every number printed is measured on the machine that ran it. Nothing here
//! asserts a target; the point is to find out what the engine actually does.
//!
//! Usage: `cargo run --release -p loci-bench -- [path-to-repo]`
//! Defaults to `fixtures/sample`.

use loci_graph::query::{self, Direction, SearchRequest};
use loci_index::IndexOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Discard the slowest tail: one outlier from a scheduler hiccup should not be
/// reported as the engine's latency.
fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index]
}

fn summarise(label: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    let total: Duration = samples.iter().sum();
    let mean = total / samples.len().max(1) as u32;
    println!(
        "  {label:<28} n={:<6} mean={:>9.3} ms  p50={:>9.3} ms  p95={:>9.3} ms  max={:>9.3} ms",
        samples.len(),
        mean.as_secs_f64() * 1000.0,
        percentile(&samples, 0.50).as_secs_f64() * 1000.0,
        percentile(&samples, 0.95).as_secs_f64() * 1000.0,
        samples.last().copied().unwrap_or_default().as_secs_f64() * 1000.0,
    );
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

fn main() {
    let repo: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/sample"));

    // Never touch the developer's real index.
    let data_dir = tempfile::tempdir().expect("temp data dir");
    std::env::set_var("LOCI_DATA_DIR", data_dir.path());

    println!("loci benchmark");
    println!("  repository : {}", repo.display());
    println!(
        "  build      : {}",
        if cfg!(debug_assertions) {
            "debug (numbers will be several times worse than release)"
        } else {
            "release"
        }
    );
    println!();

    let (report, cold) = time(|| {
        loci_index::index_repository(
            &repo,
            &IndexOptions {
                name: Some("bench".to_string()),
                full: true,
                hybrid_lsp: false,
            },
        )
        .expect("index the repository")
    });

    println!("Indexing");
    println!(
        "  cold full index            {:.1} ms for {} files, {} nodes, {} edges",
        cold.as_secs_f64() * 1000.0,
        report.files_indexed,
        report.nodes,
        report.edges
    );

    let (_, warm) = time(|| {
        loci_index::index_repository(
            &repo,
            &IndexOptions {
                name: Some("bench".to_string()),
                full: false,
                hybrid_lsp: false,
            },
        )
        .expect("re-index")
    });
    println!(
        "  incremental, no changes    {:.1} ms (hashing every file, re-resolving every edge)",
        warm.as_secs_f64() * 1000.0
    );
    println!();

    let (_, store) = loci_index::open_project("bench").expect("open the indexed project");
    let reader = store.read().expect("read transaction");

    // Pick real symbols from the graph so the benchmark exercises hits, not misses.
    let callables: Vec<_> = reader
        .all_nodes()
        .expect("nodes")
        .into_iter()
        .filter(|n| n.label.is_callable())
        .collect();
    if callables.is_empty() {
        println!("No callable symbols found; nothing to measure.");
        return;
    }

    const ITERATIONS: usize = 2_000;
    println!("Warm query latency");

    let mut exact_name = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let target = &callables[i % callables.len()];
        let request = SearchRequest {
            name: Some(target.name.clone()),
            limit: Some(50),
            ..Default::default()
        };
        let (_, elapsed) = time(|| query::search(&reader, &request).expect("search"));
        exact_name.push(elapsed);
    }
    summarise("search_graph by name", exact_name);

    let mut by_qn = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let target = &callables[i % callables.len()];
        let request = SearchRequest {
            qualified_name: Some(target.qualified_name.clone()),
            limit: Some(50),
            ..Default::default()
        };
        let (_, elapsed) = time(|| query::search(&reader, &request).expect("search"));
        by_qn.push(elapsed);
    }
    summarise("search_graph by qualified", by_qn);

    let mut traces = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let target = &callables[i % callables.len()];
        let (_, elapsed) =
            time(|| query::trace(&reader, target, Direction::Both, 3, 100, 0).expect("trace"));
        traces.push(elapsed);
    }
    summarise("trace_path depth 3", traces);

    let mut prefix = Vec::with_capacity(ITERATIONS / 4);
    for i in 0..ITERATIONS / 4 {
        let target = &callables[i % callables.len()];
        let stem: String = target.name.chars().take(3).collect();
        let request = SearchRequest {
            name_pattern: Some(format!("^{}", regex_escape(&stem))),
            limit: Some(50),
            ..Default::default()
        };
        let (_, elapsed) = time(|| query::search(&reader, &request).expect("search"));
        prefix.push(elapsed);
    }
    summarise("search_graph by prefix regex", prefix);

    println!();
    println!(
        "Graph: {} nodes, {} edges, {} callables.",
        reader.all_nodes().map(|n| n.len()).unwrap_or(0),
        reader.all_edges().map(|e| e.len()).unwrap_or(0),
        callables.len()
    );
    println!(
        "These are measurements from this run on this machine, not guarantees. Latency scales with \
         the number of matches a query returns, so a broad pattern will be slower than an exact name."
    );
}

/// Escape the handful of metacharacters a symbol name can contain.
fn regex_escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                vec![c]
            } else {
                vec!['\\', c]
            }
        })
        .collect()
}
