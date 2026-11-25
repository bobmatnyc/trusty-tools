//! The per-user usage log: one line per document each `size`, `summarize`,
//! `find` and `read` call served.
//!
//! Why: the miss log counts failures only, so "unused" and "zero misses"
//! looked the same in the pilot. The usage log is the denominator of a miss
//! rate (pilot finding).
//!
//! What: [`UsageEntry`] holds local facts only (time, command, absolute path,
//! hash, format, `tokens_est`, lines served, hits, the range `read` was
//! asked for, the query `find` was given, outcome, version, working
//! directory) and never document text. [`record`] appends it through
//! [`crate::logfile::append_line`] in the client process, so warm and
//! `--no-warm` calls record the same; a failed write is a stderr warning and
//! never fails the user's command. [`tally`] counts the log for the `usage`
//! command. The log is `$TRUSTY_SUMMARIZER_USAGE_LOG`, else `usage.jsonl`
//! beside the miss log.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::{Cache, content_hash};
use crate::config::Config;
use crate::convert::{ConvertInput, Converters, convert_cached, detect_format, read_limited};
use crate::error::Result;
use crate::record::{FileRecord, estimate_tokens};
use crate::trial::{self, TextSize};

/// Overrides the usage log's path (a file path).
pub const USAGE_LOG_ENV: &str = "TRUSTY_SUMMARIZER_USAGE_LOG";

/// The usage log: `$TRUSTY_SUMMARIZER_USAGE_LOG`, else `usage.jsonl` in the
/// miss log's directory (so a moved miss log moves it too).
pub fn usage_log_path() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os(USAGE_LOG_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let miss = trial::miss_log_path()?;
    Ok(miss.with_file_name("usage.jsonl"))
}

/// One line of the usage log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEntry {
    /// UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub ts: String,
    /// `size`, `summarize`, `find` or `read`.
    pub command: String,
    /// The document's absolute path.
    pub path: String,
    /// `sha256:<hex>` of its bytes; null when unreadable.
    pub hash: Option<String>,
    /// Detected format; null when unknown.
    pub format: Option<String>,
    /// `ceil(text_chars / 4)`; null when the text was not obtained.
    pub tokens_est: Option<usize>,
    /// `read` only: lines printed.
    pub lines_served: Option<usize>,
    /// `find` only: hits found (before the `max_hits` cut).
    pub hits: Option<usize>,
    /// `read` only: the range requested, `A-B` (B3). Absent on other
    /// commands and in lines written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// `find` only: the query as given (B3). Absent on other commands and
    /// in lines written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// `ok`, or the error kind (`io`, `unsupported_format`, ...).
    pub outcome: String,
    /// `trusty-summarizer` version.
    pub tool_version: String,
    /// The calling process's working directory.
    pub cwd: String,
}

impl UsageEntry {
    /// An `ok` entry for `path` (relative to `cwd`), facts unset.
    pub fn new(command: &str, cwd: &Path, path: &Path) -> UsageEntry {
        let joined = cwd.join(path);
        let abs = joined.canonicalize().unwrap_or(joined);
        UsageEntry {
            ts: trial::utc_now(),
            command: command.to_string(),
            path: abs.display().to_string(),
            hash: None,
            format: None,
            tokens_est: None,
            lines_served: None,
            hits: None,
            range: None,
            query: None,
            outcome: "ok".to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            cwd: cwd.display().to_string(),
        }
    }

    /// The entry for one `size` result.
    pub fn for_size(cwd: &Path, path: &Path, size: &std::result::Result<TextSize, String>) -> Self {
        let mut e = UsageEntry::new("size", cwd, path);
        e.hash = trial::hash_file(Path::new(&e.path)).ok().map(|(h, _)| h);
        match size {
            Ok(s) => {
                e.format = Some(s.format.clone());
                e.tokens_est = Some(s.tokens_est);
            }
            Err(kind) => e.outcome = kind.clone(),
        }
        e
    }

    /// The entry for one `summarize` record.
    pub fn for_record(cwd: &Path, record: &FileRecord) -> Self {
        let mut e = UsageEntry::new("summarize", cwd, Path::new(record.path()));
        match record {
            FileRecord::Ok(r) => {
                e.hash = Some(r.hash.clone());
                e.format = Some(r.format.clone());
                e.tokens_est = Some(r.tokens_est);
            }
            FileRecord::Error(r) => {
                e.hash = trial::hash_file(Path::new(&e.path)).ok().map(|(h, _)| h);
                e.format = r.error.format.clone();
                e.outcome = r.error.kind.clone();
            }
        }
        e
    }
}

/// A document's converted text, plus the hash and format of the bytes read
/// (`None` when the file could not be read). One read of the file.
///
/// Errors: as [`Converters::load_text_cached`].
pub fn load_text(
    config: &Config,
    path: &Path,
    cache: Option<&Cache>,
) -> (Result<String>, Option<(String, String)>) {
    let bytes = match read_limited(path, config.max_file_bytes) {
        Ok(b) => b,
        Err(e) => return (Err(e), None),
    };
    let format = detect_format(path, &bytes);
    let facts = Some((content_hash(&bytes), format.clone()));
    let converters = Converters::standard(config);
    let text = converters.get(&format).and_then(|converter| {
        let input = ConvertInput {
            path,
            bytes: &bytes,
            format: &format,
        };
        convert_cached(converter.as_ref(), &input, cache)
    });
    (text, facts)
}

/// Sets the hash, format and `tokens_est` of `entry` from [`load_text`]'s
/// result, or its outcome to the error kind.
pub fn fill_from_text(
    entry: &mut UsageEntry,
    text: &Result<String>,
    facts: Option<(String, String)>,
) {
    if let Some((hash, format)) = facts {
        entry.hash = Some(hash);
        entry.format = Some(format);
    }
    match text {
        Ok(t) => entry.tokens_est = Some(estimate_tokens(t.chars().count())),
        Err(e) => entry.outcome = e.kind().to_string(),
    }
}

/// Appends `entries` to the usage log; on failure prints one warning on
/// stderr and returns, so the user's command still succeeds.
///
/// Why: a broken or refused log must not stop a session reading documents,
/// but a silent loss would undercount the miss-rate denominator.
/// Test: `usage_lines_for_each_command_warm_and_no_warm`, `usage_write_failure_warns_and_keeps_exit_status`
pub fn record(entries: &[UsageEntry]) {
    if entries.is_empty() {
        return;
    }
    let result = usage_log_path().and_then(|log| {
        for e in entries {
            let line = serde_json::to_vec(e).map_err(io::Error::other)?;
            crate::logfile::append_line(&log, &line)
                .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", log.display())))?;
        }
        Ok(())
    });
    if let Err(e) = result {
        eprintln!("trusty-summarizer: warning: usage not recorded: {e}");
    }
}

/// Counts of the usage log, for the `usage` command.
#[derive(Debug, Default, Serialize)]
pub struct Tally {
    /// Entries counted (at or after `since`).
    pub count: usize,
    /// Distinct document paths.
    pub documents: usize,
    /// Entries whose outcome is not `ok`.
    pub errors: usize,
    /// Lines that are not a usage entry (skipped).
    pub unreadable_lines: usize,
    /// Per UTC day `YYYY-MM-DD`.
    pub by_day: BTreeMap<String, usize>,
    /// Per command.
    pub by_command: BTreeMap<String, usize>,
    /// Per working directory.
    pub by_cwd: BTreeMap<String, usize>,
}

/// Checks a `--since` value: `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ` (UTC).
///
/// Errors: any other shape.
pub fn parse_since(s: &str) -> std::result::Result<String, String> {
    let digits = |r: std::ops::Range<usize>| s.as_bytes()[r].iter().all(u8::is_ascii_digit);
    let b = s.as_bytes();
    let date = s.len() >= 10
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10);
    let ok = date
        && (s.len() == 10
            || (s.len() == 20
                && b[10] == b'T'
                && digits(11..13)
                && b[13] == b':'
                && digits(14..16)
                && b[16] == b':'
                && digits(17..19)
                && b[19] == b'Z'));
    if ok {
        Ok(s.to_string())
    } else {
        Err(format!(
            "--since {s:?}: expected YYYY-MM-DD or YYYY-MM-DDTHH:MM:SSZ (UTC)"
        ))
    }
}

/// Counts `lines` whose `ts` is at or after `since` (string order equals
/// time order for this timestamp format).
/// Test: `usage_command_counts_per_day_command_and_cwd`
pub fn tally(lines: &[String], since: Option<&str>) -> Tally {
    let mut t = Tally::default();
    let mut docs = std::collections::BTreeSet::new();
    for line in lines {
        let Ok(e) = serde_json::from_str::<UsageEntry>(line) else {
            t.unreadable_lines += 1;
            continue;
        };
        if since.is_some_and(|s| e.ts.as_str() < s) {
            continue;
        }
        t.count += 1;
        if e.outcome != "ok" {
            t.errors += 1;
        }
        *t.by_day
            .entry(e.ts.get(..10).unwrap_or("").to_string())
            .or_default() += 1;
        *t.by_command.entry(e.command).or_default() += 1;
        *t.by_cwd.entry(e.cwd).or_default() += 1;
        docs.insert(e.path);
    }
    t.documents = docs.len();
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_accepts_a_date_or_a_utc_time_only() {
        assert!(parse_since("2026-11-15").is_ok());
        assert!(parse_since("2026-11-15T08:00:00Z").is_ok());
        for bad in [
            "2026-11",
            "15-11-2026",
            "2026-11-15 08:00",
            "2026-11-15T08:00:00",
        ] {
            assert!(parse_since(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn range_and_query_round_trip_and_old_lines_still_parse() {
        let mut e = UsageEntry::new("read", Path::new("/w"), Path::new("/d.pdf"));
        e.range = Some("3-5".into());
        let line = serde_json::to_string(&e).unwrap();
        assert!(line.contains(r#""range":"3-5""#));
        assert!(!line.contains("query"));
        assert_eq!(serde_json::from_str::<UsageEntry>(&line).unwrap(), e);
        // A line written before B3.
        let old = r#"{"ts":"2026-11-15T00:00:00Z","command":"find","path":"/d.pdf","hash":null,"format":null,"tokens_est":null,"lines_served":null,"hits":2,"outcome":"ok","tool_version":"0.1.0","cwd":"/w"}"#;
        let parsed: UsageEntry = serde_json::from_str(old).unwrap();
        assert_eq!((parsed.range, parsed.query), (None, None));
        assert_eq!(tally(&[old.to_string()], None).count, 1);
    }

    #[test]
    fn tally_filters_by_since_and_skips_bad_lines() {
        let line = |ts: &str, cmd: &str, outcome: &str| {
            let mut e = UsageEntry::new(cmd, Path::new("/w"), Path::new("/d.pdf"));
            e.ts = ts.to_string();
            e.outcome = outcome.to_string();
            serde_json::to_string(&e).unwrap()
        };
        let lines = vec![
            line("2026-11-14T23:59:59Z", "read", "ok"),
            line("2026-11-15T00:00:00Z", "find", "ok"),
            line("2026-11-16T10:00:00Z", "read", "io"),
            "not json".to_string(),
        ];
        let all = tally(&lines, None);
        assert_eq!((all.count, all.errors, all.unreadable_lines), (3, 1, 1));
        assert_eq!(all.documents, 1);
        let t = tally(&lines, Some("2026-11-15"));
        assert_eq!(t.count, 2);
        assert_eq!(t.by_day.get("2026-11-15"), Some(&1));
        assert_eq!(t.by_command.get("read"), Some(&1));
        assert_eq!(t.by_cwd.get("/w"), Some(&2));
    }
}
