use loci_core::{paths, Result};
use loci_graph::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_SECS: i64 = 86_400;

/// One recorded tool call.
///
/// Deliberately narrow: tool name, which argument *keys* were present, the
/// project id, latency and the error class. No argument values, no source text,
/// no file contents. Written locally and never transmitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub at_unix: i64,
    pub tool: String,
    /// Argument names supplied by the caller, sorted. Values are not recorded.
    pub argument_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub duration_ms: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Result size signal, so we can see whether agents paginate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

pub fn record(entry: &JournalEntry) -> Result<()> {
    let path = paths::agent_journal_path()?;
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent)?;
    }
    let line =
        serde_json::to_string(entry).map_err(|e| loci_core::LociError::Storage(e.to_string()))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| loci_core::LociError::io(&path, e))?;
    writeln!(file, "{line}").map_err(|e| loci_core::LociError::io(&path, e))?;
    Ok(())
}

pub fn entry_for(
    tool: &str,
    args: &Value,
    duration_ms: u64,
    outcome: &std::result::Result<Value, loci_core::LociError>,
) -> JournalEntry {
    let mut argument_keys: Vec<String> = args
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    argument_keys.sort();

    let (ok, error_code, result_total, has_more) = match outcome {
        Ok(value) => (
            // A handler can return a soft error inside a successful envelope,
            // e.g. ambiguous_symbol with candidates. Count that as not-ok so the
            // usage summary shows how often agents hit it.
            value.get("error").is_none(),
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            value.get("total").and_then(Value::as_u64),
            value.get("has_more").and_then(Value::as_bool),
        ),
        Err(e) => (false, Some(e.code().to_string()), None, None),
    };

    JournalEntry {
        at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        tool: tool.to_string(),
        argument_keys,
        project: args
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_string),
        duration_ms,
        ok,
        error_code,
        result_total,
        has_more,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStats {
    pub calls: usize,
    pub ok: usize,
    pub fail: usize,
    pub duration_p50_ms: u64,
    pub duration_p95_ms: u64,
    pub duration_max_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    pub project: Option<String>,
    pub calls: usize,
    pub ok: usize,
    pub fail: usize,
    pub by_tool: std::collections::BTreeMap<String, usize>,
    pub by_error_code: std::collections::BTreeMap<String, usize>,
    pub graph_calls: usize,
    pub text_search_calls: usize,
    pub pages_requested: usize,
    pub truncated_responses: usize,
    pub paged_followthrough: usize,
    pub first_at_unix: i64,
    pub last_at_unix: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionUsage {
    pub first_at_unix: i64,
    pub last_at_unix: i64,
    pub calls: usize,
    pub first_tool: String,
    pub list_projects_first: bool,
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageCriteria {
    pub sessions: usize,
    pub list_projects_first: usize,
    pub pagination_needed: usize,
    pub pagination_followed: usize,
    pub walk_calls: usize,
    pub walk_failures: usize,
    pub detect_changes: usize,
    pub check_index_coverage: usize,
    pub project_not_found: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub total_calls: usize,
    pub ok: usize,
    pub fail: usize,
    pub first_at_unix: Option<i64>,
    pub last_at_unix: Option<i64>,
    pub by_tool: std::collections::BTreeMap<String, usize>,
    pub by_tool_stats: std::collections::BTreeMap<String, ToolStats>,
    pub by_error_code: std::collections::BTreeMap<String, usize>,
    pub by_project: Vec<ProjectUsage>,
    /// Ratio of graph-tool calls to search_code calls; low values mean the tool
    /// descriptions are not steering agents away from text search.
    pub graph_calls: usize,
    pub text_search_calls: usize,
    /// Tool call pairs, most frequent first, to reveal the sequences agents use.
    pub common_sequences: Vec<(String, usize)>,
    pub pages_requested: usize,
    pub truncated_responses: usize,
    pub paged_followthrough: usize,
    pub criteria: UsageCriteria,
    pub sessions: Vec<SessionUsage>,
    /// `day` is the last 24 hours; `all` is the whole journal.
    pub window: String,
    pub window_since_unix: Option<i64>,
    pub journal_path: String,
}

const GRAPH_TOOLS: &[&str] = &[
    "search_graph",
    "query_graph",
    "trace_path",
    "get_architecture",
];

const WALK_TOOLS: &[&str] = &["query_graph", "trace_path"];
const SESSION_GAP_SECS: i64 = 30 * 60;

/// Read the journal and summarise how agents actually used the server.
pub fn summarise() -> Result<UsageSummary> {
    summarise_window("all")
}

/// `window` is `day` (last 24 hours) or `all`.
pub fn summarise_window(window: &str) -> Result<UsageSummary> {
    let window = if window.eq_ignore_ascii_case("day") {
        "day"
    } else {
        "all"
    };
    let since_unix = (window == "day").then(|| now_unix().saturating_sub(DAY_SECS));
    let path = paths::agent_journal_path()?;
    let aliases = load_project_aliases();
    let empty = empty_summary(path.to_string_lossy().to_string(), window, since_unix);

    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(empty);
    };

    let entries: Vec<JournalEntry> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(summarise_entries(
        &entries,
        path.to_string_lossy().to_string(),
        since_unix,
        &aliases,
        window,
    ))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn load_project_aliases() -> BTreeMap<String, String> {
    Catalog::load()
        .map(|catalog| project_aliases_from(&catalog))
        .unwrap_or_default()
}

fn project_aliases_from(catalog: &Catalog) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for entry in &catalog.projects {
        map.insert(entry.id.clone(), entry.id.clone());
        let name_hits = catalog
            .projects
            .iter()
            .filter(|p| p.name == entry.name)
            .count();
        if name_hits == 1 {
            map.insert(entry.name.clone(), entry.id.clone());
        }
        let Some(base) = Path::new(&entry.root)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let base_hits = catalog
            .projects
            .iter()
            .filter(|p| {
                Path::new(&p.root)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(base)
            })
            .count();
        if base_hits == 1 {
            map.insert(base.to_string(), entry.id.clone());
        }
    }
    map
}

fn resolve_recorded_project(
    recorded: Option<&str>,
    aliases: &BTreeMap<String, String>,
) -> Option<String> {
    let raw = recorded.filter(|id| !id.is_empty())?;
    Some(
        aliases
            .get(raw)
            .cloned()
            .unwrap_or_else(|| raw.to_string()),
    )
}

fn empty_summary(journal_path: String, window: &str, since_unix: Option<i64>) -> UsageSummary {
    UsageSummary {
        total_calls: 0,
        ok: 0,
        fail: 0,
        first_at_unix: None,
        last_at_unix: None,
        by_tool: Default::default(),
        by_tool_stats: Default::default(),
        by_error_code: Default::default(),
        by_project: Vec::new(),
        graph_calls: 0,
        text_search_calls: 0,
        common_sequences: Vec::new(),
        pages_requested: 0,
        truncated_responses: 0,
        paged_followthrough: 0,
        criteria: UsageCriteria {
            sessions: 0,
            list_projects_first: 0,
            pagination_needed: 0,
            pagination_followed: 0,
            walk_calls: 0,
            walk_failures: 0,
            detect_changes: 0,
            check_index_coverage: 0,
            project_not_found: 0,
        },
        sessions: Vec::new(),
        window: window.to_string(),
        window_since_unix: since_unix,
        journal_path,
    }
}

fn summarise_entries(
    entries: &[JournalEntry],
    journal_path: String,
    since_unix: Option<i64>,
    aliases: &BTreeMap<String, String>,
    window: &str,
) -> UsageSummary {
    let scoped: Vec<JournalEntry> = entries
        .iter()
        .filter(|entry| since_unix.is_none_or(|since| entry.at_unix >= since))
        .cloned()
        .collect();
    let mut summary = empty_summary(journal_path, window, since_unix);
    if scoped.is_empty() {
        return summary;
    }
    let entries = scoped.as_slice();

    summary.total_calls = entries.len();
    summary.first_at_unix = Some(entries[0].at_unix);
    summary.last_at_unix = Some(entries[entries.len() - 1].at_unix);

    let mut durations: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    let mut project_map: std::collections::BTreeMap<String, ProjectUsage> = Default::default();

    for (index, entry) in entries.iter().enumerate() {
        *summary.by_tool.entry(entry.tool.clone()).or_insert(0) += 1;
        durations
            .entry(entry.tool.clone())
            .or_default()
            .push(entry.duration_ms);
        if entry.ok {
            summary.ok += 1;
        } else {
            summary.fail += 1;
        }
        if let Some(code) = &entry.error_code {
            *summary.by_error_code.entry(code.clone()).or_insert(0) += 1;
        }
        if GRAPH_TOOLS.contains(&entry.tool.as_str()) {
            summary.graph_calls += 1;
        }
        if entry.tool == "search_code" {
            summary.text_search_calls += 1;
        }
        if entry.argument_keys.iter().any(|k| k == "cursor") {
            summary.pages_requested += 1;
        }
        if entry.has_more == Some(true) {
            summary.truncated_responses += 1;
            if followed_with_cursor(entries, index) {
                summary.paged_followthrough += 1;
            }
        }
        if WALK_TOOLS.contains(&entry.tool.as_str()) {
            summary.criteria.walk_calls += 1;
            if !entry.ok {
                summary.criteria.walk_failures += 1;
            }
        }
        if entry.tool == "detect_changes" {
            summary.criteria.detect_changes += 1;
        }
        if entry.tool == "check_index_coverage" {
            summary.criteria.check_index_coverage += 1;
        }
        if entry.error_code.as_deref() == Some("project_not_found") {
            summary.criteria.project_not_found += 1;
        }

        let project = resolve_recorded_project(entry.project.as_deref(), aliases);
        let key = project.clone().unwrap_or_default();
        let bucket = project_map
            .entry(key.clone())
            .or_insert_with(|| ProjectUsage {
                project: project.clone(),
                calls: 0,
                ok: 0,
                fail: 0,
                by_tool: Default::default(),
                by_error_code: Default::default(),
                graph_calls: 0,
                text_search_calls: 0,
                pages_requested: 0,
                truncated_responses: 0,
                paged_followthrough: 0,
                first_at_unix: entry.at_unix,
                last_at_unix: entry.at_unix,
            });
        bucket.calls += 1;
        if entry.ok {
            bucket.ok += 1;
        } else {
            bucket.fail += 1;
        }
        *bucket.by_tool.entry(entry.tool.clone()).or_insert(0) += 1;
        if let Some(code) = &entry.error_code {
            *bucket.by_error_code.entry(code.clone()).or_insert(0) += 1;
        }
        if GRAPH_TOOLS.contains(&entry.tool.as_str()) {
            bucket.graph_calls += 1;
        }
        if entry.tool == "search_code" {
            bucket.text_search_calls += 1;
        }
        if entry.argument_keys.iter().any(|k| k == "cursor") {
            bucket.pages_requested += 1;
        }
        if entry.has_more == Some(true) {
            bucket.truncated_responses += 1;
            if followed_with_cursor(entries, index) {
                bucket.paged_followthrough += 1;
            }
        }
        bucket.first_at_unix = bucket.first_at_unix.min(entry.at_unix);
        bucket.last_at_unix = bucket.last_at_unix.max(entry.at_unix);
    }

    for (tool, mut samples) in durations {
        samples.sort_unstable();
        let calls = *summary.by_tool.get(&tool).unwrap_or(&0);
        let fail = entries.iter().filter(|e| e.tool == tool && !e.ok).count();
        summary.by_tool_stats.insert(
            tool,
            ToolStats {
                calls,
                ok: calls.saturating_sub(fail),
                fail,
                duration_p50_ms: percentile(&samples, 0.50),
                duration_p95_ms: percentile(&samples, 0.95),
                duration_max_ms: *samples.last().unwrap_or(&0),
            },
        );
    }

    let mut by_project: Vec<ProjectUsage> = project_map.into_values().collect();
    by_project.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.project.cmp(&b.project)));
    summary.by_project = by_project;

    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    for window in entries.windows(2) {
        let key = format!("{} -> {}", window[0].tool, window[1].tool);
        *pairs.entry(key).or_insert(0) += 1;
    }
    let mut sequences: Vec<(String, usize)> = pairs.into_iter().collect();
    sequences.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sequences.truncate(10);
    summary.common_sequences = sequences;

    let sessions = split_sessions(entries);
    summary.criteria.sessions = sessions.len();
    summary.criteria.list_projects_first = sessions
        .iter()
        .filter(|session| session.first().is_some_and(|e| e.tool == "list_projects"))
        .count();
    summary.criteria.pagination_needed = summary.truncated_responses;
    summary.criteria.pagination_followed = summary.paged_followthrough;
    summary.sessions = sessions.iter().map(|session| session_usage(session, aliases)).collect();

    summary
}

fn session_usage(session: &[JournalEntry], aliases: &BTreeMap<String, String>) -> SessionUsage {
    let first = session.first();
    let last = session.last();
    let mut projects: Vec<String> = session
        .iter()
        .filter_map(|entry| resolve_recorded_project(entry.project.as_deref(), aliases))
        .collect();
    projects.sort();
    projects.dedup();
    SessionUsage {
        first_at_unix: first.map(|e| e.at_unix).unwrap_or(0),
        last_at_unix: last.map(|e| e.at_unix).unwrap_or(0),
        calls: session.len(),
        first_tool: first.map(|e| e.tool.clone()).unwrap_or_default(),
        list_projects_first: first.is_some_and(|e| e.tool == "list_projects"),
        projects,
    }
}

fn followed_with_cursor(entries: &[JournalEntry], index: usize) -> bool {
    let current = &entries[index];
    entries
        .iter()
        .skip(index + 1)
        .take(7)
        .take_while(|next| next.tool == current.tool && next.project == current.project)
        .any(|next| next.argument_keys.iter().any(|k| k == "cursor"))
}

fn split_sessions(entries: &[JournalEntry]) -> Vec<&[JournalEntry]> {
    let mut starts = vec![0];
    for index in 1..entries.len() {
        if entries[index].at_unix - entries[index - 1].at_unix > SESSION_GAP_SECS {
            starts.push(index);
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(entries.len());
            &entries[start..end]
        })
        .collect()
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn journal_records_keys_but_never_values() {
        let args = json!({ "project": "sample", "pattern": "SECRET_API_KEY" });
        let entry = entry_for("search_code", &args, 5, &Ok(json!({ "total": 3 })));

        let serialised = serde_json::to_string(&entry).unwrap();
        assert!(serialised.contains("pattern"), "argument keys are recorded");
        assert!(
            !serialised.contains("SECRET_API_KEY"),
            "argument values must never reach the journal"
        );
    }

    #[test]
    fn soft_errors_inside_a_successful_envelope_count_as_failures() {
        let outcome = Ok(json!({ "error": "ambiguous_symbol" }));
        let entry = entry_for("trace_path", &json!({}), 1, &outcome);
        assert!(!entry.ok);
        assert_eq!(entry.error_code.as_deref(), Some("ambiguous_symbol"));
    }

    #[test]
    fn hard_errors_record_their_code() {
        let outcome = Err(loci_core::LociError::ProjectNotFound("x".into()));
        let entry = entry_for("index_status", &json!({}), 1, &outcome);
        assert!(!entry.ok);
        assert_eq!(entry.error_code.as_deref(), Some("project_not_found"));
    }

    #[test]
    fn pagination_signals_are_captured() {
        let args = json!({ "project": "sample", "cursor": "50" });
        let entry = entry_for("search_graph", &args, 2, &Ok(json!({ "has_more": true })));
        assert!(entry.argument_keys.contains(&"cursor".to_string()));
        assert_eq!(entry.has_more, Some(true));
    }

    fn entry(
        at: i64,
        tool: &str,
        project: Option<&str>,
        ok: bool,
        duration: u64,
        keys: &[&str],
        has_more: Option<bool>,
    ) -> JournalEntry {
        JournalEntry {
            at_unix: at,
            tool: tool.into(),
            argument_keys: keys.iter().map(|k| (*k).to_string()).collect(),
            project: project.map(str::to_string),
            duration_ms: duration,
            ok,
            error_code: (!ok).then(|| "invalid_argument".into()),
            result_total: None,
            has_more,
        }
    }

    #[test]
    fn summarise_splits_global_and_per_project_and_scores_the_design_criteria() {
        let entries = vec![
            entry(100, "list_projects", None, true, 0, &[], Some(false)),
            entry(
                101,
                "search_graph",
                Some("alpha"),
                true,
                20,
                &["project", "name"],
                Some(true),
            ),
            entry(
                102,
                "search_graph",
                Some("alpha"),
                true,
                18,
                &["cursor", "project", "name"],
                Some(false),
            ),
            entry(
                103,
                "trace_path",
                Some("alpha"),
                false,
                11,
                &["from", "project"],
                None,
            ),
            entry(
                2000,
                "check_index_coverage",
                Some("beta"),
                true,
                5,
                &["project"],
                None,
            ),
            entry(
                2001,
                "search_code",
                Some("beta"),
                true,
                40,
                &["pattern", "project"],
                Some(true),
            ),
        ];
        let summary = summarise_entries(
            &entries,
            "/tmp/agent_calls.jsonl".into(),
            None,
            &BTreeMap::new(),
            "all",
        );

        assert_eq!(summary.total_calls, 6);
        assert_eq!(summary.ok, 5);
        assert_eq!(summary.fail, 1);
        assert_eq!(summary.graph_calls, 3);
        assert_eq!(summary.text_search_calls, 1);
        assert_eq!(summary.paged_followthrough, 1);
        assert_eq!(summary.truncated_responses, 2);
        assert_eq!(summary.criteria.sessions, 2);
        assert_eq!(summary.criteria.list_projects_first, 1);
        assert_eq!(summary.criteria.walk_failures, 1);
        assert_eq!(summary.criteria.check_index_coverage, 1);
        assert_eq!(summary.criteria.detect_changes, 0);
        assert_eq!(summary.criteria.project_not_found, 0);
        assert_eq!(summary.sessions.len(), 2);
        assert_eq!(summary.sessions[0].first_tool, "list_projects");
        assert!(summary.sessions[0].list_projects_first);
        assert_eq!(summary.sessions[1].first_tool, "check_index_coverage");
        assert!(!summary.sessions[1].list_projects_first);
        assert_eq!(summary.by_project.len(), 3);
        let alpha = summary
            .by_project
            .iter()
            .find(|p| p.project.as_deref() == Some("alpha"))
            .expect("alpha");
        assert_eq!(alpha.calls, 3);
        assert_eq!(alpha.fail, 1);
        assert_eq!(alpha.paged_followthrough, 1);
        assert_eq!(summary.by_tool_stats["search_graph"].duration_p50_ms, 20);
    }

    #[test]
    fn a_day_window_drops_calls_older_than_the_cutoff() {
        let entries = vec![
            entry(100, "list_projects", None, true, 0, &[], None),
            entry(2000, "search_graph", Some("alpha"), true, 4, &["project"], None),
        ];
        let summary = summarise_entries(
            &entries,
            "/tmp/agent_calls.jsonl".into(),
            Some(1500),
            &BTreeMap::new(),
            "day",
        );
        assert_eq!(summary.window, "day");
        assert_eq!(summary.window_since_unix, Some(1500));
        assert_eq!(summary.total_calls, 1);
        assert_eq!(summary.by_tool["search_graph"], 1);
        assert!(!summary.by_tool.contains_key("list_projects"));
    }

    #[test]
    fn catalog_aliases_merge_basename_and_id_into_one_project() {
        let entries = vec![
            entry(1, "search_graph", Some("mcp"), true, 1, &["project"], None),
            entry(2, "search_graph", Some("loci"), true, 1, &["project"], None),
            entry(3, "list_projects", None, true, 1, &[], None),
        ];
        let mut aliases = BTreeMap::new();
        aliases.insert("mcp".into(), "loci".into());
        aliases.insert("loci".into(), "loci".into());
        let summary = summarise_entries(
            &entries,
            "/tmp/agent_calls.jsonl".into(),
            None,
            &aliases,
            "all",
        );
        let loci = summary
            .by_project
            .iter()
            .find(|p| p.project.as_deref() == Some("loci"))
            .expect("merged loci");
        assert_eq!(loci.calls, 2);
        assert!(!summary
            .by_project
            .iter()
            .any(|p| p.project.as_deref() == Some("mcp")));
        assert_eq!(summary.sessions[0].projects, vec!["loci".to_string()]);
    }

    #[test]
    fn freshness_and_wrong_project_id_are_scored() {
        let mut missing = entry(
            10,
            "index_status",
            Some("folder-name"),
            false,
            2,
            &["project"],
            None,
        );
        missing.error_code = Some("project_not_found".into());
        let entries = vec![
            entry(1, "detect_changes", Some("alpha"), true, 3, &["project"], None),
            entry(
                2,
                "check_index_coverage",
                Some("alpha"),
                true,
                3,
                &["project", "scopes"],
                None,
            ),
            missing,
        ];
        let summary = summarise_entries(
            &entries,
            "/tmp/agent_calls.jsonl".into(),
            None,
            &BTreeMap::new(),
            "all",
        );
        assert_eq!(summary.criteria.detect_changes, 1);
        assert_eq!(summary.criteria.check_index_coverage, 1);
        assert_eq!(summary.criteria.project_not_found, 1);
        assert_eq!(summary.by_error_code["project_not_found"], 1);
    }

    #[test]
    fn unique_root_basename_is_an_alias_for_the_catalog_id() {
        let catalog = Catalog {
            projects: vec![loci_graph::catalog::ProjectEntry {
                id: "loci".into(),
                name: "loci".into(),
                root: "/home/irin/Project/mcp".into(),
                store_path: "/tmp/loci/graph.redb".into(),
                indexed_at_unix: 1,
            }],
        };
        let aliases = project_aliases_from(&catalog);
        assert_eq!(aliases.get("mcp").map(String::as_str), Some("loci"));
        assert_eq!(aliases.get("loci").map(String::as_str), Some("loci"));
    }
}
