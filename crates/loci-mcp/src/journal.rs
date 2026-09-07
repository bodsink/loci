use loci_core::{paths, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;

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
pub struct UsageSummary {
    pub total_calls: usize,
    pub by_tool: std::collections::BTreeMap<String, usize>,
    pub by_error_code: std::collections::BTreeMap<String, usize>,
    /// Ratio of graph-tool calls to search_code calls; low values mean the tool
    /// descriptions are not steering agents away from text search.
    pub graph_calls: usize,
    pub text_search_calls: usize,
    /// Tool call pairs, most frequent first, to reveal the sequences agents use.
    pub common_sequences: Vec<(String, usize)>,
    pub pages_requested: usize,
    pub truncated_responses: usize,
    pub journal_path: String,
}

const GRAPH_TOOLS: &[&str] = &[
    "search_graph",
    "query_graph",
    "trace_path",
    "get_architecture",
];

/// Read the journal and summarise how agents actually used the server.
pub fn summarise() -> Result<UsageSummary> {
    let path = paths::agent_journal_path()?;
    let mut summary = UsageSummary {
        total_calls: 0,
        by_tool: Default::default(),
        by_error_code: Default::default(),
        graph_calls: 0,
        text_search_calls: 0,
        common_sequences: Vec::new(),
        pages_requested: 0,
        truncated_responses: 0,
        journal_path: path.to_string_lossy().to_string(),
    };

    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(summary);
    };

    let entries: Vec<JournalEntry> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    for entry in &entries {
        summary.total_calls += 1;
        *summary.by_tool.entry(entry.tool.clone()).or_insert(0) += 1;
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
        }
    }

    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    for window in entries.windows(2) {
        let key = format!("{} -> {}", window[0].tool, window[1].tool);
        *pairs.entry(key).or_insert(0) += 1;
    }
    let mut sequences: Vec<(String, usize)> = pairs.into_iter().collect();
    sequences.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sequences.truncate(10);
    summary.common_sequences = sequences;

    Ok(summary)
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
}
