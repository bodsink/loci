use crate::schema::{CoverageReason, CoverageStatus, FileRecord};
use crate::store::GraphReader;
use loci_core::Result;
use serde::Serialize;
use std::collections::BTreeMap;

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

/// One file standing for every file skipped the same way in the same place.
#[derive(Debug, Clone, Serialize)]
pub struct SampledFile<'a> {
    #[serde(flatten)]
    pub file: &'a FileRecord,
    /// Further files in that directory left out for the same reason.
    pub others_like_it: usize,
}

/// Choose up to `cap` files that show as many distinct situations as possible.
///
/// Taking the alphabetical head instead lets one directory own the sample: a
/// `docs/mocks/` holding twenty PNGs answers "binary" twenty times and hides
/// every other reason a file was left out. One representative per
/// directory-and-reason pair answers "what kinds of thing did you skip",
/// which is the question a sample exists to answer.
///
/// Padding the spare slots with the files that were folded away would undo
/// exactly that, so the count travels with the representative instead. The
/// total is reported separately, and an exact path can always be classified
/// with check_index_coverage.
pub fn sample_files(files: &[FileRecord], cap: usize) -> Vec<SampledFile<'_>> {
    let mut chosen: Vec<SampledFile> = Vec::new();
    let mut index = BTreeMap::new();

    for file in files {
        let directory = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let key = (directory, file.reason.map(CoverageReason::as_str));
        match index.get(&key) {
            Some(&position) => {
                let folded: &mut SampledFile = &mut chosen[position];
                folded.others_like_it += 1;
            }
            None => {
                index.insert(key, chosen.len());
                chosen.push(SampledFile {
                    file,
                    others_like_it: 0,
                });
            }
        }
    }

    chosen.truncate(cap);
    chosen
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::CoverageStatus;

    fn skipped(path: &str, reason: CoverageReason) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: String::new(),
            size: 0,
            language: None,
            status: CoverageStatus::Skipped,
            reason: Some(reason),
            detail: None,
            node_ids: Vec::new(),
        }
    }

    /// Shaped after the case that prompted this: a mocks directory of PNGs
    /// sorting ahead of everything else and answering "binary" over and over.
    #[test]
    fn one_directory_cannot_take_over_the_sample() {
        let mut files = Vec::new();
        for n in 0..20 {
            files.push(skipped(
                &format!("docs/mocks/shot-{n:02}.png"),
                CoverageReason::Binary,
            ));
        }
        files.push(skipped("Makefile", CoverageReason::UnsupportedLanguage));
        files.push(skipped(
            "cmake/toolchain.cmake",
            CoverageReason::UnsupportedLanguage,
        ));

        let picked = super::sample_files(&files, 5);
        let paths: Vec<&str> = picked.iter().map(|s| s.file.path.as_str()).collect();

        let from_mocks = paths
            .iter()
            .filter(|p| p.starts_with("docs/mocks/"))
            .count();
        assert_eq!(
            from_mocks, 1,
            "one directory-and-reason pair earns one slot: {paths:?}"
        );
        assert!(
            paths.contains(&"Makefile") && paths.contains(&"cmake/toolchain.cmake"),
            "the situations that appear once must survive: {paths:?}"
        );
    }

    /// Folding files away loses information unless the count goes with the
    /// representative, so the reader can tell one stray PNG from twenty.
    #[test]
    fn the_representative_carries_how_many_it_stands_for() {
        let files: Vec<FileRecord> = (0..10)
            .map(|n| skipped(&format!("docs/shot-{n}.png"), CoverageReason::Binary))
            .collect();

        let picked = super::sample_files(&files, 4);

        assert_eq!(
            picked.len(),
            1,
            "ten identical situations are one situation"
        );
        assert_eq!(
            picked[0].others_like_it, 9,
            "the other nine must still be accounted for"
        );
    }

    /// Same reason, different directory, is a different situation: a vendored
    /// tree and a docs folder both full of binaries are worth seeing apart.
    #[test]
    fn the_same_reason_in_another_directory_earns_its_own_slot() {
        let files = vec![
            skipped("docs/a.png", CoverageReason::Binary),
            skipped("assets/b.png", CoverageReason::Binary),
        ];

        assert_eq!(super::sample_files(&files, 5).len(), 2);
    }

    #[test]
    fn a_short_list_is_returned_whole() {
        let files = vec![
            skipped("a.png", CoverageReason::Binary),
            skipped("Makefile", CoverageReason::UnsupportedLanguage),
        ];

        let picked = super::sample_files(&files, 10);
        assert_eq!(picked.len(), 2);
        assert!(
            picked.iter().all(|s| s.others_like_it == 0),
            "nothing was folded, so nothing stands for anything else"
        );
    }
}
