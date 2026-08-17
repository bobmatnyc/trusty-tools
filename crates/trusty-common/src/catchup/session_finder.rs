//! Unified paused-session discovery across trusty-mpm and claude-mpm formats.
//!
//! Why: during the migration period, a project may have paused sessions in both
//! the trusty-mpm native markdown format and the claude-mpm JSON format. This
//! module finds, merges, and renders sessions from both sources so the operator
//! gets a single, time-ordered catch-up digest.
//! What: [`find_paused_sessions`] returns a sorted list of [`PausedSession`];
//! [`render_resume_context`] renders a markdown digest from that list.
//! Test: `find_merges_both_formats`, `find_orders_newest_first`,
//! `render_contains_digest_not_conversation` in the inline test module.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::catchup::mpm_session::{ClaudeMpmSession, load_all_claude_mpm_sessions};

/// A discovered paused session in either the trusty-mpm or claude-mpm format.
///
/// Why: the two formats have different field sets; a tagged enum lets callers
/// dispatch cleanly while the shared rendering code handles both variants.
/// What: `TrustyMpm` wraps parsed data from a `.trusty-mpm/sessions/session-*.md`
/// file; `ClaudeMpm` wraps a [`ClaudeMpmSession`] loaded from the JSON format.
/// Test: `find_merges_both_formats` exercises both arms.
#[derive(Debug, Clone)]
pub enum PausedSession {
    /// A session paused by trusty-mpm (native markdown format).
    TrustyMpm {
        /// Path to the `.md` session file.
        path: PathBuf,
        /// Timestamp parsed from the `session-YYYYMMDD-HHMMSS.md` filename,
        /// falling back to the file's mtime when the filename carries no
        /// parseable stamp. `None` only when the filesystem cannot supply a
        /// modification time — and such a record is excluded from every
        /// watermark-filtered digest (see [`filter_sessions_since`], #5072).
        paused_at: Option<DateTime<Utc>>,
        /// Content of the `## Summary` section.
        summary: String,
        /// Content of the `## Git Context` section.
        git_context: Option<String>,
        /// Content of the `## In Progress` section.
        in_progress: Option<String>,
        /// Content of the `## Next Steps` section.
        next_steps: Option<String>,
        /// Why: lets resume re-align to the originating tmux window instead of
        /// leaving the operator on whatever window they happen to be on.
        /// What: the `session_name:window_index:window_id` string captured at
        /// pause time from `tmux display-message` and stored in the
        /// `## Tmux Window` section; `None` when the snapshot predates the
        /// feature or was created outside tmux.
        /// Test: `parse_extracts_tmux_window`, `parse_missing_tmux_window_is_none`.
        tmux_window: Option<String>,
    },
    /// A session paused by the claude-mpm Python tool (JSON format).
    ///
    // CUTOVER BRIDGE — remove post-migration (#1762)
    ClaudeMpm { session: ClaudeMpmSession },
}

impl PausedSession {
    /// Return a sortable pause timestamp, or `None` when the session cannot be
    /// dated at all.
    ///
    /// Why: needed to sort sessions newest-first regardless of format — and,
    /// since #5072, to decide watermark membership, where `None` means
    /// "excluded" rather than "always included". Both variants therefore get
    /// the same file-mtime fallback, so neither arm can go undatable merely
    /// because its recorded timestamp is missing or malformed.
    /// What: for `TrustyMpm` returns `paused_at`, which the parser already
    /// backfills from mtime. For `ClaudeMpm` parses the ISO-8601 `paused_at`
    /// string, falling back to
    /// [`ClaudeMpmSession::source_mtime`](crate::catchup::mpm_session::ClaudeMpmSession::source_mtime).
    /// `None` survives only for a session with no file behind it.
    /// Test: `find_orders_newest_first`,
    /// `claude_mpm_session_with_no_paused_at_is_dated_by_mtime`,
    /// `contract_sort_key_none_means_excluded_not_always_included`.
    ///
    /// # Code Contract
    /// Preconditions:
    /// - None. Every `PausedSession` value is accepted.
    ///
    /// Postconditions:
    /// - `None` means "this session cannot be dated at all", which since #5072
    ///   means EXCLUDED from a watermark-filtered digest. It does not mean
    ///   "always included" — that was the reading `is_none_or` encoded, and it
    ///   inverted a real project's digest.
    /// - Both variants receive the same file-mtime fallback, so neither arm can
    ///   be undatable merely because its recorded timestamp is missing or
    ///   malformed. `None` survives only for a session with no file behind it.
    ///
    /// Invariants:
    /// - Pure with respect to this value: no field is mutated and no file is
    ///   written. For a fixed filesystem state the result is stable across calls.
    /// - The ordering it induces is the ONE ordering used for both sorting and
    ///   watermark membership; the two never diverge.
    pub fn sort_key(&self) -> Option<DateTime<Utc>> {
        match self {
            PausedSession::TrustyMpm { paused_at, .. } => *paused_at,
            PausedSession::ClaudeMpm { session } => session
                .paused_at
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                // #5072: same mtime rescue the TrustyMpm arm gets, so
                // fail-closed filtering applies symmetrically to both.
                .or(session.source_mtime),
        }
    }
}

/// Find all paused sessions for `project_dir`, merging both formats newest-first.
///
/// Why: during cutover a project may hold both a trusty-mpm `.md` session and
/// older claude-mpm `.json` sessions; a unified view lets the operator decide
/// which context to resume from.
/// What: scans `<project_dir>/.trusty-mpm/sessions/session-*.md` for native
/// sessions and `<project_dir>/.claude-mpm/sessions/session-*.json` for claude-mpm
/// sessions, merges them, and sorts newest-first by pause timestamp (unknown
/// timestamps sort last). Returns an empty vec — not an error — when no sessions
/// exist in either format.
/// Test: `find_merges_both_formats`, `find_orders_newest_first`.
pub fn find_paused_sessions(project_dir: &Path) -> anyhow::Result<Vec<PausedSession>> {
    let mut sessions = Vec::new();

    // ── trusty-mpm native format (.md) ───────────────────────────────────────
    // #5272: snapshots now live under `sessions/<session-id>/`, so scan the
    // store root AND one level of per-session directories.
    let tm_sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    for dir in session_scan_dirs(&tm_sessions_dir) {
        collect_trusty_mpm_snapshots(&dir, &mut sessions);
    }

    // ── claude-mpm JSON format ────────────────────────────────────────────────
    // CUTOVER BRIDGE — remove post-migration (#1762)
    let claude_sessions = load_all_claude_mpm_sessions(project_dir).unwrap_or_default();
    for s in claude_sessions {
        sessions.push(PausedSession::ClaudeMpm { session: s });
    }

    // Sort newest-first; sessions with no parseable timestamp go last.
    sessions.sort_by(|a, b| match (a.sort_key(), b.sort_key()) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(sessions)
}

/// The directories a native-snapshot scan must visit: the store root plus each
/// per-session subdirectory.
///
/// Why: #5272 moved pause snapshots into `sessions/<session-id>/`. A scan that
/// only reads the store root would show pre-#5272 flat files and NOTHING
/// written since — an empty catch-up digest on a project full of snapshots.
/// Depth is capped at one level because that is the whole layout; recursing
/// further would only invite unrelated `.md` files in.
/// What: `[root]` when it exists, followed by its immediate subdirectories in
/// directory order. Empty when `root` is absent.
/// Test: `find_includes_per_session_directories`.
fn session_scan_dirs(root: &Path) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(root) {
        dirs.extend(rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()));
    }
    dirs
}

/// Parse every `session-*.md` directly inside `dir` into `out`.
///
/// Why: the per-directory half of [`session_scan_dirs`], factored out so the
/// root and each per-session directory go through identical parsing.
/// What: appends one [`PausedSession::TrustyMpm`] per readable, parseable
/// `session-*.md`; unreadable files are skipped, matching the pre-#5272
/// fail-open scan.
/// Test: `find_includes_per_session_directories`, `find_merges_both_formats`.
fn collect_trusty_mpm_snapshots(dir: &Path, out: &mut Vec<PausedSession>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().into_string().unwrap_or_default();
        if name.starts_with("session-")
            && name.ends_with(".md")
            && let Ok(s) = parse_trusty_mpm_session(&entry.path())
        {
            out.push(s);
        }
    }
}

/// The outcome of a watermark filter: what survived, and what was withheld.
///
/// Why: the count is not diagnostics, it is a receipt. The watermark advances
/// after a catch-up whether or not a session was withheld, so a withheld
/// session falls outside every future window permanently — the caller must be
/// able to tell "nothing paused since your last catch-up" apart from "N
/// sessions existed and could not be dated" and re-run with `full`. A stderr
/// warning is not a receipt when the consumer is an MCP client reading a JSON
/// body (#5072).
/// What: `kept` is the filtered list; `dropped_undatable` counts sessions
/// excluded solely because [`PausedSession::sort_key`] returned `None`. It is
/// always 0 when there is no watermark.
/// Test: `filter_sessions_since_reports_dropped_count`,
/// `generate_catchup_json_reports_undatable_drop_count`.
#[derive(Debug, Default)]
pub struct FilteredSessions {
    /// Sessions that provably postdate the watermark.
    pub kept: Vec<PausedSession>,
    /// How many sessions were withheld because they could not be dated.
    pub dropped_undatable: usize,
}

/// Retain only the sessions that provably paused after `watermark`.
///
/// Why: this predicate previously lived, copy-pasted, in both
/// [`crate::catchup::generate_catchup_context`] and
/// [`crate::catchup::generate_catchup_json`] as
/// `s.sort_key().is_none_or(|ts| ts > wm)`. `is_none_or` yields `true` for an
/// unknown key, so a session whose pause instant could not be derived was
/// admitted by EVERY watermark, forever, while every session that could be
/// dated was correctly filtered out. On a real project that inverted the
/// digest: the one undatable record survived and dozens of newer well-formed
/// snapshots did not (#5072). "Sessions since T" is not a claim an undatable
/// record can satisfy, so it is now excluded — but the exclusion is reported in
/// the return value, not merely logged, because the watermark advances past a
/// withheld session and never comes back for it.
/// What: with no watermark (a `full` catch-up) returns every session and a zero
/// count. With a watermark, keeps only sessions whose
/// [`PausedSession::sort_key`] is `Some` and strictly greater, counting the
/// undatable exclusions and warning on stderr for each.
/// Test: `filter_sessions_since_drops_undatable_session`,
/// `filter_sessions_since_keeps_everything_without_watermark`,
/// `filter_sessions_since_reports_dropped_count`,
/// `generate_catchup_json_excludes_undatable_session_behind_watermark`,
/// `contract_filter_sessions_since_partitions_the_input`.
///
/// # Code Contract
/// Preconditions:
/// - None. Every `sessions` list and every `watermark` is accepted; an empty
///   list and a `None` watermark are ordinary inputs, not edge cases.
///
/// Postconditions:
/// - With `watermark == None`, `kept` is `sessions` unchanged in length and
///   order, and `dropped_undatable == 0`.
/// - With `watermark == Some(wm)`, `kept` contains exactly those sessions whose
///   `sort_key()` is `Some(ts)` with `ts > wm`. The comparison is STRICT, so a
///   session paused exactly at the watermark is excluded.
/// - `dropped_undatable` counts exactly the sessions whose `sort_key()` is
///   `None`. Since #5072 an undatable session is WITHHELD, not admitted — the
///   inverse of the `is_none_or` predicate this replaced.
/// - `kept` preserves the relative order of the input.
///
/// Invariants:
/// - `kept.len() + dropped_undatable <= sessions.len()`, with the shortfall
///   being sessions that were datable and did not postdate the watermark.
/// - No session is both kept and counted as dropped.
/// - The count is a RECEIPT, not diagnostics: a withheld session falls outside
///   every future window once the watermark advances, so it must be reportable
///   in the return value and not only on stderr.
pub fn filter_sessions_since(
    sessions: Vec<PausedSession>,
    watermark: Option<DateTime<Utc>>,
) -> FilteredSessions {
    let Some(wm) = watermark else {
        return FilteredSessions {
            kept: sessions,
            dropped_undatable: 0,
        };
    };
    let mut out = FilteredSessions::default();
    for s in sessions {
        // #5072: fail-closed — an undatable session cannot be shown to postdate
        // the watermark, so it is withheld instead of admitted unconditionally.
        match s.sort_key() {
            Some(ts) if ts > wm => out.kept.push(s),
            Some(_) => {}
            None => {
                out.dropped_undatable += 1;
                eprintln!(
                    "catchup: warning: withholding a paused session with no derivable \
                     pause timestamp from the since-{wm} digest; re-run with \
                     full=true to see it"
                );
            }
        }
    }
    out
}

/// Read a file's modification time as a UTC instant.
///
/// Why: the fallback pause instant for both [`PausedSession`] variants when the
/// recorded one is missing or unparseable (#5072). It is a fallback, not an
/// equal: mtime tracks the last WRITE, not the pause, so editing an old
/// hand-written snapshot re-dates it to the edit and promotes it to the head of
/// the digest. Acceptable because the alternative is an undatable record, which
/// [`filter_sessions_since`] drops entirely — a stale-but-present ordering beats
/// an absent session. It does not fire on a fresh clone: `.trusty-mpm/` and
/// `.claude-mpm/` are gitignored, so no snapshot carries a checkout mtime.
/// What: `metadata().modified()`, converted to `DateTime<Utc>`; `None` when the
/// platform or filesystem cannot supply one.
/// Test: `parse_falls_back_to_mtime_when_filename_lacks_timestamp`,
/// `claude_mpm_session_with_no_paused_at_is_dated_by_mtime`.
pub(crate) fn mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

/// Resolve the native `.trusty-mpm` snapshot a given session may resume from.
///
/// Why: #5272 — several PM sessions now share one `.trusty-mpm/sessions/` store
/// (the PM runs on the project's main checkout), and the old chain answered
/// "newest pause overall" whenever the asking session had no snapshot of its
/// own. Session `7bd5c27a…` asked and was handed `session-20260809-010155.md`,
/// which the log attributes to `2eb72dca…`, with nothing in the response saying
/// so. Under #2731's one-session-per-checkout model that fallback was right;
/// with a shared store it turns "no snapshot for me" into "someone else's".
/// What: `session_id` is now the ATTRIBUTION, not a hint. `Some(id)` resolves
/// through [`session_log::resolve_session_snapshot`](crate::catchup::session_log::resolve_session_snapshot),
/// which never crosses session boundaries — an id may name another session, and
/// that explicit request still succeeds, which is the only way a cross-session
/// read happens. `None` means the caller did not identify itself, so no
/// snapshot is attributable to it and the answer is `None` rather than an
/// arbitrary session's file.
/// Test: `latest_snapshot_prefers_session_log`,
/// `latest_snapshot_refuses_cross_session_fallback`,
/// `latest_snapshot_requires_a_session_id`,
/// `latest_snapshot_reads_legacy_flat_snapshot_via_log`,
/// `contract_latest_snapshot_none_session_id_is_none`.
///
/// # Code Contract
/// Preconditions:
/// - `session_id` is the ATTRIBUTION of the asking session, not a search hint.
///   `None` asserts "the caller did not identify itself". It does NOT mean
///   "any session will do" — that was the pre-#5272 reading, and it is the
///   precondition that changed under a byte-identical signature.
/// - `project_dir` need not exist; a missing directory yields `None`, not an
///   error.
///
/// Postconditions:
/// - Returns `None` whenever `session_id` is `None`, for every `project_dir`,
///   including one holding snapshots that would have matched before #5272.
/// - A returned path is always a snapshot the session log attributes to
///   `session_id`; it is never selected by recency across session boundaries.
/// - A returned path is `<project_dir>/.trusty-mpm/sessions/` rooted.
///
/// Invariants:
/// - Read-only: no file, directory, or session-log entry is created, modified,
///   or removed.
/// - Referentially transparent for a fixed filesystem state — repeated calls
///   with equal arguments return equal results.
pub fn latest_trusty_mpm_snapshot(project_dir: &Path, session_id: Option<&str>) -> Option<PathBuf> {
    // #5272: no session id → nothing is attributable to this caller → empty.
    let id = session_id?;
    let sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    crate::catchup::session_log::resolve_session_snapshot(&sessions_dir, id, "md")
}

/// Render a markdown catch-up digest for a slice of paused sessions.
///
/// Why: `tm session catchup` prints this digest to stdout so the operator (or
/// the PM skill) can inject it as conversation context for the current Claude
/// session.
/// What: produces one block per session containing `resume_instructions`,
/// `important_reminders`, `open_questions`, `todos`/`task_list`, `git_context`,
/// `paused_at`, and `context_usage`. Raw `conversation` content is never included.
/// Returns an empty string when `sessions` is empty.
/// Test: `render_contains_digest_not_conversation`.
pub fn render_resume_context(sessions: &[PausedSession]) -> String {
    if sessions.is_empty() {
        return String::from("No paused sessions found.\n");
    }

    let mut out = String::from("# Paused Session Catch-Up\n\n");
    for (i, session) in sessions.iter().enumerate() {
        out.push_str(&format!("## Session {} of {}\n\n", i + 1, sessions.len()));
        render_session(&mut out, session);
        out.push('\n');
    }
    out
}

fn render_session(out: &mut String, session: &PausedSession) {
    match session {
        PausedSession::TrustyMpm {
            path,
            paused_at,
            summary,
            git_context,
            in_progress,
            next_steps,
            tmux_window,
        } => {
            out.push_str("**Format:** trusty-mpm (native)\n");
            if let Some(ts) = paused_at {
                out.push_str(&format!("**Paused At:** {ts}\n"));
            }
            if let Some(win) = tmux_window
                && !win.is_empty()
            {
                out.push_str(&format!("**Tmux Window:** {win}\n"));
            }
            out.push_str(&format!("**File:** {}\n\n", path.display()));
            if !summary.is_empty() {
                out.push_str(&format!("### Summary\n{summary}\n\n"));
            }
            if let Some(ctx) = in_progress
                && !ctx.is_empty()
            {
                out.push_str(&format!("### In Progress\n{ctx}\n\n"));
            }
            if let Some(steps) = next_steps
                && !steps.is_empty()
            {
                out.push_str(&format!("### Next Steps\n{steps}\n\n"));
            }
            if let Some(git) = git_context
                && !git.is_empty()
            {
                out.push_str(&format!("### Git Context\n{git}\n\n"));
            }
        }
        PausedSession::ClaudeMpm { session: s } => {
            // CUTOVER BRIDGE — remove post-migration (#1762)
            out.push_str("**Format:** claude-mpm (legacy)\n");
            if let Some(pa) = &s.paused_at {
                out.push_str(&format!("**Paused At:** {pa}\n"));
            }
            if let Some(cu) = s.context_usage {
                out.push_str(&format!("**Context Usage:** {:.0}%\n", cu * 100.0));
            }
            if let Some(dh) = s.duration_hours {
                out.push_str(&format!("**Duration:** {dh:.1}h\n"));
            }
            out.push('\n');
            if let Some(ri) = &s.resume_instructions
                && !ri.is_empty()
            {
                out.push_str(&format!("### Resume Instructions\n{ri}\n\n"));
            }
            if let Some(reminders) = &s.important_reminders
                && !reminders.is_empty()
            {
                out.push_str("### Important Reminders\n");
                for r in reminders {
                    out.push_str(&format!("- {r}\n"));
                }
                out.push('\n');
            }
            if let Some(oq) = &s.open_questions
                && !oq.is_empty()
            {
                out.push_str("### Open Questions\n");
                for q in oq {
                    out.push_str(&format!("- {q}\n"));
                }
                out.push('\n');
            }
            // Render todos + task_list together.
            let tasks: Vec<&String> = s
                .todos
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .chain(s.task_list.as_deref().unwrap_or(&[]).iter())
                .collect();
            if !tasks.is_empty() {
                out.push_str("### Tasks\n");
                for t in tasks {
                    out.push_str(&format!("- {t}\n"));
                }
                out.push('\n');
            }
            if let Some(git) = &s.git_context
                && !git.is_empty()
            {
                out.push_str(&format!("### Git Context\n{git}\n\n"));
            }
        }
    }
}

/// Parse a trusty-mpm native session markdown file.
///
/// Why: isolates the markdown parsing so it can be tested independently.
/// What: reads the file, extracts sections by `## <Header>` delimiters, and
/// attempts to parse a timestamp from the filename (`session-YYYYMMDD-HHMMSS.md`).
/// Test: `parse_trusty_mpm_session_extracts_sections`.
fn parse_trusty_mpm_session(path: &Path) -> anyhow::Result<PausedSession> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;

    let summary = extract_section(&content, "Summary").unwrap_or_default();
    let git_context = extract_section(&content, "Git Context");
    let in_progress = extract_section(&content, "In Progress");
    let next_steps = extract_section(&content, "Next Steps");
    // `None` for snapshots without the section keeps older files back-compat.
    let tmux_window = extract_section(&content, "Tmux Window");

    // Try to parse a UTC timestamp from the filename: session-YYYYMMDD-HHMMSS.md
    // #5072: hand-written snapshots (`session-20260730-bounce.md`) carry no
    // parseable stamp; fall back to the file's mtime so the record is still
    // datable — an undatable one is dropped by `filter_sessions_since`.
    let paused_at = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("session-"))
        .and_then(parse_filename_timestamp)
        .or_else(|| mtime_utc(path));

    Ok(PausedSession::TrustyMpm {
        path: path.to_owned(),
        paused_at,
        summary,
        git_context,
        in_progress,
        next_steps,
        tmux_window,
    })
}

/// Extract the content of a `## <header>` section from markdown text.
///
/// Why: the trusty-mpm session format uses level-2 headers as section delimiters.
/// What: returns the trimmed content between the end of the `## <header>`
/// LINE and the next `## ` header or end-of-file. Returns `None` when the
/// section is absent or empty.
///
/// Trailing text on the header line (e.g. a hand-written `## Next Steps
/// (all Bob's call — none required)`) is skipped along with the rest of that
/// line rather than being rejected outright. An earlier version of this
/// function rejected any header with trailing text and returned `None` for
/// the whole section — that is a WORSE bug than the one it was meant to fix:
/// annotated headers (`## In Progress (BACKGROUND AGENTS DIE AT PAUSE — …)`,
/// `## Next Steps (RESUME PLAYBOOK)`, `## Completed (this leg)`) were this
/// project's own standard hand-written convention for weeks before
/// `pause.rs`'s native writer existed, not an isolated one-off. Simulating
/// the fail-closed version against this project's own 50-file
/// `.trusty-mpm/sessions/*.md` archive (07-06 through 07-24) regressed 64 of
/// 300 section extractions from present content to `None` — and
/// `render_session` silently *omits* a `None` section with no warning, so a
/// resume digest would misreport "nothing in progress" for sessions that
/// recorded substantial unfinished work. Skipping just the header's own
/// line — regardless of its trailer — fixes the original leak (the trailing
/// annotation text no longer ends up prepended to the body) without losing
/// content from any annotated historical header.
/// Test: `extract_section_finds_content`,
/// `extract_section_strips_trailing_header_annotation_from_body`,
/// `extract_section_allows_trailing_whitespace_on_header_line`,
/// `extract_section_first_occurrence_wins_when_header_repeated`,
/// `extract_section_handles_representative_corpus_header_styles`.
fn extract_section(text: &str, header: &str) -> Option<String> {
    let needle = format!("## {header}");
    let start = text.find(&needle)?;
    let after_needle = start + needle.len();
    let rest = &text[after_needle..];
    // Skip past the rest of the header LINE (through its own newline, if
    // any) so trailing annotation text never leaks into the captured body.
    let line_end = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
    let body = &rest[line_end..];
    let end = body.find("\n## ").unwrap_or(body.len());
    let section = body[..end].trim().to_owned();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Parse a timestamp from the `YYYYMMDD-HHMMSS` portion of a session filename.
///
/// Why: lets us sort native sessions by pause time even when no timestamp header
/// exists in the file body. Session filenames are not always machine-generated
/// (#5072 explicitly accommodates hand-written snapshots like
/// `session-20260730-bounce.md`), so this must reject malformed input rather
/// than assume a fixed byte layout.
/// What: expects input like `20260627-142030`; parses it as UTC. The `len()`
/// guards below check BYTE length, not char count, so a stem containing a
/// multi-byte UTF-8 character can satisfy them while still landing a
/// fixed-byte-offset slice mid-character — the `is_ascii_digit` check that
/// follows rules that out before any slicing happens (#5294: `stem =
/// "123é456-142030"` is 15 bytes and its parts are 8/6 bytes, but slicing
/// `&date_part[0..4]` used to panic since byte offset 4 falls inside 'é').
/// Once every byte is confirmed an ASCII digit, byte offsets and char offsets
/// coincide and the slices are safe.
/// Test: `parse_filename_timestamp_roundtrip`,
/// `parse_filename_timestamp_rejects_non_ascii_without_panicking`.
fn parse_filename_timestamp(stem: &str) -> Option<DateTime<Utc>> {
    // stem is like "20260627-142030"
    if stem.len() != 15 {
        return None;
    }
    let (date_part, time_part) = stem.split_once('-')?;
    if date_part.len() != 8 || time_part.len() != 6 {
        return None;
    }
    // #5294: guard ASCII-digit-only before any byte-offset slicing.
    if !date_part.bytes().all(|b| b.is_ascii_digit())
        || !time_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let s = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &date_part[0..4],
        &date_part[4..6],
        &date_part[6..8],
        &time_part[0..2],
        &time_part[2..4],
        &time_part[4..6],
    );
    s.parse::<DateTime<Utc>>().ok()
}

#[cfg(test)]
#[path = "session_finder_tests.rs"]
mod tests;
