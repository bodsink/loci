//! Count edges by type, and resolved calls by how they were resolved.

use std::collections::BTreeMap;

fn main() {
    let project = std::env::args()
        .nth(1)
        .expect("usage: edge_census <project>");
    let (_, store) = loci_index::open_project(&project).expect("open");
    let reader = store.read().expect("read");

    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_detail: BTreeMap<String, usize> = BTreeMap::new();
    for edge in reader.all_edges().expect("edges") {
        *by_type.entry(format!("{:?}", edge.edge_type)).or_default() += 1;
        if edge.edge_type == loci_graph::EdgeType::Calls {
            *by_detail
                .entry(edge.detail.clone().unwrap_or_else(|| "-".to_string()))
                .or_default() += 1;
        }
    }

    println!("=== sisi per jenis ===");
    for (name, count) in &by_type {
        println!("  {name:<18} {count}");
    }
    println!("\n=== panggilan terselesaikan, menurut caranya ===");
    for (name, count) in &by_detail {
        println!("  {name:<34} {count}");
    }
}
