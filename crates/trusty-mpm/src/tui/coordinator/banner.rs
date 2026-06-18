//! Claude-Code-style startup banner + backplane probes (STUI-0, DOC-16 §3.1).
//!
//! Why: operators launching `tm sessions tui` want the same confidence-building
//! startup Claude Code gives — the tool names itself + its version and confirms
//! the trusty backplane (memory + search) is live before the interactive view
//! takes over the terminal. DOC-16 §3.1 (STUI-0) pins that contract: a banner
//! line `trusty-mpm sessions v<CARGO_PKG_VERSION>`, a memory line (probed against
//! the fixed user-level `user` palace, decision D4), a search line (plain health
//! probe), and a startup line with the active-session count + a `/help` hint. A
//! probe failure renders `○ unreachable` and never blocks the TUI (§3.1 error
//! conditions).
//! What: the pure [`compose_banner`] (probe results + count → the exact banner
//! text, unit-testable offline), the [`ProbeOutcome`] result type, the async
//! [`probe_memory`] / [`probe_search`] probes (short-timeout, fail-safe), and
//! [`print_startup_banner`] which runs the probes and prints the banner to
//! stderr (stdout stays clean) before the alternate screen is entered.
//! Test: `super::tests` covers the pure composition (version, glyphs, palace
//! note, count + `/help`) for active/unreachable/zero-count permutations; the
//! async probes degrade to `unreachable` against a dead daemon and are exercised
//! by `probe_unreachable_*`.

use std::time::Duration;

use crate::tui::health::{DEFAULT_MEMORY_URL, DEFAULT_SEARCH_URL};

/// Per-probe HTTP timeout for the startup backplane checks.
///
/// Why: a down (or slow) memory/search daemon must not stall the operator's
/// startup; a short timeout turns an unresponsive service into a clean
/// `○ unreachable` line rather than a multi-second hang before the TUI opens.
/// What: two seconds — comfortably above a healthy local round-trip yet short
/// enough to keep startup snappy when a service is down.
/// Test: timeout value is side-effect-only; the unreachable path is covered by
/// `probe_unreachable_memory_is_inactive` / `probe_unreachable_search_is_inactive`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The fixed user-level palace the banner confirms memory against (D4).
///
/// Why: DOC-16 decision D4 fixes memory confirmation to a single user-scoped
/// palace named `user` (shared across projects, distinct from the per-project
/// and `session-manager` palaces). Naming it once here single-sources the id
/// between the probe and its rendered note.
/// What: the literal palace id `user`.
/// Test: `compose_banner_active_shows_palace_note` asserts the rendered note
/// carries this id.
pub const USER_PALACE: &str = "user";

/// Glyph shown for an active (reachable + confirmed) backplane service.
///
/// Why: §3.1 pins `●` for active so the operator reads "live" at a glance; a
/// constant keeps the glyph single-sourced between the renderer and its tests.
/// What: the literal `●`.
/// Test: `compose_banner_active_shows_glyphs`.
pub const GLYPH_ACTIVE: char = '●';

/// Glyph shown for an unreachable backplane service.
///
/// Why: the counterpart to [`GLYPH_ACTIVE`] — §3.1 pins `○` for unreachable so
/// a degraded backplane reads distinctly without aborting startup.
/// What: the literal `○`.
/// Test: `compose_banner_unreachable_shows_glyphs`.
pub const GLYPH_UNREACHABLE: char = '○';

/// The `/help` hint appended to the startup line.
///
/// Why: §3.1 requires the startup line to point operators at `/help`; naming
/// the literal here keeps the hint single-sourced.
/// What: the literal `type /help for commands`.
/// Test: `compose_banner_appends_help_hint`.
pub const HELP_HINT: &str = "type /help for commands";

/// The outcome of probing one backplane service for the startup banner.
///
/// Why: the pure [`compose_banner`] must render memory/search lines without
/// performing any IO, so the async probes collapse their (network-dependent)
/// result into this small value the composer consumes. Carrying an optional
/// `note` lets the memory probe surface the `user`-palace detail (or the reason
/// it is degraded) alongside the active/unreachable verdict (D4).
/// What: `active` (reachable + any service-specific confirmation passed) plus an
/// optional parenthetical `note` (e.g. `palace: user`).
/// Test: `compose_banner_*` construct these directly to drive the composer
/// offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// Whether the service is active (health probe succeeded and, for memory,
    /// the `user` palace is present/creatable).
    pub active: bool,
    /// Optional parenthetical detail rendered after the status (e.g.
    /// `palace: user`, or a degraded reason). `None` renders no parenthetical.
    pub note: Option<String>,
}

impl ProbeOutcome {
    /// Build an active outcome carrying an optional note.
    ///
    /// Why: keeps the probe call sites terse when reporting success.
    /// What: `ProbeOutcome { active: true, note }`.
    /// Test: `compose_banner_active_shows_palace_note`.
    pub fn active(note: Option<String>) -> Self {
        Self { active: true, note }
    }

    /// Build an unreachable outcome carrying an optional reason note.
    ///
    /// Why: keeps the probe call sites terse when reporting a degraded service.
    /// What: `ProbeOutcome { active: false, note }`.
    /// Test: `compose_banner_unreachable_shows_glyphs`.
    pub fn unreachable(note: Option<String>) -> Self {
        Self {
            active: false,
            note,
        }
    }

    /// The status glyph for this outcome.
    ///
    /// Why: the composer renders `●`/`○` per the active flag; centralising the
    /// mapping keeps it consistent across the two service lines.
    /// What: [`GLYPH_ACTIVE`] when active, else [`GLYPH_UNREACHABLE`].
    /// Test: `compose_banner_active_shows_glyphs`,
    /// `compose_banner_unreachable_shows_glyphs`.
    fn glyph(&self) -> char {
        if self.active {
            GLYPH_ACTIVE
        } else {
            GLYPH_UNREACHABLE
        }
    }

    /// The status word for this outcome.
    ///
    /// Why: the service line reads `<glyph> <word>`; a pure helper keeps the
    /// word mapping in one place.
    /// What: `active` when active, else `unreachable`.
    /// Test: `compose_banner_unreachable_shows_glyphs`.
    fn word(&self) -> &'static str {
        if self.active { "active" } else { "unreachable" }
    }
}

/// Compose the full startup banner text from probe results and a session count.
///
/// Why: STUI-0's banner shape (§3.1) is a behavior contract that must be
/// verifiable WITHOUT a terminal or live daemons; keeping composition pure
/// (probe outcomes + count to string) makes the exact layout — a version line,
/// memory/search glyph lines with optional parentheticals, and an active-count
/// plus `/help` startup line — unit-testable offline. The version is the crate
/// `CARGO_PKG_VERSION` resolved at the call site so this stays a pure function.
/// What: returns a multi-line string of the form:
/// ```text
/// trusty-mpm sessions  v<version>
///   memory  <glyph> <word>   (<note>)
///   search  <glyph> <word>
/// <count> active sessions  ·  type /help for commands
/// ```
/// The memory/search parentheticals are emitted only when the outcome carries a
/// `note`. The count line uses the singular `session` when `active_count == 1`
/// and renders a daemon-down marker when `active_count` is `None`.
/// Test: `compose_banner_active_shows_glyphs`,
/// `compose_banner_unreachable_shows_glyphs`,
/// `compose_banner_active_shows_palace_note`, `compose_banner_appends_help_hint`,
/// `compose_banner_pluralizes_count`, `compose_banner_handles_unknown_count`.
pub fn compose_banner(
    version: &str,
    memory: &ProbeOutcome,
    search: &ProbeOutcome,
    active_count: Option<usize>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("trusty-mpm sessions  v{version}\n"));
    out.push_str(&service_line("memory", memory));
    out.push_str(&service_line("search", search));
    out.push_str(&startup_line(active_count));
    out
}

/// Build one backplane service line: `  <label>  <glyph> <word>   (<note>)`.
///
/// Why: the memory and search lines share the same shape (only the label and the
/// optional note differ); a helper keeps the two-space indent, the padded label,
/// and the optional parenthetical consistent.
/// What: returns `  <label padded to 6>  <glyph> <word>` plus `   (<note>)` when
/// the outcome carries a note, terminated by a newline.
/// Test: covered by the `compose_banner_*` tests via [`compose_banner`].
fn service_line(label: &str, outcome: &ProbeOutcome) -> String {
    let mut line = format!("  {label:<6}  {} {}", outcome.glyph(), outcome.word());
    if let Some(note) = &outcome.note {
        line.push_str(&format!("   ({note})"));
    }
    line.push('\n');
    line
}

/// Build the startup line: `<count> active sessions  ·  type /help for commands`.
///
/// Why: §3.1 requires the active-session count plus a `/help` hint; an unknown
/// count (daemon unreachable at startup) degrades to a marker rather than a
/// misleading `0`. A pure helper keeps the pluralisation and the unknown-count
/// branch testable offline.
/// What: renders `<n> active session(s)` (singular at `1`) when the count is
/// known, or `daemon unreachable` when `None`, then `  ·  <HELP_HINT>`, and
/// terminates with a single trailing newline so the composed banner ends cleanly
/// (the print site adds exactly one blank separator, never two).
/// Test: `compose_banner_pluralizes_count`, `compose_banner_handles_unknown_count`,
/// `compose_banner_has_single_trailing_newline`.
fn startup_line(active_count: Option<usize>) -> String {
    let lead = match active_count {
        Some(1) => "1 active session".to_string(),
        Some(n) => format!("{n} active sessions"),
        None => "daemon unreachable".to_string(),
    };
    format!("{lead}  ·  {HELP_HINT}\n")
}

/// Probe trusty-search health for the startup banner (fail-safe).
///
/// Why: §3.1 requires search to be confirmed via its plain health probe; a
/// failure must degrade to `○ unreachable` (the TUI still opens), never abort.
/// What: GETs `<base>/health` with a short timeout; any transport or non-2xx
/// response yields [`ProbeOutcome::unreachable`]. `base` defaults to
/// [`DEFAULT_SEARCH_URL`] when `None`. Builds a single bounded client and reuses
/// it for the health check.
/// Test: `probe_unreachable_search_is_inactive` drives the dead-daemon branch;
/// the live path is exercised by launching the TUI against a running daemon.
pub async fn probe_search(base: Option<&str>) -> ProbeOutcome {
    let base = base.unwrap_or(DEFAULT_SEARCH_URL);
    let client = probe_client();
    if health_ok_with(&client, base).await {
        ProbeOutcome::active(None)
    } else {
        ProbeOutcome::unreachable(None)
    }
}

/// Probe trusty-memory health + the fixed `user` palace for the banner (D4).
///
/// Why: §3.1 + decision D4 make memory "active" only when (a) the health probe
/// succeeds AND (b) the `user` palace exists — created idempotently on first run
/// via `POST /api/v1/palaces`. The probe is fail-safe: a down daemon or a failed
/// palace assertion degrades to `○ unreachable` with a reason note, and the TUI
/// still opens.
/// What: GETs `<base>/health`; on success, GETs `<base>/api/v1/palaces/user` and,
/// only on a real 404, POSTs `<base>/api/v1/palaces` with `{"name":"user"}` to
/// create it. Builds a single bounded client and threads it into both helpers.
/// Returns [`ProbeOutcome::active`] with note `palace: user` when memory is live
/// and the palace is present/creatable, else [`ProbeOutcome::unreachable`] with a
/// short reason. `base` defaults to [`DEFAULT_MEMORY_URL`] when `None`.
/// Test: `probe_unreachable_memory_is_inactive` drives the dead-daemon branch;
/// the live + palace-assertion path is exercised against a running daemon.
pub async fn probe_memory(base: Option<&str>) -> ProbeOutcome {
    let base = base.unwrap_or(DEFAULT_MEMORY_URL);
    let client = probe_client();
    if !health_ok_with(&client, base).await {
        return ProbeOutcome::unreachable(Some("memory daemon down".to_string()));
    }
    if ensure_user_palace_with(&client, base).await {
        ProbeOutcome::active(Some(format!("palace: {USER_PALACE}")))
    } else {
        ProbeOutcome::unreachable(Some(format!("{USER_PALACE} palace unavailable")))
    }
}

/// Build the short-timeout HTTP client shared by the startup probes.
///
/// Why: every probe needs the same bounded client so a hung service cannot stall
/// startup; centralising construction keeps the timeout single-sourced.
/// What: a `reqwest::Client` with [`PROBE_TIMEOUT`], falling back to the default
/// client if the builder somehow fails (it cannot in practice).
/// Test: covered indirectly by the probe tests.
pub(super) fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// GET `<base>/health` with the shared client and report whether it answered 2xx.
///
/// Why: both probes share the same health check; a daemon that is down, slow, or
/// returns an error status all collapse to "not healthy" so the banner degrades
/// cleanly. Taking the client by reference lets each probe build exactly one
/// client and reuse it across the health check and any follow-up request.
/// What: issues a bounded GET to `<base>/health` via `client` and returns `true`
/// only on a 2xx response.
/// Test: covered by `probe_unreachable_*` (dead daemon → `false`).
async fn health_ok_with(client: &reqwest::Client, base: &str) -> bool {
    matches!(
        client.get(format!("{base}/health")).send().await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Ensure the fixed `user` palace exists at `base`, creating it ONLY on a 404 (D4).
///
/// Why: D4 makes memory "active" only when the `user` palace is present; the
/// banner asserts it idempotently so first-run operators get a confirmed memory
/// line without a manual provisioning step. Crucially, creation must be gated on
/// a *real* 404 — a transient `500`/`503` (or any other non-2xx) means the
/// daemon's state is unknown, so blindly POSTing a create would be wrong (it
/// could race a half-up daemon or mask a real outage). Those cases degrade to
/// `false` (memory renders `○ unreachable`) instead of attempting a write.
/// What: GETs `<base>/api/v1/palaces/user` with the shared `client` and branches
/// on the status: a 2xx means the palace already exists → `true`; a 404 means it
/// is genuinely absent → POST `<base>/api/v1/palaces` with `{"name":"user"}` and
/// return whether the create answered 2xx; any other status (5xx, etc.) →
/// `false` without creating; a transport error → `false`.
/// Test: `ensure_user_palace_creates_only_on_404` exercises the status branching
/// against a stub server; the live assertion path runs against a running daemon.
pub(super) async fn ensure_user_palace_with(client: &reqwest::Client, base: &str) -> bool {
    let resp = match client
        .get(format!("{base}/api/v1/palaces/{USER_PALACE}"))
        .send()
        .await
    {
        Ok(resp) => resp,
        // Transport error (daemon unreachable): do not attempt a create.
        Err(_) => return false,
    };

    let status = resp.status();
    if status.is_success() {
        // Palace already exists.
        return true;
    }
    if status != reqwest::StatusCode::NOT_FOUND {
        // Any non-404 (5xx, etc.): state is unknown — do NOT create.
        return false;
    }

    // Genuine 404 → the palace is absent, so create it.
    matches!(
        client
            .post(format!("{base}/api/v1/palaces"))
            .json(&serde_json::json!({ "name": USER_PALACE }))
            .send()
            .await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Run the backplane probes and print the startup banner to stderr.
///
/// Why: §3.1 requires the banner to render before the interactive view opens.
/// Printing to stderr (not stdout) follows the workspace convention that stdout
/// stays clean for any downstream framing, and printing BEFORE the alternate
/// screen is entered (mirroring Claude Code) means the banner stays visible in
/// the operator's scrollback after the TUI exits. Probes run concurrently and
/// are fail-safe, so a down service slows startup by at most one [`PROBE_TIMEOUT`]
/// and never aborts.
/// What: probes memory + search concurrently, composes the banner with the
/// crate `CARGO_PKG_VERSION` and `active_count`, and writes it to stderr. The
/// composed banner ends with a single trailing newline (from `startup_line`), so
/// `eprintln!` adds exactly one blank separator after it — not two. `memory_url`
/// / `search_url` default to the canonical local addresses when `None`.
/// Test: composition is unit-tested via [`compose_banner`]; this print-only glue
/// is exercised by launching the TUI.
pub async fn print_startup_banner(
    memory_url: Option<&str>,
    search_url: Option<&str>,
    active_count: Option<usize>,
) {
    let (memory, search) = tokio::join!(probe_memory(memory_url), probe_search(search_url));
    let banner = compose_banner(env!("CARGO_PKG_VERSION"), &memory, &search, active_count);
    eprintln!("{banner}");
}
