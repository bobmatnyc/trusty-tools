//! Corruption detection and offline repair for `sessions.json` (#5007).
//!
//! Why: on 2026-08-06 the owner's `~/.trusty-mpm/session-manager/sessions.json`
//! held a complete JSON document ending at byte 145090 followed by 1111 stale
//! bytes from a longer earlier version of the same document. `serde_json`
//! reported only `trailing characters at line 3755 column 2`, which names
//! neither the file nor a byte offset, and no code path could repair it —
//! [`super::store::SessionStore`] reloads before every save, so a file it
//! cannot read is a file it can never rewrite. The store stayed wedged for an
//! unknown period until a human hand-truncated it.
//! What: [`analyze`] turns a parse failure into a diagnostic that names the
//! file, the byte offset where the valid document ends, and how many bytes
//! follow it. [`plan`] and [`repair`] automate the manual repair: find the
//! longest complete JSON document at the head, verify the discarded tail names
//! no session absent from it, back the whole file up to a timestamped sibling,
//! then truncate.
//! Test: `store_integrity_tests.rs` — every function in this file has a named
//! test there.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::store::StoredData;

/// Length of a canonical hyphenated UUID (`8-4-4-4-12`).
const UUID_LEN: usize = 36;

/// The one place that decides whether a store file's bytes are loadable.
///
/// Why (#5027 review): before this existed, `tm doctor` and
/// [`plan`] each parsed a `serde_json::Value` while
/// [`SessionStore::load`](super::store::SessionStore::load) parsed
/// [`StoredData`]. `SessionRecord` has six fields with no serde default, so
/// plenty of valid JSON is not a loadable store — and for such a file the
/// doctor positively asserted health with a record count while every write
/// failed, and the store told the operator to run `tm repair session-store`
/// while that command answered "parses cleanly; nothing to repair". Three
/// surfaces, three verdicts. Routing all three through one function makes the
/// agreement structural instead of coincidental.
/// What: deserializes `raw` as the store's real on-disk schema, turning a
/// failure into the full [`StoreIntegrity`] diagnosis.
/// Test: `session_store_check_agrees_with_the_store_about_what_loads`,
/// `plan_and_the_store_agree_that_valid_json_is_not_a_store`.
pub(crate) fn validate(path: &Path, raw: &str) -> Result<StoredData, StoreIntegrity> {
    serde_json::from_str::<StoredData>(raw).map_err(|e| analyze(path, raw, &e))
}

/// What a store file's bytes actually contain.
///
/// Why: "trailing characters at line 3755 column 2" told the operator nothing
/// actionable — not which file, not where the good data ends, not how much of
/// the file is junk. Every one of those is knowable at the moment of failure,
/// so this carries them.
/// What: the path, the file's total byte length, the byte length of the longest
/// complete JSON document at the head (`None` when the head itself is
/// unparseable), and serde's own message plus its line/column.
/// Test: `analyze_locates_the_valid_prefix_of_a_trailing_tail`,
/// `analyze_reports_no_valid_prefix_for_a_broken_head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreIntegrity {
    /// The store file the diagnosis is about.
    pub path: PathBuf,
    /// Total byte length of the file as read.
    pub total_bytes: usize,
    /// Byte length of the longest complete JSON document at the head of the
    /// file, or `None` when no complete document could be read at all.
    pub valid_prefix_bytes: Option<usize>,
    /// Whether cutting the file at `valid_prefix_bytes` leaves a document the
    /// daemon can actually load.
    ///
    /// Why (#5027 review): the diagnostic used to advertise
    /// `tm repair session-store` for every parse failure, including
    /// `{"sessions": 5}` — valid JSON, no trailing junk, and a file the repair
    /// command refuses. It also described "0 trailing byte(s)", junk that is
    /// not there. Measuring whether truncation would help is what lets the
    /// message, and [`plan`], agree about it.
    /// What: `true` only when `raw[..valid_prefix_bytes]` deserializes as the
    /// store's schema — i.e. the file really is a good document followed by a
    /// stale tail, the 2026-08-06 shape.
    /// Test: `diagnostic_does_not_advertise_a_repair_that_would_refuse`.
    pub repairable_by_truncation: bool,
    /// `serde_json`'s own message, verbatim.
    pub detail: String,
    /// 1-based line reported by `serde_json`.
    pub line: usize,
    /// 1-based column reported by `serde_json`.
    pub column: usize,
}

impl StoreIntegrity {
    /// How many bytes sit past the end of the valid document.
    ///
    /// Why: this is the number that identified the failure shape — 1111 stale
    /// bytes of a longer earlier document — and the number an operator needs to
    /// decide whether a repair is discarding a rounding error or half the file.
    /// What: `total_bytes - valid_prefix_bytes`, or `total_bytes` when nothing
    /// at the head parsed (the whole file is unusable).
    /// Test: `trailing_bytes_counts_the_discarded_tail`.
    pub fn trailing_bytes(&self) -> usize {
        self.total_bytes
            .saturating_sub(self.valid_prefix_bytes.unwrap_or(0))
    }

    /// One line an operator can act on.
    ///
    /// Why: the message has to name the file (there is more than one JSON store
    /// under `~/.trusty-mpm/`), the byte offset (so a human can verify the cut
    /// before trusting the tool), and the fix. Naming a fix that will refuse is
    /// worse than naming none (#5027 review), so `tm repair session-store` is
    /// advertised only when [`Self::repairable_by_truncation`] says it would
    /// work.
    /// What: path, serde's message, and either the measured cut plus the repair
    /// command, or why truncation cannot help.
    /// Test: `diagnostic_names_the_file_the_offset_and_the_repair_command`,
    /// `diagnostic_does_not_advertise_a_repair_that_would_refuse`.
    pub fn diagnostic(&self) -> String {
        let head = format!(
            "{} is corrupt: {} (line {}, column {})",
            self.path.display(),
            self.detail,
            self.line,
            self.column,
        );
        if self.repairable_by_truncation {
            return format!(
                "{head}; a complete JSON document ends at byte {} of {}, followed by {} trailing \
                 byte(s). Repair with `tm repair session-store`",
                self.valid_prefix_bytes.unwrap_or(0),
                self.total_bytes,
                self.trailing_bytes(),
            );
        }
        let why = match self.valid_prefix_bytes {
            None => format!(
                "no complete JSON document could be read from its {} byte(s)",
                self.total_bytes
            ),
            Some(_) if self.trailing_bytes() == 0 => format!(
                "its {} byte(s) are one complete JSON document, but not a session store",
                self.total_bytes
            ),
            Some(valid) => format!(
                "the complete JSON document at its first {valid} byte(s) is not a session store \
                 either"
            ),
        };
        format!(
            "{head}; {why}. Truncation cannot recover it — restore a backup, or move the file aside to start fresh"
        )
    }
}

/// Diagnose a store file whose parse just failed.
///
/// Why: the parse failure is the only moment where both the bytes and the
/// error are in hand, so the diagnosis is cheapest and most accurate here.
/// What: replays `raw` through a streaming deserializer to find where the first
/// complete JSON value ends, and pairs that offset with serde's message.
/// Test: `analyze_locates_the_valid_prefix_of_a_trailing_tail`,
/// `analyze_reports_no_valid_prefix_for_a_broken_head`.
pub fn analyze(path: &Path, raw: &str, err: &serde_json::Error) -> StoreIntegrity {
    let valid = valid_prefix_bytes(raw);
    StoreIntegrity {
        path: path.to_path_buf(),
        total_bytes: raw.len(),
        valid_prefix_bytes: valid,
        // The one question that decides what the operator should be told to do:
        // would cutting here leave a store the daemon can load?
        repairable_by_truncation: valid
            .is_some_and(|v| serde_json::from_str::<StoredData>(&raw[..v]).is_ok()),
        detail: err.to_string(),
        line: err.line(),
        column: err.column(),
    }
}

/// Byte length of the longest complete JSON document at the head of `raw`.
///
/// Why: this is the whole basis of the automated repair. A writer that rewrote
/// a shorter document over a longer one leaves a complete document followed by
/// the old tail, so the recoverable data is exactly the head — and its length
/// has to be measured, not guessed from serde's line/column.
/// What: pulls one `serde_json::Value` off a streaming deserializer and reports
/// the stream's byte offset afterwards. `None` when the head is itself broken.
/// Test: `valid_prefix_bytes_measures_a_whole_document`,
/// `valid_prefix_bytes_is_none_for_a_truncated_head`.
pub fn valid_prefix_bytes(raw: &str) -> Option<usize> {
    let mut stream = serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(_)) => Some(stream.byte_offset()),
        _ => None,
    }
}

/// Why a repair could not be performed.
#[derive(Debug, Error)]
pub enum RepairError {
    /// The file parses fine — there is nothing to repair.
    #[error("session store {0} parses cleanly; nothing to repair")]
    NotCorrupt(PathBuf),

    /// Not even the head of the file is a complete JSON document.
    #[error(
        "session store {path} holds no complete JSON document in its {total_bytes} byte(s); \
         truncation cannot recover it — restore from a backup"
    )]
    NoValidPrefix {
        /// The store file.
        path: PathBuf,
        /// Its byte length.
        total_bytes: usize,
    },

    /// The file holds valid JSON that is not a session store, so there is
    /// nothing truncation could cut to make it load.
    ///
    /// Why (#5027 review): `{"sessions": 5}` used to return
    /// [`RepairError::NotCorrupt`] — "parses cleanly; nothing to repair" —
    /// while the store reported it as corrupt and pointed at this very command.
    /// The dead end was the disagreement, not the file.
    /// Test: `plan_and_the_store_agree_that_valid_json_is_not_a_store`.
    #[error(
        "session store {path} holds valid JSON that is not a session store: {detail}; \
         truncation cannot recover it — restore a backup, or move the file aside to start fresh"
    )]
    NotAStore {
        /// The store file.
        path: PathBuf,
        /// The schema error, verbatim.
        detail: String,
    },

    /// The bytes about to be discarded name sessions the recovered document
    /// does not have.
    #[error(
        "refusing to truncate {path}: the {discarded} discarded byte(s) name session id(s) that \
         the recovered document does not contain ({}); re-run with --force to discard them anyway",
        orphans.join(", ")
    )]
    OrphanedSessions {
        /// The store file.
        path: PathBuf,
        /// How many bytes truncation would drop.
        discarded: usize,
        /// The ids found only in the discarded tail.
        orphans: Vec<String>,
    },

    /// Reading, backing up, or truncating the file failed.
    #[error("session store repair I/O error on {path}: {source}")]
    Io {
        /// The file being operated on.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// What a repair would do, computed without writing anything.
///
/// Why: the fix deletes bytes from the operator's only copy of the fleet's
/// state. Planning first is what lets `tm repair session-store` show the cut
/// before making it, and what lets [`repair`] refuse a cut that would lose a
/// session.
/// What: the diagnosis, how many sessions the recovered document holds, and the
/// session ids named ONLY in the bytes that would be discarded.
/// Test: `repair_backs_up_then_truncates_to_the_valid_document`,
/// `repair_refuses_an_orphaned_tail_without_force`.
#[derive(Debug, Clone)]
pub struct RepairPlan {
    /// The diagnosis this plan is derived from.
    pub integrity: StoreIntegrity,
    /// Number of session records the recovered document holds.
    pub recovered_sessions: usize,
    /// Session ids named in the discarded tail but absent from the recovered
    /// document — the only reason to refuse an automatic repair.
    pub orphan_ids: Vec<String>,
}

/// Result of a completed repair.
#[derive(Debug, Clone)]
pub struct RepairOutcome {
    /// Where the original bytes were copied before truncation.
    pub backup: PathBuf,
    /// Bytes kept.
    pub kept_bytes: usize,
    /// Bytes discarded.
    pub discarded_bytes: usize,
    /// Sessions in the recovered document.
    pub sessions: usize,
    /// Orphan ids discarded because `--force` was passed. Empty on a clean
    /// repair.
    pub forced_orphans: Vec<String>,
}

/// Read `path` and compute what a repair would do. Writes nothing.
///
/// Why: see [`RepairPlan`]. Separating the plan from the write is also what
/// makes the safety check testable without touching a file the test would then
/// have to restore.
/// What: reads the file, returns [`RepairError::NotCorrupt`] when it parses,
/// [`RepairError::NoValidPrefix`] when nothing at the head is a whole document,
/// otherwise a plan carrying the orphan-id verdict.
/// Test: `plan_refuses_a_healthy_store`,
/// `repair_rejects_a_file_with_no_valid_prefix`,
/// `repair_refuses_an_orphaned_tail_without_force`,
/// `plan_and_the_store_agree_that_valid_json_is_not_a_store`.
pub fn plan(path: &Path) -> Result<RepairPlan, RepairError> {
    plan_bytes(path, &read(path)?)
}

/// Read a store file, reporting the path in any I/O error.
///
/// Why: [`plan`] and [`repair`] both need the bytes, and [`repair`] must read
/// them EXACTLY ONCE — see its doc.
/// What: `read_to_string` with the path attached to the error.
/// Test: exercised by every `plan_*` and `repair_*` test.
fn read(path: &Path) -> Result<String, RepairError> {
    std::fs::read_to_string(path).map_err(|source| RepairError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Compute the plan from bytes already in hand.
///
/// Why (#5027 review): [`repair`] used to call [`plan`] and then re-read the
/// file, slicing the second read at an offset measured from the first — a panic
/// if the file had shrunk in between, a silent mis-truncation otherwise.
/// Planning from a caller-owned `raw` is what lets `repair` read once.
/// What: the body of [`plan`], minus the read.
/// Test: `repair_backs_up_then_truncates_to_the_valid_document`,
/// `plan_and_the_store_agree_that_valid_json_is_not_a_store`.
fn plan_bytes(path: &Path, raw: &str) -> Result<RepairPlan, RepairError> {
    let integrity = match validate(path, raw) {
        Ok(_) => return Err(RepairError::NotCorrupt(path.to_path_buf())),
        Err(integrity) => integrity,
    };
    let Some(valid) = integrity.valid_prefix_bytes else {
        return Err(RepairError::NoValidPrefix {
            path: path.to_path_buf(),
            total_bytes: integrity.total_bytes,
        });
    };
    // Truncation is a REPAIR only if what survives is a store the daemon can
    // load. Cutting a file whose head is not a store leaves it exactly as
    // wedged as it was, minus the evidence.
    let recovered = validate(path, &raw[..valid]).map_err(|i| RepairError::NotAStore {
        path: path.to_path_buf(),
        detail: i.detail,
    })?;
    let kept_ids = kept_ids(&recovered);
    let orphan_ids = orphaned_ids(raw, valid, &kept_ids);
    Ok(RepairPlan {
        recovered_sessions: kept_ids.len(),
        integrity,
        orphan_ids,
    })
}

/// Back up `path` and truncate it to its longest valid JSON document.
///
/// Why: the manual repair the owner performed on 2026-08-06 — verify the
/// discarded bytes hold nothing new, copy the file aside, cut it at the end of
/// the valid document — is mechanical, and leaving it to a human at 09:00 with
/// a wedged daemon is how a store stays broken for hours.
/// What: reads the file ONCE, plans from those bytes, refuses when the tail
/// names an unknown session unless `force`, copies the bytes it read to
/// `<path>.corrupt-backup-<YYYYMMDD-HHMMSS>`, then writes back only their valid
/// prefix. Reading once is what makes the cut sound (#5027 review): the offset
/// and the bytes it indexes now come from the same read, so a file that changed
/// underneath cannot produce an out-of-range slice or a silently misplaced cut.
/// The backup is fsync-free but always precedes the truncating write, so a
/// crash mid-repair leaves either the original file or the original plus its
/// backup — never a truncated file with no backup.
/// Test: `repair_backs_up_then_truncates_to_the_valid_document`,
/// `repair_refuses_an_orphaned_tail_without_force`,
/// `repair_with_force_discards_an_orphaned_tail`.
pub fn repair(path: &Path, force: bool, now: DateTime<Utc>) -> Result<RepairOutcome, RepairError> {
    let raw = read(path)?;
    let plan = plan_bytes(path, &raw)?;
    if !plan.orphan_ids.is_empty() && !force {
        return Err(RepairError::OrphanedSessions {
            path: path.to_path_buf(),
            discarded: plan.integrity.trailing_bytes(),
            orphans: plan.orphan_ids,
        });
    }
    let backup = backup_path(path, now);
    std::fs::write(&backup, &raw).map_err(|source| RepairError::Io {
        path: backup.clone(),
        source,
    })?;
    // `valid_prefix_bytes` is `Some` here — `plan_bytes` returns `NoValidPrefix`
    // otherwise — and it was measured from THESE bytes, so the slice below
    // cannot be out of range.
    let kept = plan.integrity.valid_prefix_bytes.unwrap_or(raw.len());
    std::fs::write(path, &raw[..kept]).map_err(|source| RepairError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(RepairOutcome {
        backup,
        kept_bytes: kept,
        discarded_bytes: plan.integrity.trailing_bytes(),
        sessions: plan.recovered_sessions,
        forced_orphans: plan.orphan_ids,
    })
}

/// Where the pre-repair copy of a store file goes.
///
/// Why: the backup has to be adjacent (an operator looking at the broken store
/// finds it without being told where to look) and timestamped (a second repair
/// must not overwrite the evidence from the first). This reuses the exact name
/// shape of the 2026-08-06 manual backup.
/// What: `<path>.corrupt-backup-<YYYYMMDD-HHMMSS>` in UTC.
/// Test: `backup_path_is_a_timestamped_sibling`.
pub fn backup_path(path: &Path, now: DateTime<Utc>) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-backup-{}", now.format("%Y%m%d-%H%M%S")));
    path.with_file_name(name)
}

/// Session ids the recovered document actually contains.
///
/// Why: the orphan check needs the set of ids that survive truncation. Reading
/// them from the DESERIALIZED document (rather than scanning its text) means a
/// UUID that happens to appear inside a task description does not count as a
/// surviving session.
/// What: the map keys, lowercased to match [`uuid_tokens`].
/// Test: `kept_ids_come_from_the_map_keys_not_the_document_text`.
fn kept_ids(data: &StoredData) -> BTreeSet<String> {
    data.sessions
        .keys()
        .map(|k| k.to_ascii_lowercase())
        .collect()
}

/// Session ids named in the bytes a truncation would discard.
///
/// Why (#5027 review): scanning only `raw[valid..]` misses an id that STRADDLES
/// the cut — the scan sees its second half, which matches no UUID shape, so the
/// refusal does not fire and the repair discards a session the recovered
/// document never had. Starting the scan `UUID_LEN - 1` bytes earlier is enough
/// for any id spanning the boundary to be seen whole.
/// What: scans from just before the cut (rounded back to a char boundary) and
/// keeps the ids absent from `kept`. The extra 35 bytes can only ADD ids, and
/// an extra id can only make the repair refuse more often — the same
/// over-report-is-safe direction [`uuid_tokens`] already takes.
/// Test: `orphan_scan_sees_an_id_that_straddles_the_cut`.
fn orphaned_ids(raw: &str, valid: usize, kept: &BTreeSet<String>) -> Vec<String> {
    let mut from = valid.saturating_sub(UUID_LEN - 1);
    while from > 0 && !raw.is_char_boundary(from) {
        from -= 1;
    }
    uuid_tokens(&raw[from..])
        .into_iter()
        .filter(|id| !kept.contains(id))
        .collect()
}

/// A minimal but genuinely LOADABLE `sessions.json` body, for tests.
///
/// Why (#5027 review): fixtures like `{"sessions":{"a":{},"b":{}}}` are valid
/// JSON that [`SessionStore`](super::store::SessionStore) cannot load, so the
/// doctor test built on one asserted health for a file that blocks every write
/// — it proved the opposite of what it claimed. Every "healthy store" fixture
/// now comes from here, so a test cannot accidentally pick a shape only one
/// surface accepts.
/// What: `{"sessions":{…}}` carrying the six fields `SessionRecord` requires,
/// one record per `(id, task)` pair.
/// Test: used by `store_integrity_tests.rs` and `doctor_session_store_tests.rs`.
#[cfg(test)]
pub(crate) fn loadable_store_json(sessions: &[(&str, &str)]) -> String {
    let records: Vec<String> = sessions
        .iter()
        .map(|(id, task)| {
            let fields = r#""tmux_name":"tm-5007","cwd":"/tmp","state":"active""#;
            let created = r#""created_at":"2026-08-06T09:01:17Z""#;
            format!(r#""{id}":{{"id":"{id}","task":"{task}",{fields},{created}}}"#)
        })
        .collect();
    format!(r#"{{"sessions":{{{}}}}}"#, records.join(","))
}

#[cfg(test)]
#[path = "store_integrity_tests.rs"]
mod store_integrity_tests;

/// Every canonical UUID appearing anywhere in `s`.
///
/// Why: the discarded tail is a fragment, not valid JSON, so it cannot be
/// parsed — but the question asked of it ("does it mention a session the
/// recovered document lacks?") only needs its UUIDs. Scanning text
/// deliberately OVER-reports: a UUID inside a task string counts, which can
/// only make the repair refuse more often, never less.
/// What: lowercased 8-4-4-4-12 hex-and-hyphen windows, deduplicated.
/// Test: `uuid_tokens_finds_ids_in_a_json_fragment`,
/// `uuid_tokens_ignores_non_uuid_text`.
fn uuid_tokens(s: &str) -> BTreeSet<String> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = s.as_bytes();
    let mut out = BTreeSet::new();
    for start in 0..bytes.len().saturating_sub(UUID_LEN - 1) {
        let window = &bytes[start..start + UUID_LEN];
        let mut at = 0usize;
        let shaped = GROUPS.iter().enumerate().all(|(i, len)| {
            if i > 0 {
                if window[at] != b'-' {
                    return false;
                }
                at += 1;
            }
            let ok = window[at..at + len].iter().all(u8::is_ascii_hexdigit);
            at += len;
            ok
        });
        if shaped && s.is_char_boundary(start) && s.is_char_boundary(start + UUID_LEN) {
            out.insert(s[start..start + UUID_LEN].to_ascii_lowercase());
        }
    }
    out
}
