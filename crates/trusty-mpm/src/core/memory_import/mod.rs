//! Inference-free bulk import of memory `.md` files into a trusty-memory palace.
//!
//! Why (issue #4837, option 1): migrating a directory of memory files into a
//! palace is ETL — read file, map frontmatter onto drawer fields, write. Routing
//! it through an agent cost 622k tokens for 120 files (~5.5k tokens per file
//! copy) because every tool round re-sends the agent's accumulated context.
//! This module is the deterministic replacement, and the prerequisite for
//! issue #4834 (whose whole point is that migration).
//! What: [`run_import`] walks a directory of `*.md` memory files, derives each
//! one's drawer `text`/`tags` via [`parse`], skips any whose slug is already
//! stored, writes the rest through trusty-memory's JSON-RPC surface, and
//! returns a machine-readable [`ImportReport`].
//!
//! Idempotency, without `memory_recall`: the recall path is broken (issue
//! #4836 — it returns the same five drawers at score 1.0 for every query), so
//! dedup here is an exact **tag lookup plus a structural filter**, not a
//! similarity search. Every derived drawer carries its file's frontmatter
//! `name` (the kebab slug, identical to the filename stem and unique per file)
//! as a tag, and trusty-memory's `memory_list` filters tags by exact string
//! equality server-side — so `memory_list { tag: <slug> }` returns a small,
//! deterministic candidate set with no embedding, ranking, or scoring
//! involved.
//!
//! The tag alone is not sufficient, because a *different* file that links to
//! `[[<slug>]]` also carries that tag. The candidates are separated by
//! [`links_to`] rather than by comparing prose: a foreign slug enters a tag set
//! **only** via a `[[wikilink]]` in that file's body, and the body is stored
//! verbatim — so re-deriving the stored text's wikilink targets with the very
//! function that produced the tags reproduces exactly why each referrer carries
//! it, and removes precisely those drawers. What remains is the file's own
//! drawer, identified without reference to its text, so a description that has
//! since drifted still reads as *present* instead of absent (issue #4837
//! review: the earlier headline-only check made drift look like a missing
//! drawer and wrote a second one). Nothing here can write a duplicate: when the
//! remainder is ambiguous, or the candidate set hit its ceiling, the file is
//! reported as failed rather than guessed at.
//!
//! Test: `core::memory_import::tests`.

pub mod parse;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Serialize;
use serde_json::{Value, json};

pub use parse::{ParsedMemory, parse_memory_file};

/// Everything [`run_import`] needs to run one import.
///
/// Why: the flag set is wide enough that a positional-argument signature would
/// be unreadable at the call site, and threading a struct keeps the CLI layer
/// a pure translation of clap args.
/// What: source directory, target palace, and the three behaviour switches.
/// Test: `dry_run_writes_nothing`, `import_is_idempotent`.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Directory of memory `.md` files. Scanned non-recursively.
    pub dir: PathBuf,
    /// Target palace slug (e.g. `trusty-tools`).
    pub palace: String,
    /// Parse, derive, and dedup-check, but never write.
    pub dry_run: bool,
    /// Pass `allow_secret_like` on writes, so drawers whose prose trips
    /// trusty-memory's secret heuristic (a URL, a token-shaped identifier)
    /// still store instead of aborting the file.
    pub allow_secret_like: bool,
    /// Explicit trusty-memory base URL. `None` uses daemon discovery.
    pub memory_url: Option<String>,
}

/// Per-file outcome of an import.
///
/// Why: a caller must be able to verify the run without a second pass, which
/// means every file's fate — and the drawer id it landed in — has to be in the
/// report itself.
/// What: serialises to the lowercase snake_case strings a script matches on.
/// Test: `dry_run_writes_nothing`, `import_is_idempotent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Written; `drawer_id` carries the new drawer.
    Created,
    /// Already present (its own drawer was found) or not a memory file at all;
    /// `drawer_id` carries the existing drawer when one was found, and `detail`
    /// says whether that drawer's text still matches the file.
    Skipped,
    /// Dry run: would have been written.
    WouldCreate,
    /// Dry run: would have been skipped.
    WouldSkip,
    /// Parse or write error; `error` carries the cause.
    Failed,
}

/// One row of the machine-readable report.
#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    /// File name relative to the scanned directory.
    pub file: String,
    /// Frontmatter `name` slug, when the file parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What happened to this file.
    pub status: ImportStatus,
    /// The drawer this file corresponds to, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawer_id: Option<String>,
    /// Derived tag set, when the file parsed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Failure cause or skip reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The full result of an import run.
///
/// Why: printed verbatim under `--json` so a caller can verify the migration
/// without re-reading the palace.
/// What: run-level context, per-status counts, and one [`FileResult`] per file.
/// Test: `dry_run_writes_nothing`, `import_is_idempotent`.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    /// Target palace slug.
    pub palace: String,
    /// Scanned directory, as given.
    pub dir: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Number of `*.md` files considered.
    pub total: usize,
    /// Files written (or, in a dry run, that would be written).
    pub created: usize,
    /// Files already present or not memory files.
    pub skipped: usize,
    /// Files that failed to parse or write.
    pub failed: usize,
    /// Per-file detail, in filename order.
    pub files: Vec<FileResult>,
}

impl ImportReport {
    /// Recount the summary fields from `files`.
    fn tally(&mut self) {
        self.total = self.files.len();
        self.created = self.count(&[ImportStatus::Created, ImportStatus::WouldCreate]);
        self.skipped = self.count(&[ImportStatus::Skipped, ImportStatus::WouldSkip]);
        self.failed = self.count(&[ImportStatus::Failed]);
    }

    fn count(&self, wanted: &[ImportStatus]) -> usize {
        self.files
            .iter()
            .filter(|f| wanted.contains(&f.status))
            .count()
    }
}

/// Import every memory file in `opts.dir` into `opts.palace`.
///
/// Why: the deterministic, zero-inference path issue #4837 calls for. Failures
/// are per-file and never abort the run, so one unparseable file cannot strand
/// a 148-file migration halfway through the way the agent-driven attempt did.
/// What: resolves the trusty-memory base URL (explicit override, else daemon
/// discovery), lists `*.md` files in filename order, and for each one derives
/// the drawer fields, checks for an existing drawer with the same slug tag,
/// and — unless `dry_run` — writes it via `memory_remember`. Returns the
/// [`ImportReport`]; a non-empty `failed` count is the caller's cue to exit
/// non-zero.
/// Test: `dry_run_writes_nothing`, `import_is_idempotent`,
/// `non_memory_files_are_skipped`, `unparseable_file_is_reported_not_fatal`.
pub async fn run_import(opts: &ImportOptions) -> anyhow::Result<ImportReport> {
    let base_url = match &opts.memory_url {
        Some(url) => url.clone(),
        None => trusty_common::mcp::memory_rpc::resolve_memory_base_url()
            .context("resolve trusty-memory daemon address")?,
    };

    let mut report = ImportReport {
        palace: opts.palace.clone(),
        dir: opts.dir.display().to_string(),
        dry_run: opts.dry_run,
        total: 0,
        created: 0,
        skipped: 0,
        failed: 0,
        files: Vec::new(),
    };

    for path in markdown_files(&opts.dir)? {
        report.files.push(import_one(&base_url, opts, &path).await);
    }
    report.tally();
    Ok(report)
}

/// List the `*.md` files directly inside `dir`, in filename order.
///
/// Why: deterministic ordering makes a partial run resumable and two runs
/// comparable. Non-recursive because the memory format is a flat directory of
/// facts plus an index file.
/// What: reads the directory, keeps regular files with a `.md` extension
/// (case-insensitive), and sorts by path.
/// Test: `non_memory_files_are_skipped`.
fn markdown_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read memory directory {}", dir.display()))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_md = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        if is_md {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Derive, dedup-check, and (unless dry-running) write one file.
///
/// Why: keeping the whole per-file decision in one function means every exit
/// path produces exactly one [`FileResult`], so the report can never
/// under-count.
/// What: reads and parses the file; a file with no frontmatter is a clean skip
/// (`MEMORY.md` and other index files land here). Otherwise asks
/// [`existing_drawer`] whether the palace already holds this file's own drawer
/// and skips when it does — whether or not that drawer's text still matches;
/// otherwise writes via `memory_remember` with `force` set (the established
/// mapping — these drawers are deliberate re-writes of curated content, not
/// conversational capture). Note that `force` bypasses trusty-memory's own
/// dedup along with the rest of its quality gates, so the check above is the
/// only thing standing between a re-run and a duplicate.
/// Test: `import_is_idempotent`, `drifted_description_is_not_reimported`,
/// `non_memory_files_are_skipped_and_non_markdown_ignored`.
async fn import_one(base_url: &str, opts: &ImportOptions, path: &Path) -> FileResult {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return failure(file, None, format!("read failed: {e}"), Vec::new()),
    };
    let parsed = match parse::parse_memory_file(&source) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return FileResult {
                file,
                name: None,
                status: skip_status(opts.dry_run),
                drawer_id: None,
                tags: Vec::new(),
                detail: Some("not a memory file (no YAML frontmatter)".to_string()),
            };
        }
        Err(e) => return failure(file, None, format!("parse failed: {e:#}"), Vec::new()),
    };

    match existing_drawer(base_url, &opts.palace, &parsed).await {
        Ok(Existing::Same(drawer_id)) => {
            return skipped(file, parsed, drawer_id, "already imported", opts.dry_run);
        }
        Ok(Existing::Drifted(drawer_id)) => {
            return skipped(
                file,
                parsed,
                drawer_id,
                "already imported, but the stored drawer's text has drifted from \
                 this file — left unchanged, never duplicated",
                opts.dry_run,
            );
        }
        Ok(Existing::Absent) => {}
        Err(e) => {
            return failure(
                file,
                Some(parsed.name),
                format!("dedup check failed: {e:#}"),
                parsed.tags,
            );
        }
    }

    if opts.dry_run {
        return FileResult {
            file,
            name: Some(parsed.name),
            status: ImportStatus::WouldCreate,
            drawer_id: None,
            tags: parsed.tags,
            detail: None,
        };
    }

    match write_drawer(base_url, opts, &parsed).await {
        Ok(drawer_id) => FileResult {
            file,
            name: Some(parsed.name),
            status: ImportStatus::Created,
            drawer_id: Some(drawer_id),
            tags: parsed.tags,
            detail: None,
        },
        Err(e) => failure(
            file,
            Some(parsed.name),
            format!("write failed: {e:#}"),
            parsed.tags,
        ),
    }
}

/// Build a skip row for a file whose drawer is already in the palace.
fn skipped(
    file: String,
    parsed: ParsedMemory,
    drawer_id: String,
    detail: &str,
    dry_run: bool,
) -> FileResult {
    FileResult {
        file,
        name: Some(parsed.name),
        status: skip_status(dry_run),
        drawer_id: Some(drawer_id),
        tags: parsed.tags,
        detail: Some(detail.to_string()),
    }
}

/// Build a `Failed` row.
fn failure(file: String, name: Option<String>, detail: String, tags: Vec<String>) -> FileResult {
    FileResult {
        file,
        name,
        status: ImportStatus::Failed,
        drawer_id: None,
        tags,
        detail: Some(detail),
    }
}

/// The skip status appropriate to the run mode.
fn skip_status(dry_run: bool) -> ImportStatus {
    if dry_run {
        ImportStatus::WouldSkip
    } else {
        ImportStatus::Skipped
    }
}

/// How many slug-tagged candidates to inspect before giving up.
///
/// Why: the candidate set is "the file itself, plus every file that links to
/// it" — tens at most in practice. A bounded fetch keeps the check O(1) in
/// palace size, and `memory_list` offers no cursor to page with, so hitting
/// this ceiling is treated as "cannot prove absence" rather than "absent".
const DEDUP_CANDIDATE_LIMIT: usize = 200;

/// What the palace already holds for one file.
#[derive(Debug, PartialEq, Eq)]
enum Existing {
    /// The file's own drawer, storing the headline this file derives.
    Same(String),
    /// The file's own drawer, but its stored text has drifted from the file.
    Drifted(String),
    /// Nothing in the palace corresponds to this file.
    Absent,
}

/// Look up an already-imported drawer for this file.
///
/// Why: this is the idempotency check, and deliberately NOT `memory_recall` —
/// see the module doc and issue #4836. It must answer "is this file's drawer
/// present?" without depending on the drawer's prose, because prose drifts and
/// a drifted drawer read as absent is a duplicate.
/// What: calls `memory_list { palace, tag, limit }`, drops every candidate that
/// carries the tag only because it links to the slug ([`links_to`]), and
/// classifies what is left. A full result page means the tag is shared by more
/// drawers than can be inspected, and more than one surviving candidate means
/// the palace is in a shape this cannot resolve — both are errors, so the
/// caller reports the file rather than writing a possible duplicate.
/// Test: `import_is_idempotent`, `linking_drawer_does_not_block_its_target`,
/// `drifted_description_is_not_reimported`, `truncated_candidate_set_fails_closed`,
/// `ambiguous_candidates_fail_closed`.
async fn existing_drawer(
    base_url: &str,
    palace: &str,
    parsed: &ParsedMemory,
) -> anyhow::Result<Existing> {
    let result = trusty_common::mcp::memory_rpc::call_memory_tool_at(
        base_url,
        "memory_list",
        json!({ "palace": palace, "tag": parsed.name, "limit": DEDUP_CANDIDATE_LIMIT }),
    )
    .await?;
    let drawers = result
        .get("drawers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if drawers.len() >= DEDUP_CANDIDATE_LIMIT {
        anyhow::bail!(
            "the tag `{}` fills the whole {DEDUP_CANDIDATE_LIMIT}-drawer candidate page, \
             so the candidate set is truncated and absence cannot be proven",
            parsed.name
        );
    }

    // A file that links to its own slug would be filtered out as a referrer of
    // itself, so for that (rare) shape every candidate stays in the running.
    let self_linking = links_to(&parsed.text, &parsed.name);
    let own: Vec<(&str, &str)> = drawers
        .iter()
        .filter_map(|d| {
            let id = d.get("drawer_id").and_then(Value::as_str)?;
            let content = d.get("content").and_then(Value::as_str).unwrap_or_default();
            (self_linking || !links_to(content, &parsed.name)).then_some((id, content))
        })
        .collect();

    match own.as_slice() {
        [] => Ok(Existing::Absent),
        [(id, content)] => Ok(if has_headline_of(content, &parsed.text) {
            Existing::Same((*id).to_string())
        } else {
            Existing::Drifted((*id).to_string())
        }),
        many => match many.iter().find(|(_, c)| has_headline_of(c, &parsed.text)) {
            Some((id, _)) => Ok(Existing::Same((*id).to_string())),
            None => anyhow::bail!(
                "{} drawers tagged `{}` could each be this file's own drawer and none \
                 carries its headline — refusing to guess",
                many.len(),
                parsed.name
            ),
        },
    }
}

/// Whether `text` carries `slug` because it links to it.
///
/// Why: this is what separates a referrer from the file's own drawer, and it is
/// exact rather than heuristic — a foreign slug reaches a drawer's tag set only
/// through [`parse::wikilink_targets`], so running that same derivation over the
/// stored text recovers precisely the set of slugs the drawer was tagged with
/// for linking. Alias, anchor, and `.md` forms all normalise identically here
/// and there.
/// What: true when re-deriving `text`'s wikilink targets yields `slug`.
/// Test: `linking_drawer_does_not_block_its_target`,
/// `drift_behind_a_referrer_is_not_reimported`.
fn links_to(text: &str, slug: &str) -> bool {
    parse::wikilink_targets(text).iter().any(|t| t == slug)
}

/// Whether a drawer still stores the headline this file derives.
///
/// Why: once [`links_to`] has established that a candidate is the file's own
/// drawer, this only decides how to *describe* the skip — matched, or drifted —
/// so a mismatch never changes the write decision.
/// What: compares first lines; when the derived headline is empty (a file with
/// no `description`), falls back to whole-text equality.
/// Test: `headline_match_identifies_own_drawer`.
fn has_headline_of(drawer_content: &str, derived_text: &str) -> bool {
    let headline = derived_text.lines().next().unwrap_or_default();
    if headline.is_empty() {
        drawer_content.trim() == derived_text.trim()
    } else {
        drawer_content.lines().next().unwrap_or_default() == headline
    }
}

/// Write one derived memory as a palace drawer.
///
/// Why: `memory_remember` is the same write surface the MCP tool layer uses,
/// reached through trusty-common's shared discovery + JSON-RPC entry point
/// rather than a bespoke REST call.
/// What: posts `{ palace, text, tags, force: true, allow_secret_like }` and
/// returns the new `drawer_id`. A response without a `drawer_id` means the
/// daemon declined the write (a content gate fired); its `reason`/`status` is
/// surfaced as the error so the report says why.
/// Test: covered end-to-end by `writes_and_then_skips_against_stub_daemon`.
async fn write_drawer(
    base_url: &str,
    opts: &ImportOptions,
    parsed: &ParsedMemory,
) -> anyhow::Result<String> {
    let result = trusty_common::mcp::memory_rpc::call_memory_tool_at(
        base_url,
        "memory_remember",
        json!({
            "palace": opts.palace,
            "text": parsed.text,
            "tags": parsed.tags,
            "force": true,
            "allow_secret_like": opts.allow_secret_like,
        }),
    )
    .await?;
    match result.get("drawer_id").and_then(Value::as_str) {
        Some(id) => Ok(id.to_string()),
        None => {
            let reason = result
                .get("reason")
                .or_else(|| result.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("no drawer_id in response");
            anyhow::bail!("trusty-memory declined the write: {reason}")
        }
    }
}
