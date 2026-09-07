//! The `loci` command line interface.
//!
//! Exit codes: 0 on success, 1 on a handled loci error, 2 on bad usage.

mod install;

use clap::{Parser, Subcommand};
use loci_core::{LociError, Result};
use loci_graph::{catalog::Catalog, coverage, query::SearchRequest};
use loci_index::IndexOptions;
use serde_json::json;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "loci",
    version,
    about = "Local-first code intelligence for AI coding agents",
    long_about = "loci indexes a repository into a persistent local graph and serves it to \
                  MCP clients such as Cursor. Nothing is uploaded and no API key is needed."
)]
struct Cli {
    /// Emit JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register loci with Cursor by adding a server entry to ~/.cursor/mcp.json.
    Install {
        /// Only `cursor` is supported in this release.
        #[arg(long, default_value = "cursor")]
        client: String,
    },

    /// Index a repository into the local graph.
    Index {
        /// Path to the repository root.
        path: PathBuf,
        /// Project id to use instead of the directory basename.
        #[arg(long)]
        name: Option<String>,
        /// Re-parse every file, ignoring content hashes.
        #[arg(long)]
        full: bool,
        /// Ask installed language servers about calls the AST could not
        /// resolve. Slower, and only useful where a server is on PATH.
        #[arg(long)]
        lsp: bool,
    },

    /// Show what is indexed. With no argument, lists every project.
    Status {
        /// Project id from `loci status`.
        project: Option<String>,
        /// Summarise how MCP agents have been calling this server.
        #[arg(long)]
        agent_usage: bool,
    },

    /// Search the graph for symbols. A human-facing wrapper around search_graph.
    Query {
        #[arg(long)]
        project: String,
        /// Exact simple name, case-insensitive.
        #[arg(long)]
        name: Option<String>,
        /// Regex over the simple name.
        #[arg(long)]
        pattern: Option<String>,
        /// Restrict to one label, e.g. Function, Class, Route.
        #[arg(long)]
        label: Option<String>,
        /// Regex over the repository-relative file path.
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },

    /// Show which files changed since the last index run.
    Changes {
        project: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Delete a project's graph. The source repository is untouched.
    Delete { project: String },

    /// Run the MCP server over stdio. Cursor starts this; you rarely run it by hand.
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if cli.json {
                let payload = json!({ "error": error.code(), "message": error.to_string() });
                println!("{payload}");
            } else {
                eprintln!("error [{}]: {error}", error.code());
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Install { client } => cmd_install(client, cli.json),
        Command::Index {
            path,
            name,
            full,
            lsp,
        } => cmd_index(path, name.clone(), *full, *lsp, cli.json),
        Command::Status {
            project,
            agent_usage,
        } => cmd_status(project.as_deref(), *agent_usage, cli.json),
        Command::Query {
            project,
            name,
            pattern,
            label,
            file,
            limit,
        } => cmd_query(project, name, pattern, label, file, *limit, cli.json),
        Command::Changes { project, limit } => cmd_changes(project, *limit, cli.json),
        Command::Delete { project } => cmd_delete(project, cli.json),
        Command::Mcp => cmd_mcp(),
    }
}

fn cmd_install(client: &str, as_json: bool) -> Result<()> {
    if client != "cursor" {
        return Err(LociError::InvalidArgument(format!(
            "only --client cursor is supported in this release; got '{client}'"
        )));
    }

    let outcome = install::run()?;

    let install_dir = outcome
        .binary_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let on_path = install::is_on_path(&install_dir);

    if as_json {
        println!(
            "{}",
            json!({
                "client": "cursor",
                "config_path": outcome.config_path,
                "binary": outcome.binary_path,
                "binary_installed": matches!(outcome.binary, install::BinaryOutcome::Installed { .. }),
                "install_dir_on_path": on_path,
                "created_config": outcome.created_config,
                "replaced_existing_entry": outcome.replaced_existing_entry,
                "other_servers_preserved": outcome.other_servers,
                "backup": outcome.backup_path,
            })
        );
        return Ok(());
    }

    println!("Registered loci with Cursor.");
    println!("  config : {}", outcome.config_path.display());
    println!("  binary : {}", outcome.binary_path.display());
    if let install::BinaryOutcome::Installed { from } = &outcome.binary {
        println!("  copied : from {}", from.display());
    }
    if let Some(backup) = &outcome.backup_path {
        println!("  backup : {}", backup.display());
    }
    if !outcome.other_servers.is_empty() {
        println!(
            "  kept   : {} (existing servers were not modified)",
            outcome.other_servers.join(", ")
        );
    }

    println!();
    if on_path {
        println!("Next:");
        println!("  1. loci index /path/to/your/repo");
    } else {
        // Cursor is configured either way, because its config holds the
        // absolute path. Only the shell command is missing.
        println!("{} is not on your PATH.", install_dir.display());
        println!("Cursor will still work, but the `loci` command will not. To fix it:");
        println!();
        println!(
            "  echo 'export PATH=\"{}:$PATH\"' >> ~/.bashrc && exec bash",
            install_dir.display()
        );
        println!();
        println!("Next:");
        println!(
            "  1. {} index /path/to/your/repo",
            outcome.binary_path.display()
        );
    }
    println!("  2. Reload MCP servers in Cursor (Settings -> MCP -> refresh, or restart Cursor).");
    println!("  3. Ask the agent to call list_projects.");
    Ok(())
}

fn cmd_index(
    path: &Path,
    name: Option<String>,
    full: bool,
    hybrid_lsp: bool,
    as_json: bool,
) -> Result<()> {
    let report = loci_index::index_repository(
        path,
        &IndexOptions {
            name,
            full,
            hybrid_lsp,
        },
    )?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Indexed '{}' in {} ms", report.project, report.duration_ms);
    if let Some(note) = &report.name_note {
        println!("  note   : {note}");
    }
    println!("  root   : {}", report.root);
    println!("  store  : {}", report.store_path);
    println!("  graph  : {} nodes, {} edges", report.nodes, report.edges);
    println!(
        "  graph  : {} files indexed, {} parse_partial, {} skipped",
        report.files_indexed, report.files_parse_partial, report.files_skipped
    );
    println!(
        "  this run: {} reparsed, {} unchanged, {} removed",
        report.files_reparsed, report.files_unchanged, report.files_removed
    );

    let p = &report.phase_ms;
    println!(
        "  phases : walk {} ms, hash {} ms, parse {} ms, load {} ms, edges {} ms, lsp {} ms, report {} ms",
        p.walk, p.hash, p.parse, p.load_facts, p.resolve_edges, p.hybrid_lsp, p.report
    );

    if report.hybrid_lsp.enabled {
        for language in &report.hybrid_lsp.languages {
            match &language.skipped_reason {
                Some(reason) => println!("  lsp    : {} skipped ({reason})", language.language),
                None => println!(
                    "  lsp    : {} resolved {}/{} via {} in {} ms ({} unanswered, {} outside graph)",
                    language.language,
                    language.resolved,
                    language.attempted,
                    language.server,
                    language.duration_ms,
                    language.unanswered,
                    language.outside_graph
                ),
            }
        }
        if report.hybrid_lsp.budget_exhausted {
            println!("  lsp    : stopped early; the time budget ran out");
        }
    }

    if !report.languages.is_empty() {
        let languages: Vec<String> = report
            .languages
            .iter()
            .map(|(language, count)| format!("{language} {count}"))
            .collect();
        println!("  langs  : {}", languages.join(", "));
    }

    for example in &report.parse_partial_examples {
        println!("  partial: {example}");
    }
    if !report.skipped_examples.is_empty() {
        println!("  skipped examples:");
        for example in &report.skipped_examples {
            match example.others_like_it {
                0 => println!("    {} ({})", example.path, example.reason),
                n => println!(
                    "    {} ({}, and {n} more like it here)",
                    example.path, example.reason
                ),
            }
        }
    }
    println!();
    print_note(report.coverage_note);
    Ok(())
}

fn cmd_status(project: Option<&str>, agent_usage: bool, as_json: bool) -> Result<()> {
    if agent_usage {
        let summary = loci_mcp::journal::summarise()?;
        if as_json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
            return Ok(());
        }
        println!("Agent usage ({} calls)", summary.total_calls);
        println!("  journal: {}", summary.journal_path);
        if summary.total_calls == 0 {
            println!("  No MCP tool calls recorded yet.");
            return Ok(());
        }
        println!(
            "  graph tools: {}, search_code: {}",
            summary.graph_calls, summary.text_search_calls
        );
        println!(
            "  truncated responses: {}, follow-up pages requested: {}",
            summary.truncated_responses, summary.pages_requested
        );
        println!("  by tool:");
        for (tool, count) in &summary.by_tool {
            println!("    {tool}: {count}");
        }
        if !summary.by_error_code.is_empty() {
            println!("  errors:");
            for (code, count) in &summary.by_error_code {
                println!("    {code}: {count}");
            }
        }
        if !summary.common_sequences.is_empty() {
            println!("  common sequences:");
            for (sequence, count) in &summary.common_sequences {
                println!("    {sequence}: {count}");
            }
        }
        return Ok(());
    }

    let Some(project) = project else {
        let catalog = Catalog::load()?;
        if as_json {
            println!("{}", serde_json::to_string_pretty(&catalog)?);
            return Ok(());
        }
        if catalog.projects.is_empty() {
            println!("No projects indexed yet. Run: loci index <path>");
            return Ok(());
        }
        println!("{} project(s):", catalog.projects.len());
        for entry in &catalog.projects {
            println!("  {}  {}", entry.id, entry.root);
        }
        println!();
        println!("Run 'loci status <project>' for details.");
        return Ok(());
    };

    let (_, store) = loci_index::open_project(project)?;
    let reader = store.read()?;
    let meta = reader
        .meta()?
        .ok_or_else(|| LociError::IndexMissing(project.to_string()))?;
    let summary = coverage::summarise(&reader)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "project": project,
                "meta": meta,
                "coverage": summary,
                "labels": reader.label_counts()?,
            }))?
        );
        return Ok(());
    }

    println!("Project '{}'", meta.name);
    println!("  root   : {}", meta.root);
    println!(
        "  graph  : {} nodes, {} edges",
        meta.node_count, meta.edge_count
    );
    println!("  files  : {}", meta.file_count);
    println!(
        "  built  : {} ms at unix {}",
        meta.duration_ms, meta.indexed_at_unix
    );
    println!(
        "  cover  : {} indexed, {} parse_partial, {} skipped",
        summary.indexed, summary.parse_partial, summary.skipped
    );

    let labels = reader.label_counts()?;
    let rendered: Vec<String> = labels
        .iter()
        .map(|(label, count)| format!("{label} {count}"))
        .collect();
    println!("  labels : {}", rendered.join(", "));
    println!();
    print_note(coverage::COVERAGE_NOTE);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_query(
    project: &str,
    name: &Option<String>,
    pattern: &Option<String>,
    label: &Option<String>,
    file: &Option<String>,
    limit: usize,
    as_json: bool,
) -> Result<()> {
    if name.is_none() && pattern.is_none() && label.is_none() && file.is_none() {
        return Err(LociError::InvalidArgument(
            "pass at least one of --name, --pattern, --label or --file".to_string(),
        ));
    }

    let (_, store) = loci_index::open_project(project)?;
    let reader = store.read()?;
    let response = loci_graph::query::search(
        &reader,
        &SearchRequest {
            name: name.clone(),
            qualified_name: None,
            name_pattern: pattern.clone(),
            label: label.clone(),
            file_pattern: file.clone(),
            limit: Some(limit),
            offset: Some(0),
        },
    )?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    if response.results.is_empty() {
        println!("No symbol matched.");
        println!("The graph holds no such symbol; that is not proof the code lacks one.");
        println!("Check coverage with: loci status {project}");
        return Ok(());
    }

    println!(
        "{} match(es), showing {}:",
        response.total,
        response.results.len()
    );
    for hit in &response.results {
        println!(
            "  {:<9} {}  {}:{}-{}  in:{} out:{}",
            hit.label,
            hit.qualified_name,
            hit.file_path,
            hit.start_line,
            hit.end_line,
            hit.in_degree,
            hit.out_degree
        );
    }
    if response.has_more {
        println!();
        println!("More results available; raise --limit to see them.");
    }
    Ok(())
}

fn cmd_changes(project: &str, limit: usize, as_json: bool) -> Result<()> {
    let (entry, store) = loci_index::open_project(project)?;
    let sandbox = loci_core::Sandbox::new(&entry.root)?;
    let report = loci_index::changes::detect_changes(&store, &sandbox, project, limit, 0)?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "{} added, {} modified, {} removed, {} unchanged",
        report.added, report.modified, report.removed, report.unchanged
    );
    for file in &report.files {
        println!("  {:<9} {}", file.change, file.path);
    }
    if report.total > 0 {
        println!();
        println!("Run 'loci index {}' to refresh the graph.", entry.root);
    }
    Ok(())
}

fn cmd_delete(project: &str, as_json: bool) -> Result<()> {
    let entry = loci_index::delete_project(project)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }
    println!("Deleted graph for '{}'.", entry.id);
    println!("The repository at {} was not touched.", entry.root);
    Ok(())
}

/// Print the coverage caveat folded onto short lines.
///
/// The note is one 191-character string because MCP returns it as a JSON
/// field. Left to the terminal, it soft-wraps at whatever the window happens
/// to be, and a copied transcript then shows the wrap point as a missing
/// space. Folding it here makes the printed form independent of window width.
fn print_note(note: &str) {
    for line in fold(note, 78) {
        println!("{line}");
    }
}

fn fold(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn cmd_mcp() -> Result<()> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        // Running this by hand is almost always a mistake; say so on stderr,
        // which an MCP client ignores, and keep serving anyway.
        eprintln!(
            "loci mcp speaks JSON-RPC over stdin/stdout. Cursor starts this for you after \
             'loci install'. Press Ctrl-D to exit."
        );
    }

    let output = std::io::stdout();
    loci_mcp::serve_stdio(stdin.lock(), output.lock())
        .map_err(|e| LociError::io(PathBuf::from("<stdio>"), e))
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn folding_keeps_every_word_and_respects_the_width() {
        let folded = fold(loci_graph::coverage::COVERAGE_NOTE, 78);

        assert!(
            folded.len() > 1,
            "a 191-character note must not stay on one line"
        );
        for line in &folded {
            assert!(
                line.chars().count() <= 78,
                "line exceeds the width: {line:?}"
            );
        }
        assert_eq!(
            folded.join(" "),
            loci_graph::coverage::COVERAGE_NOTE,
            "folding must not drop or add a word"
        );
    }

    /// The wrap point is where a space goes missing when a soft-wrapped
    /// terminal is copied, so the phrase either side of it must stay intact.
    #[test]
    fn a_word_is_never_split_across_lines() {
        for line in fold(loci_graph::coverage::COVERAGE_NOTE, 78) {
            assert!(
                !line.starts_with(' ') && !line.ends_with(' '),
                "a line must not carry padding: {line:?}"
            );
        }
        assert_eq!(
            fold("alpha beta gamma", 11),
            vec!["alpha beta", "gamma"],
            "words must break only at spaces"
        );
    }
}
