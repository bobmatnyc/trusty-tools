//! The secrets scan the DD report used to disclaim (#6077, epic #6074).
//!
//! Why: the report's exec summary names secret leakage as an assurance gap it
//! does not fill, and that was true only because nothing in the pipeline
//! scanned for one. `gitleaks` answers the question deterministically against
//! any repository's tree in one subprocess, and the answer belongs in the
//! report rather than in this process's stderr.
//!
//! This is a DIFFERENT question from `crate::credential_scan`, which checks the
//! invoking session's OWN outbound package for credentials this process holds.
//! That one protects the operator; this one measures the target.
//!
//! Owner ruling 2026-08-19 puts the collector HERE rather than in
//! trusty-review: all collector intelligence lives in trusty-audit and the
//! manifest is the interface, so tuning what is scanned is a single-crate
//! rebuild.
//!
//! What: one leg, `gitleaks detect --no-git` against the checkout, reduced to
//! one [`Leak`] per row. [`write_into`] puts them in `[report].findings` through
//! [`super::findings::append`], the shared writer #6075 and #6076 also use;
//! [`ground_into`] is the caller-facing shape every other grounding leg has.
//!
//! ## The value never leaves [`parse`]
//!
//! gitleaks reports the matched credential verbatim in two fields (`Secret` and
//! `Match`). Neither is carried out of [`parse`]: the row a reader sees carries
//! `trusty_common::credentials::redact_secret`'s masked preview instead — the
//! workspace's one redaction implementation (#2401), not a second one written
//! here. Redacting at the parse boundary rather than at the write boundary is
//! deliberate: no other function in this module ever holds the value, so no
//! later change can route it somewhere by accident.
//!
//! `-v` is deliberately NOT passed to gitleaks, because its verbose stderr
//! prints matched secrets — and stderr's first line reaches a gap line.
//!
//! Two other places the raw value exists, both bounded: gitleaks' own report
//! file, which lives in a 0700 [`TempDir`] its drop removes (see
//! [`private_report_dir`]), and [`Run::report`], whose `Debug` is hand-written
//! so no `{:?}` can print it.
//!
//! ## Scope, and why there is no ecosystem gate
//!
//! Unlike [`super::cve`] and [`super::license`], this leg reads no dependency
//! manifest, so [`super::ecosystem::detect`] has nothing to say about it: every
//! repository has a tree that can hold a credential, and there is no language
//! for which the scan does not apply. There is therefore no declared-skip arm —
//! a checkout either gets scanned or earns a gap.
//!
//! The scan covers the WORKING TREE, not git history. `--no-git` is what keeps
//! the cost proportional to the checkout rather than to every revision ever
//! made, and a secret that was committed and later removed is exactly the case
//! it does not see. The clean-scan gap line says so on the page rather than
//! leaving the reader to assume otherwise.
//!
//! ## Degradation
//!
//! Two outcomes ([`Outcome`]), and the fail-open rule is the one every leg in
//! this module follows: the sweep continues, and the cost is a NAMED line in
//! `[report].gaps`, never a silent zero-findings result (#5620 — a recorded
//! skip permits, a blind gate does not). Every gap this module writes LEADS
//! with its own diagnosis; a child process's stderr is only ever the
//! parenthetical (#6720).
//!
//! Test: `secrets_tests`.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use toml_edit::{InlineTable, Value};

use super::cve::Severity;
use super::findings::first_line;

/// The collector's name, as it appears at the head of every gap line it writes.
pub const COLLECTOR: &str = "secrets-scan";

/// The `category` every leak this collector records carries.
///
/// trusty-review's `report::assurance::subsection_title` already renders this
/// key as "Secret Leakage" — the third of the three categories epic #6074
/// defines, and the reason this leg needs no change in that crate.
pub const CATEGORY: &str = "secrets";

/// The binary that performs the scan.
pub const BINARY: &str = "gitleaks";

/// What an operator runs to get [`BINARY`], named in the missing-binary gap.
pub const INSTALL_COMMAND: &str = "brew install gitleaks";

/// The keys that identify one declared row, for the resumed-sweep skip.
///
/// The rule and the location alone would collapse two distinct credentials
/// matched by the same rule on the same line; the title carries the redacted
/// preview, which separates them.
const IDENTITY: &[&str] = &["id", "package", "title"];

/// Substring marking a gitleaks rule that matches on shape and entropy rather
/// than on a provider's own credential format.
///
/// Why: `generic-api-key` is the rule that fires on "a long high-entropy string
/// next to the word `token`". It is worth reporting and it is not worth
/// reporting at the same band as a matched AWS key id, because it is the rule
/// that produces the false positives a reader has to triage by hand.
const GENERIC_RULE_MARKER: &str = "generic";

/// One credential gitleaks matched in the target's working tree.
///
/// Why: these are the fields a due-diligence reader acts on — which rule, in
/// which file at which line, how confident, and enough of the value to find it
/// without the value itself ever being written down.
/// What: `excerpt` is ALREADY REDACTED when this struct is constructed; the raw
/// value gitleaks reported exists only inside [`parse`]. `severity` is this
/// collector's own band, reusing [`Severity`] rather than declaring a second
/// enum whose `as_str` returns the same two strings.
/// Test: `secrets_tests::{a_provider_credential_bands_red,
/// a_generic_entropy_match_bands_amber}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Leak {
    /// The gitleaks rule that matched, e.g. `aws-access-token`.
    pub rule: String,
    /// The file, relative to the checkout when gitleaks reported it absolutely.
    pub file: String,
    /// The line the match starts on.
    pub line: u64,
    /// The band this row renders under.
    pub severity: Severity,
    /// The rule's own one-line description.
    pub description: String,
    /// A non-reversible preview of the matched value — never the value.
    pub excerpt: String,
}

impl Leak {
    /// `file:line`, the location a reader opens.
    ///
    /// Test: `secrets_tests::the_fixture_yields_every_row`.
    #[must_use]
    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }

    /// The row's summary: what the rule found, and the redacted preview.
    ///
    /// Test: `secrets_tests::the_manifest_never_carries_the_secret_value`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{} — redacted match {}", self.description, self.excerpt)
    }
}

/// What the leg produced for one repository.
///
/// Why: there is no third variant here, unlike [`super::cve::Outcome`]. A
/// repository with no dependency manifest has no CVE surface to miss; there is
/// no equivalent repository with no tree, so "does not apply" is not a state
/// this leg can be in.
/// What: the scan's rows (possibly empty, which IS a clean bill for the working
/// tree), or a failure carrying the one line the caller turns into a gap.
/// Test: `secrets_tests::{a_clean_scan_states_its_own_scope,
/// an_uninstalled_binary_is_a_named_gap}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The scan ran; these are its rows, in the order gitleaks reported them.
    Scanned(Vec<Leak>),
    /// The scan could not run, or could not be read; why not.
    Unavailable(String),
}

/// One completed `gitleaks` invocation, as this module needs to see it.
///
/// Why: gitleaks exits NON-ZERO when it finds leaks, so the exit status alone
/// cannot say whether the run failed — the report document does. Carrying both
/// is what lets [`scan_with`] apply that rule in one place, and what lets the
/// failure arms be tested without a `gitleaks` on the machine.
/// What: whether the process exited 0, the JSON report it wrote, and its
/// diagnostics. `report` is the file's content rather than stdout because
/// gitleaks writes its findings to `--report-path`, never to stdout.
/// Test: `secrets_tests::a_nonzero_exit_with_a_readable_report_is_still_a_scan`.
///
/// `Debug` is HAND-WRITTEN, and must stay that way: `report` holds gitleaks'
/// `Secret` and `Match` fields verbatim — the only place in this crate the
/// matched credential exists outside [`parse`] — and `stderr` is a child
/// process's output this module does not control. A derived `Debug` puts both
/// into any `{:?}`, `tracing` field, or test panic that ever touches this
/// struct. The impl below prints byte counts and nothing else.
/// Test: `secrets_tests::the_raw_run_never_debug_prints_its_content`.
#[derive(Clone)]
#[non_exhaustive]
pub struct Run {
    /// Whether the process exited zero.
    pub success: bool,
    /// The JSON document `--report-format json` produced.
    pub report: String,
    /// Diagnostics, used only as the parenthetical of a gap line (#6720).
    pub stderr: String,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("success", &self.success)
            .field("report", &Withheld(self.report.len()))
            .field("stderr", &Withheld(self.stderr.len()))
            .finish()
    }
}

/// A field [`Run`]'s `Debug` states the size of rather than the content of.
struct Withheld(usize);

impl std::fmt::Debug for Withheld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{} bytes, withheld>", self.0)
    }
}

/// Scan `checkout`'s working tree, or say why there is no scan.
///
/// Why/What: see the module docs.
///
/// # Postconditions
/// Never panics and never returns an error: every failure is an
/// [`Outcome::Unavailable`] reason string, safe to show the recipient, and
/// carrying no matched credential.
///
/// Test: `secrets_tests`.
#[must_use]
pub fn scan(checkout: &Path) -> Outcome {
    scan_with(checkout, run_gitleaks)
}

/// [`scan`] with the subprocess supplied by the caller.
///
/// Why: the arms this collector MUST get right — the binary is not installed,
/// the spawn failed, the run exited non-zero with nothing to read, the report is
/// not JSON — are precisely the arms a test cannot reach through a real
/// `gitleaks`, which is either installed on the machine or not. `run` is the
/// seam, and it also keeps the subprocess out of every unit test, so nothing
/// here spawns a process or touches the network.
/// What: the directory check, then `run`, then [`parse`]. A `run` that hands
/// back an `Err` is [`Outcome::Unavailable`] carrying its reason verbatim — the
/// reason is already this module's own diagnosis. An EMPTY report is a clean
/// scan only when the process also exited zero; empty after a non-zero exit is
/// a failed run, and calling it clean would be the false clean claim #5620
/// forbids. A report that parses is a scan WHATEVER the exit status, because
/// gitleaks exits 1 on finding leaks and that is its most important result.
/// Test: `secrets_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_spawn_failure_is_a_named_gap, a_nonzero_exit_with_no_report_is_a_named_gap,
/// an_unreadable_report_is_a_named_gap,
/// a_nonzero_exit_with_a_readable_report_is_still_a_scan}`.
#[must_use]
pub fn scan_with<F>(checkout: &Path, run: F) -> Outcome
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    if !checkout.is_dir() {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: {} is not a directory, so there was no working tree to scan",
            checkout.display()
        ));
    }
    let output = match run(checkout) {
        Ok(output) => output,
        Err(cause) => return Outcome::Unavailable(cause),
    };
    let report = output.report.trim();
    if report.is_empty() {
        return if output.success {
            Outcome::Scanned(Vec::new())
        } else {
            Outcome::Unavailable(format!(
                "{COLLECTOR}: `{BINARY}` exited non-zero and left no findings report to read ({})",
                first_line(&output.stderr)
            ))
        };
    }
    match parse(report, checkout) {
        Ok(leaks) => Outcome::Scanned(leaks),
        Err(cause) if output.success => Outcome::Unavailable(format!("{COLLECTOR}: {cause}")),
        Err(cause) => Outcome::Unavailable(format!(
            "{COLLECTOR}: {cause}, and `{BINARY}` exited non-zero ({})",
            first_line(&output.stderr)
        )),
    }
}

/// Reduce one gitleaks JSON report to its leaks, redacting as it goes.
///
/// Why: split from [`scan_with`] so every row shape is testable against a
/// captured document with no `gitleaks`, no checkout and no subprocess in the
/// test. It is also the ONE place the matched credential exists — see the module
/// docs.
/// What: each array element becomes a [`Leak`] whose `excerpt` is
/// `trusty_common::credentials::redact_secret` applied to the reported value. A
/// row naming no file is skipped: it has no location a reader could open. An
/// empty array is a clean scan, not an error.
///
/// # Errors
/// One line when the document is not JSON, or is not the array every
/// `--report-format json` run emits.
///
/// # Postconditions
/// No returned [`Leak`] contains the `Secret` or `Match` gitleaks reported.
///
/// Test: `secrets_tests::{the_fixture_yields_every_row,
/// the_manifest_never_carries_the_secret_value,
/// output_that_is_not_json_is_a_reason}`.
pub fn parse(json: &str, checkout: &Path) -> Result<Vec<Leak>, String> {
    let doc: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("`{BINARY}` report is not readable as JSON ({e})"))?;
    let rows = doc
        .as_array()
        .ok_or_else(|| format!("`{BINARY}` report is not the array of findings it emits"))?;

    let mut leaks = Vec::with_capacity(rows.len());
    for entry in rows {
        let field = |key: &str| {
            entry
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let Some(file) = field("File").or_else(|| field("SymlinkFile")) else {
            continue;
        };
        let rule = field("RuleID").unwrap_or("unnamed-rule").to_string();
        // #6077: the two fields carrying the credential. Read here, redacted
        // here, and never stored — see the module docs.
        let secret = field("Secret").or_else(|| field("Match")).unwrap_or("");
        leaks.push(Leak {
            severity: band(&rule),
            description: field("Description")
                .unwrap_or("a credential-shaped value")
                .to_string(),
            excerpt: trusty_common::credentials::redact_secret(secret),
            file: relative(file, checkout),
            line: entry
                .get("StartLine")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            rule,
        });
    }
    Ok(leaks)
}

/// The band one rule's matches render under.
///
/// Why: gitleaks states no severity. The split that carries information is the
/// one between a provider's own credential format — an AWS key id, a Slack
/// token, a private-key header, all of which mean a real credential is in the
/// tree — and the entropy heuristic, which means a human has to look. That maps
/// onto the report's RED/AMBER vocabulary exactly.
/// Test: `secrets_tests::{a_provider_credential_bands_red,
/// a_generic_entropy_match_bands_amber}`.
#[must_use]
pub fn band(rule: &str) -> Severity {
    if rule.to_ascii_lowercase().contains(GENERIC_RULE_MARKER) {
        Severity::Amber
    } else {
        Severity::Red
    }
}

/// The path as the report should state it: relative to the checkout it scanned.
///
/// gitleaks reports paths relative to `--source` in some versions and absolutely
/// in others; an absolute path also leaks the operator's directory layout into a
/// document that leaves their machine.
fn relative(file: &str, checkout: &Path) -> String {
    Path::new(file)
        .strip_prefix(checkout)
        .map_or_else(|_| file.to_string(), |path| path.display().to_string())
}

/// Run `gitleaks detect --no-git` against `checkout`.
///
/// Why `--no-git`: it scans the working tree. The git-history mode reads every
/// revision's diff, which on a large target is minutes of CPU on every core for
/// a question the tree already answers — and the clean-scan gap line states that
/// history is out of scope rather than letting the omission read as a clean
/// history.
/// What: the report goes inside a private [`TempDir`] (see
/// [`private_report_dir`]) — never under the target repository, and never
/// world-readable — which its own drop removes on every return path from here.
/// `output()` waits for the child, so the process is reaped here. Only v8's
/// oldest and most stable flags are used, and `-v` is deliberately absent (it
/// prints matched secrets to stderr, which reaches a gap line).
///
/// # Errors
/// One line, leading with this module's own diagnosis, when the binary is not
/// installed, no private directory can be made, or the spawn fails (#6720).
fn run_gitleaks(checkout: &Path) -> Result<Run, String> {
    let binary = trusty_common::bin_resolve::resolve_binary(BINARY).ok_or_else(|| {
        format!(
            "{COLLECTOR}: `{BINARY}` is not installed, so no secrets scan ran (install it with \
             `{INSTALL_COMMAND}`)"
        )
    })?;
    // Bound to a name so it lives until the report has been read, and drops —
    // removing the report with it — however this function returns.
    let dir = private_report_dir()?;
    let report_path = report_path_in(&dir);
    let output = Command::new(&binary)
        .arg("detect")
        .arg("--source")
        .arg(checkout)
        .arg("--no-git")
        .arg("--report-format")
        .arg("json")
        .arg("--report-path")
        .arg(&report_path)
        .output()
        .map_err(|e| format!("{COLLECTOR}: `{BINARY}` could not be run ({e})"))?;

    let report = std::fs::read_to_string(&report_path).unwrap_or_default();

    Ok(Run {
        success: output.status.success(),
        report,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// A directory only this user can read, for gitleaks to write its report into.
///
/// Why: the report is the one artefact in this pipeline holding the matched
/// credentials UNREDACTED. A path composed by hand under [`std::env::temp_dir`]
/// is created by gitleaks at its own umask — 0644 on a world-readable `/tmp` —
/// and stays readable by every local account for as long as the scan runs.
///
/// Why two directories rather than one: `tempfile` supplies the unguessable name
/// and the drop that removes the tree however the caller returns — including the
/// early-return and panic paths a manual `remove_file` misses — but it creates
/// its directory through `std::fs::create_dir`, which lands 0755 under the usual
/// 022 umask. Chmod-ing it afterwards leaves a window in which another local
/// account can open a descriptor on the directory and read, through that
/// descriptor, files created in it after the mode changed. So the report goes in
/// a SUBDIRECTORY made by `mkdir(2)` with an explicit 0700 — a umask can only
/// clear bits, so that one is private from birth and there is no window at all.
/// What: a `TempDir` named [`REPORT_DIR_PREFIX`]`<random>` holding a 0700
/// [`REPORT_SUBDIR`]. The caller keeps it alive for exactly as long as it needs
/// the report, and reads the path from [`report_path_in`].
///
/// # Errors
/// One line, leading with this module's own diagnosis, when either directory
/// cannot be created — the collector refuses to write the raw report anywhere
/// less private rather than falling back (#6077).
///
/// Test: `secrets_tests::the_raw_report_lives_in_a_private_directory_and_does_not_outlive_the_run`.
fn private_report_dir() -> Result<TempDir, String> {
    let refuse = |what: &str, e: std::io::Error| {
        format!(
            "{COLLECTOR}: no private {what} could be made for `{BINARY}`'s raw report ({e}), and \
             the collector will not write it anywhere less private"
        )
    };
    let dir = tempfile::Builder::new()
        .prefix(REPORT_DIR_PREFIX)
        .tempdir()
        .map_err(|e| refuse("directory", e))?;

    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(PRIVATE_MODE);
    }
    builder
        .create(dir.path().join(REPORT_SUBDIR))
        .map_err(|e| refuse("subdirectory", e))?;
    Ok(dir)
}

/// Where gitleaks writes its report inside `dir` — the private subdirectory.
fn report_path_in(dir: &TempDir) -> PathBuf {
    dir.path().join(REPORT_SUBDIR).join(REPORT_FILE)
}

/// Prefix of the private directory [`private_report_dir`] creates.
const REPORT_DIR_PREFIX: &str = "trusty-audit-secrets-";

/// The 0700 subdirectory the raw report actually lives in.
const REPORT_SUBDIR: &str = "private";

/// The report's name inside that subdirectory.
const REPORT_FILE: &str = "report.json";

/// Owner-only, the mode [`REPORT_SUBDIR`] is created with on Unix.
#[cfg(unix)]
const PRIVATE_MODE: u32 = 0o700;

/// Record `leaks` in the manifest at `path` as `[report].findings`.
///
/// What: builds one row per leak and hands them to [`super::findings::append`],
/// the writer #6075 and #6076 share. `version` is written empty — a credential
/// in a tree has no version, and the renderer's fixed column set shows an
/// em-dash for it.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back.
///
/// # Postconditions
/// On `Ok`, every leak is declared exactly once and nothing else in the document
/// changed. No matched credential is written. An empty `leaks` writes nothing
/// and cannot fail.
///
/// Test: `secrets_tests::{the_leaks_land_in_the_manifest,
/// a_resumed_sweep_does_not_restate_a_leak,
/// the_manifest_never_carries_the_secret_value}`.
pub fn write_into(path: &Path, leaks: &[Leak]) -> Result<(), String> {
    let rows: Vec<InlineTable> = leaks.iter().map(row).collect();
    super::findings::append(path, &rows, IDENTITY)
}

/// One leak as the inline table trusty-review deserialises.
fn row(leak: &Leak) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("category", Value::from(CATEGORY));
    table.insert("id", Value::from(leak.rule.as_str()));
    table.insert("package", Value::from(leak.location()));
    table.insert("version", Value::from(""));
    table.insert("severity", Value::from(leak.severity.as_str()));
    table.insert("title", Value::from(leak.summary()));
    table
}

/// [`scan`], then write what it produced into `manifest`.
///
/// Why: the same shape [`super::ground_manifest`] gives every other leg — the
/// caller gets gap lines and nothing else to decide about.
/// What: an unavailable scan is one gap naming `display`, the cause, and what
/// the report therefore will not carry; a scan that found rows writes them, and
/// a write failure becomes a gap of its own. A scan that found NOTHING writes no
/// rows and states its own scope, because "gitleaks found no credential" and "no
/// gitleaks ran" must not read the same on the page.
/// Test: `secrets_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_clean_scan_states_its_own_scope,
/// a_manifest_that_cannot_be_written_is_a_named_gap}`.
pub fn ground_into(manifest: &Path, checkout: &Path, display: &str) -> Vec<String> {
    ground_into_with(manifest, checkout, display, run_gitleaks)
}

/// [`ground_into`] with the subprocess supplied by the caller — see [`scan_with`].
pub fn ground_into_with<F>(manifest: &Path, checkout: &Path, display: &str, run: F) -> Vec<String>
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    match scan_with(checkout, run) {
        Outcome::Unavailable(cause) => vec![format!(
            "{display}: {cause} — the report states no secret leakage for it, which must be read \
             as unassessed rather than as a tree with no credential in it"
        )],
        Outcome::Scanned(leaks) if leaks.is_empty() => vec![format!(
            "{display}: {COLLECTOR}: `{BINARY}` scanned its working tree and matched no \
             credential. Git history, files outside the checkout, secrets held in a configuration \
             store rather than in the tree, and any credential shape gitleaks' rule set does not \
             cover are not covered by that scan"
        )],
        Outcome::Scanned(leaks) => match write_into(manifest, &leaks) {
            Ok(()) => Vec::new(),
            Err(cause) => vec![format!(
                "{display}: {COLLECTOR}: {cause} — the report states none of the {} leak(s) \
                 `{BINARY}` reported for it",
                leaks.len()
            )],
        },
    }
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod secrets_tests;
