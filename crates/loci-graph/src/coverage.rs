use crate::schema::{CoverageReason, CoverageStatus, FileRecord};
use crate::store::GraphReader;
use loci_core::Result;
use serde::Serialize;

/// Coverage answer for one exact path.
#[derive(Debug, Clone, Serialize)]
pub struct PathCoverage {
    pub path: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub symbols_in_graph: usize,
    /// What to do when the graph cannot be trusted for this path.
    pub fallback: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CoverageSummary {
    pub indexed: usize,
    pub parse_partial: usize,
    pub skipped: usize,
    pub excluded: usize,
    /// Files the walker deliberately never visited are not enumerated here;
    /// ask check_index_coverage for a specific path instead.
    pub note: &'static str,
}

pub const COVERAGE_NOTE: &str =
    "Best-effort signal. Absence from the skipped/parse_partial lists is not proof a file was \
     fully understood. Files excluded by .gitignore are not enumerated; query an exact path to \
     classify it.";

pub fn summarise(reader: &GraphReader) -> Result<CoverageSummary> {
    let mut summary = CoverageSummary {
        note: COVERAGE_NOTE,
        ..Default::default()
    };
    for record in reader.all_files()? {
        match record.status {
            CoverageStatus::Indexed => summary.indexed += 1,
            CoverageStatus::ParsePartial => summary.parse_partial += 1,
            CoverageStatus::Skipped => summary.skipped += 1,
            CoverageStatus::Excluded => summary.excluded += 1,
        }
    }
    Ok(summary)
}

fn fallback_for(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Indexed => "graph_is_authoritative_for_recorded_symbols",
        CoverageStatus::ParsePartial => "read_source_in_the_flagged_ranges",
        CoverageStatus::Skipped => "read_source_directly",
        CoverageStatus::Excluded => "not_indexed_by_design_change_ignore_rules_and_reindex",
    }
}

fn from_record(record: &FileRecord) -> PathCoverage {
    PathCoverage {
        path: record.path.clone(),
        status: record.status.as_str(),
        reason: record.reason.map(CoverageReason::as_str),
        detail: record.detail.clone(),
        language: record.language.map(|l| l.as_str().to_string()),
        symbols_in_graph: record.node_ids.len(),
        fallback: fallback_for(record.status),
    }
}

/// Classify an exact repository-relative path.
///
/// `excluded_by_ignore` lets the caller supply the answer for files the walker
/// never recorded because an ignore rule pruned them.
pub fn classify_path(
    reader: &GraphReader,
    path: &str,
    excluded_by_ignore: bool,
    exists_on_disk: bool,
) -> Result<PathCoverage> {
    if let Some(record) = reader.file_record(path)? {
        return Ok(from_record(&record));
    }

    if excluded_by_ignore {
        return Ok(PathCoverage {
            path: path.to_string(),
            status: CoverageStatus::Excluded.as_str(),
            reason: Some(CoverageReason::Gitignore.as_str()),
            detail: Some("matched an ignore rule; excluded by design".to_string()),
            language: None,
            symbols_in_graph: 0,
            fallback: fallback_for(CoverageStatus::Excluded),
        });
    }

    let detail = if exists_on_disk {
        "file exists but has no index record; it was added after the last index run or lies \
         outside the walked set"
    } else {
        "no such file under the project root at this path"
    };

    Ok(PathCoverage {
        path: path.to_string(),
        status: CoverageStatus::Skipped.as_str(),
        reason: None,
        detail: Some(detail.to_string()),
        language: None,
        symbols_in_graph: 0,
        fallback: fallback_for(CoverageStatus::Skipped),
    })
}

/// Coverage for every recorded file under a path prefix.
pub fn classify_scope(
    reader: &GraphReader,
    prefix: &str,
    limit: usize,
    offset: usize,
) -> Result<(Vec<PathCoverage>, usize, bool)> {
    let normalised = if prefix == "." || prefix.is_empty() {
        String::new()
    } else {
        prefix.trim_end_matches('/').to_string()
    };

    let mut matched: Vec<FileRecord> = reader
        .all_files()?
        .into_iter()
        .filter(|record| {
            normalised.is_empty()
                || record.path == normalised
                || record.path.starts_with(&format!("{normalised}/"))
        })
        .collect();
    matched.sort_by(|a, b| a.path.cmp(&b.path));

    let total = matched.len();
    let page: Vec<PathCoverage> = matched
        .iter()
        .skip(offset)
        .take(limit)
        .map(from_record)
        .collect();
    let has_more = offset + page.len() < total;
    Ok((page, total, has_more))
}
