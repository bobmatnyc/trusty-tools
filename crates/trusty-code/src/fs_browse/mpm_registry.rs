//! Re-source the project-picker roster from trusty-mpm's shared project
//! registry (issue #3435).
//!
//! Why: Bob's directive — "for 'Projects' we should be using the shared
//! (mpm) projects manager to list projects" — per the tcode-as-harness,
//! trusty-mpm-as-canonical-owner doctrine, `~/.trusty-mpm/project-registry/
//! projects.json` (surfaced over the mpm daemon's `GET /api/v1/projects`,
//! `crates/trusty-mpm/src/daemon/managed_routes/project_registry_routes.rs`)
//! is the single owner of "what projects does this operator work with", not
//! this crate's own filesystem scan (`super::roster`, issue #3365). That scan
//! forked project knowledge away from the canonical registry — expedient at
//! the time ("no pre-session roster endpoint exists yet"), wrong now that
//! one exists.
//!
//! **Consumption path (survey result, issue #3435 design question 1):** a
//! loopback HTTP call to the mpm daemon's `GET /api/v1/projects`
//! (`http://127.0.0.1:7880` by default, overridable via `TRUSTY_MPM_URL` —
//! the same env var `trusty-mpm-gui`'s `GuiState` already uses, so operators
//! have exactly one knob for "where is the mpm daemon" across both GUIs), NOT
//! a direct read of `projects.json`. Two reasons: (1) every existing
//! cross-daemon consumer in this workspace goes over loopback HTTP —
//! `trusty-mpm-gui/src/commands.rs`'s "1:1 proxy layer, never embed business
//! logic" pattern is the precedent this module mirrors — and there is no
//! workspace precedent for one daemon reading a sibling daemon's on-disk
//! store directly; (2) ADR-0018 (the loopback-only doctrine; ADR-0011 is
//! amended by it) explicitly sanctions same-machine loopback HTTP between
//! sibling daemons, including for "same-machine GUI clients" — a daemon-to-
//! daemon read is the same shape, one hop earlier. `crate::serve::mod` also
//! already documents port 7880 as trusty-mpm's reserved port in this crate's
//! own sibling-port table, so this module introduces no new fact, just a
//! caller.
//!
//! **Merged view (design question 2):** the registry is keyed on
//! `name`/`repo_url`, not a local filesystem path, and does not know about
//! folders an operator has never registered (e.g. a `bakeoff-l1..l5`
//! scratch checkout). Silently dropping those from the picker would be a
//! regression versus today's fs-only scan. So [`merge`] treats the registry
//! as PRIMARY (its entries win: `registered: true`, and its `name` overrides
//! whatever the local directory happened to be called) and the filesystem
//! scan as SECONDARY — any locally-discovered candidate NOT resolvable to a
//! registry entry survives in the roster with `registered: false`
//! ([`super::roster::ProjectCandidate::registered`]). A registry entry whose
//! `repo_url` cannot be resolved to an existing local checkout under
//! `~/trusty-mpm-projects/<owner>/<repo>` is skipped, not fabricated as a
//! roster row with a made-up path — the picker can only bind to a directory
//! that actually exists on this machine.
//!
//! **Registry hygiene (design question 3):** no `tm project remove` verb
//! exists yet (see #3364-era findings on stale entries like
//! `tm-throwaway-verify-2748`), so a stale registry entry becomes
//! user-visible here if — and only if — a matching local checkout still
//! exists under the owner-scoped layout; a stale entry with no local
//! checkout is silently absent from the roster already (see the previous
//! paragraph), which happens to filter most throwaway/no-longer-cloned
//! entries for free. Filtering live-but-stale entries whose checkout DOES
//! still exist is explicitly out of scope here — it depends on a future `tm
//! project remove` (or a repo_url throwaway-pattern heuristic the issue
//! itself calls out of scope) — this module deliberately does not guess.
//!
//! **Degradation:** if the mpm daemon is unreachable (down, wrong port,
//! timeout), [`merged_roster`] degrades to the plain filesystem-only roster
//! (every entry `registered: false`) rather than an empty picker or a
//! caller-facing error — the same best-effort philosophy `super::roster`
//! already documents.
//!
//! What: [`merged_roster`] is the real entry point `fs.list_projects`
//! (`super::protocol::list_projects`) calls — it resolves the mpm daemon URL,
//! runs the existing fs scan, and fetches the registry, then delegates to the
//! two testable pure/near-pure pieces: [`fetch_registry_projects`] (the one
//! network call) and [`merge`] (the merge logic, a plain function of already-
//! fetched data plus filesystem existence checks against a given `home` —
//! same real-vs-injectable split `super::roster::list_projects`/
//! `list_projects_in` already uses, so tests never touch the developer's
//! actual home directory or a real mpm daemon).
//! Test: `tests::*`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::is_git_repo;
use super::roster::{MAX_ENTRIES, ProjectCandidate, ProjectRoster};

/// Default loopback base URL for the trusty-mpm daemon's REST API.
///
/// Why: `crate::serve::mod`'s sibling-port table already reserves 7880 for
/// trusty-mpm; this is that same fact, scoped to this module's one caller.
/// Test: `tests::mpm_daemon_url_defaults_when_env_var_unset`.
pub const DEFAULT_MPM_DAEMON_URL: &str = "http://127.0.0.1:7880";

/// Env var overriding the mpm daemon's base URL.
///
/// Why: the SAME variable `trusty-mpm-gui::state::GuiState` already reads —
/// reusing it (rather than inventing a `tcode`-specific name) means an
/// operator who points one GUI at a non-default mpm daemon does not also
/// have to remember a second env var for this one.
const MPM_DAEMON_URL_ENV: &str = "TRUSTY_MPM_URL";

/// Timeout budget for the registry call.
///
/// Why: this method backs an interactive picker modal — the operator must
/// never be left staring at a spinner because a sibling daemon is wedged
/// rather than simply down. A closed port fails near-instantly (connection
/// refused); this bound only matters for a daemon that accepted the TCP
/// connection but never answers.
const REGISTRY_FETCH_TIMEOUT: Duration = Duration::from_millis(1500);

/// The subset of trusty-mpm's `Project` registry record this module needs.
///
/// Why: a minimal mirror, not the full record
/// (`trusty_mpm::project::record::Project`) — pulling in the whole
/// `trusty-mpm` crate as a dependency just for a struct shape would be a
/// heavier coupling than this thin consumer needs; `name` and `repo_url` are
/// the only fields the merge step reads (see module docs). Extra fields the
/// real response carries (`tags`, `description`, ...) are ignored by serde's
/// default "unknown fields are fine" deserialization.
#[derive(Debug, Clone, Deserialize)]
struct RegistryProject {
    name: String,
    repo_url: String,
}

/// Mirrors `trusty_mpm::daemon::managed_routes::project_registry_routes::
/// ProjectsListResponse`'s wire shape (`{ projects, count }`) — only
/// `projects` is read; `count` is redundant with `projects.len()`.
#[derive(Debug, Deserialize)]
struct RegistryProjectsResponse {
    projects: Vec<RegistryProject>,
}

/// Resolve the mpm daemon's base URL for THIS call.
///
/// Why: split out so [`merged_roster`] stays a one-line "resolve, then
/// delegate" wrapper, matching `super::roster::list_projects`'s own shape.
/// What: `TRUSTY_MPM_URL` if set (trailing slash trimmed), else
/// [`DEFAULT_MPM_DAEMON_URL`].
/// Test: `tests::mpm_daemon_url_defaults_when_env_var_unset`.
fn mpm_daemon_url() -> String {
    std::env::var(MPM_DAEMON_URL_ENV)
        .unwrap_or_else(|_| DEFAULT_MPM_DAEMON_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// `GET {base_url}/api/v1/projects` — the one network call this module
/// makes.
///
/// Why: isolated from [`merge`] so the merge logic is testable without any
/// I/O, and this call is testable without any filesystem setup — see module
/// docs.
/// What: any transport failure, non-2xx status, or malformed body collapses
/// to a single `Err(String)` — the caller ([`merged_roster_with`]) treats
/// every failure mode identically (degrade to fs-only), so there is no value
/// in a typed error here.
/// Test: `tests::merged_roster_with_uses_registry_when_daemon_reachable`,
/// `tests::merged_roster_with_degrades_to_fs_only_when_daemon_unreachable`.
async fn fetch_registry_projects(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Vec<RegistryProject>, String> {
    let url = format!("{base_url}/api/v1/projects");
    let resp = client
        .get(&url)
        .timeout(REGISTRY_FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("mpm daemon returned HTTP {}", resp.status()));
    }
    resp.json::<RegistryProjectsResponse>()
        .await
        .map(|body| body.projects)
        .map_err(|e| e.to_string())
}

/// Parse `(owner, repo)` out of a registry `repo_url`.
///
/// Why: the registry has no local filesystem path (it is keyed on
/// `name`/`repo_url`, not a checkout location) — this recovers the
/// `<owner>/<repo>` pair the workspace's own `~/trusty-mpm-projects/<owner>/
/// <repo>` layout convention needs to resolve a local path (see
/// [`resolve_local_path`]).
/// What: handles both `https://github.com/<owner>/<repo>[.git]` and
/// `git@github.com:<owner>/<repo>[.git]` (the two forms
/// `trusty_mpm::project::record::Project::repo_url`'s own docs give as
/// examples), trimming a trailing slash and `.git` suffix first. Returns
/// `None` for anything that doesn't resolve to a non-empty `owner` and
/// `repo` — a malformed/unexpected `repo_url` shape must never panic or
/// fabricate a bogus path.
/// Test: `tests::parse_owner_repo_handles_https_url`,
/// `tests::parse_owner_repo_handles_https_url_with_git_suffix`,
/// `tests::parse_owner_repo_handles_ssh_url`,
/// `tests::parse_owner_repo_rejects_malformed_url`.
fn parse_owner_repo(repo_url: &str) -> Option<(String, String)> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    // Strip a `<scheme>://` prefix when present (e.g. `https://`); an SSH
    // shorthand URL (`git@host:owner/repo`) has no `://` and is left as-is.
    let path_part = match trimmed.split_once("://") {
        Some((_, rest)) => rest,
        None => trimmed,
    };
    // `path_part` is now `<host>/<owner>/<repo>` (HTTPS) or
    // `git@<host>:<owner>/<repo>` (SSH) — either way, the first `/` or `:`
    // after the host separates it from the `<owner>/<repo>` pair.
    let (_, after_host) = path_part.split_once(['/', ':'])?;
    let (owner, repo) = after_host.rsplit_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Resolve `<home>/trusty-mpm-projects/<owner>/<repo>` to an existing local
/// git-repo checkout, or `None` if it isn't one.
///
/// Why: a registry entry with no local checkout on THIS machine has nothing
/// the picker could bind to — see module docs on why such an entry is
/// skipped, not fabricated as a dead roster row.
/// What: reuses [`super::is_git_repo`] — the identical discriminator
/// `super::roster`'s own scan already filters on, so a registered project is
/// held to the same "must be a real git checkout" bar as a discovered one.
/// Test: `tests::merge_marks_registered_projects_with_local_checkout`,
/// `tests::merge_skips_registry_projects_without_local_checkout`.
fn resolve_local_path(home: &Path, owner: &str, repo: &str) -> Option<PathBuf> {
    let candidate = home.join("trusty-mpm-projects").join(owner).join(repo);
    (candidate.is_dir() && is_git_repo(&candidate)).then_some(candidate)
}

/// Merge the filesystem-scanned roster with registry projects, registry
/// primary.
///
/// Why: the pure(-ish — filesystem existence checks against `home`, no
/// network) core of the design question 2 policy — see module docs for the
/// full rationale. Split out from [`merged_roster_with`] so it is testable
/// with hand-built inputs, no mock server required.
/// What: for each registry project that resolves to an existing local
/// checkout ([`resolve_local_path`]): if a filesystem-scanned entry already
/// has that exact path, it is promoted in place (`registered: true`, `name`
/// and `owner` set from the registry's canonical values); otherwise a new
/// entry is appended with `registered: true`. Filesystem-scanned entries
/// with no matching registry project are kept as-is (`registered: false` —
/// the local-only, unregistered secondary view). `home: None` (no resolvable
/// home directory) short-circuits to the filesystem roster untouched, since
/// no registry project could resolve a local path either. Result is
/// re-sorted case-insensitively by `name` and re-capped at
/// [`MAX_ENTRIES`] (a registered project can newly enter the roster, so the
/// cap must be re-applied after merging, not just inherited from the fs
/// scan).
/// Test: `tests::merge_marks_registered_projects_with_local_checkout`,
/// `tests::merge_marks_unregistered_local_only_projects`,
/// `tests::merge_skips_registry_projects_without_local_checkout`.
fn merge(
    fs_roster: ProjectRoster,
    registry_projects: &[RegistryProject],
    home: Option<&Path>,
) -> ProjectRoster {
    let mut entries = fs_roster.entries;
    if let Some(home) = home {
        let mut path_index: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        for rp in registry_projects {
            let Some((owner, repo)) = parse_owner_repo(&rp.repo_url) else {
                continue;
            };
            let Some(path) = resolve_local_path(home, &owner, &repo) else {
                continue;
            };
            let path_str = path.display().to_string();
            if let Some(&idx) = path_index.get(&path_str) {
                entries[idx].registered = true;
                entries[idx].name = rp.name.clone();
                entries[idx].owner = Some(owner);
            } else {
                let idx = entries.len();
                entries.push(ProjectCandidate {
                    name: rp.name.clone(),
                    path: path_str.clone(),
                    owner: Some(owner),
                    registered: true,
                });
                path_index.insert(path_str, idx);
            }
        }
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries.truncate(MAX_ENTRIES);
    ProjectRoster { entries }
}

/// Fetch the registry (if reachable) and merge it with an already-computed
/// filesystem roster.
///
/// Why: the testable core of [`merged_roster`] — takes `fs_roster`/`home` as
/// plain arguments (rather than re-scanning/re-resolving internally) so
/// tests supply hand-built inputs instead of touching the real filesystem or
/// spawning a real mpm daemon, mirroring `super::roster`'s
/// `list_projects`/`list_projects_in` split.
/// What: on a successful fetch, delegates to [`merge`]. On ANY failure
/// (unreachable daemon, non-2xx, malformed body), logs a `tracing::warn!`
/// and returns `fs_roster` unchanged — already all `registered: false`, i.e.
/// exactly the graceful-degradation view module docs describe.
/// Test: `tests::merged_roster_with_uses_registry_when_daemon_reachable`,
/// `tests::merged_roster_with_degrades_to_fs_only_when_daemon_unreachable`.
async fn merged_roster_with(
    client: &reqwest::Client,
    base_url: &str,
    fs_roster: ProjectRoster,
    home: Option<&Path>,
) -> ProjectRoster {
    match fetch_registry_projects(client, base_url).await {
        Ok(registry_projects) => merge(fs_roster, &registry_projects, home),
        Err(error) => {
            tracing::warn!(
                error = %error,
                mpm_daemon_url = base_url,
                "mpm project registry unavailable; falling back to filesystem-only roster"
            );
            fs_roster
        }
    }
}

/// `fs.list_projects()`'s real data source (issue #3435) — registry primary,
/// filesystem-scan secondary/fallback.
///
/// Why: the one function `super::protocol::list_projects` calls; resolves
/// every real input (a fresh `reqwest::Client`, the mpm daemon URL, the real
/// filesystem scan, the real home directory) so the RPC handler itself stays
/// a one-liner, exactly like `super::roster::list_projects`'s own shape. A
/// fresh `Client` per call (rather than a pooled, shared one) matches this
/// module family's existing "no shared state" design
/// (`super::protocol`'s own module docs) and is appropriate for an
/// interactive, low-frequency, user-triggered call — not a hot path.
/// What: delegates to [`merged_roster_with`].
/// Test: exercised indirectly via `super::protocol::tests::
/// list_projects_returns_entries_array` (this function's own behavior beyond
/// delegation is untestable without touching the real home directory and a
/// real mpm daemon; [`merged_roster_with`] carries the real coverage).
pub async fn merged_roster() -> ProjectRoster {
    merged_roster_with(
        &reqwest::Client::new(),
        &mpm_daemon_url(),
        super::roster::list_projects(),
        dirs::home_dir().as_deref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use axum::extract::State;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;

    fn mkdir(p: &Path) {
        std::fs::create_dir_all(p).expect("mkdir");
    }

    fn mk_git_repo(p: &Path) {
        mkdir(p);
        mkdir(&p.join(".git"));
    }

    // ── `parse_owner_repo` ──────────────────────────────────────────────

    #[test]
    fn parse_owner_repo_handles_https_url() {
        assert_eq!(
            parse_owner_repo("https://github.com/bobmatnyc/trusty-tools"),
            Some(("bobmatnyc".to_string(), "trusty-tools".to_string()))
        );
    }

    #[test]
    fn parse_owner_repo_handles_https_url_with_git_suffix() {
        assert_eq!(
            parse_owner_repo("https://github.com/bobmatnyc/trusty-tools.git"),
            Some(("bobmatnyc".to_string(), "trusty-tools".to_string()))
        );
    }

    #[test]
    fn parse_owner_repo_handles_ssh_url() {
        assert_eq!(
            parse_owner_repo("git@github.com:bobmatnyc/trusty-tools.git"),
            Some(("bobmatnyc".to_string(), "trusty-tools".to_string()))
        );
    }

    #[test]
    fn parse_owner_repo_rejects_malformed_url() {
        assert_eq!(parse_owner_repo("not-a-url"), None);
        assert_eq!(parse_owner_repo(""), None);
    }

    // ── `merge` ──────────────────────────────────────────────────────────

    /// A registry project whose repo_url resolves to an existing local git
    /// checkout must be marked `registered: true` — the registry-primary
    /// merge's core case.
    #[test]
    fn merge_marks_registered_projects_with_local_checkout() {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("trusty-tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(home.path());
        let registry = vec![RegistryProject {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        }];

        let merged = merge(fs_roster, &registry, Some(home.path()));

        assert_eq!(merged.entries.len(), 1);
        assert!(merged.entries[0].registered, "must be marked registered");
        assert_eq!(merged.entries[0].name, "trusty-tools");
        assert_eq!(merged.entries[0].owner.as_deref(), Some("bobmatnyc"));
    }

    /// A filesystem-discovered project with NO matching registry entry must
    /// survive the merge, marked `registered: false` — local-only projects
    /// (e.g. `bakeoff-l1`) must never be silently dropped.
    #[test]
    fn merge_marks_unregistered_local_only_projects() {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("bakeoff-l1"),
        );
        let fs_roster = super::super::roster::list_projects_in(home.path());

        let merged = merge(fs_roster, &[], Some(home.path()));

        assert_eq!(merged.entries.len(), 1);
        assert!(!merged.entries[0].registered, "must stay unregistered");
        assert_eq!(merged.entries[0].name, "bakeoff-l1");
    }

    /// A registry project with NO local checkout on this machine must be
    /// skipped entirely — the picker cannot bind to a path that doesn't
    /// exist, so no dead row is fabricated.
    #[test]
    fn merge_skips_registry_projects_without_local_checkout() {
        let home = tempfile::tempdir().expect("tempdir");
        // No `trusty-mpm-projects` dir at all on this machine.
        let fs_roster = super::super::roster::list_projects_in(home.path());
        let registry = vec![RegistryProject {
            name: "ghost-project".to_string(),
            repo_url: "https://github.com/someone/ghost-project".to_string(),
        }];

        let merged = merge(fs_roster, &registry, Some(home.path()));

        assert!(merged.entries.is_empty());
    }

    // ── `merged_roster_with` (network integration) ──────────────────────

    /// Spin up a one-route mock mpm daemon serving `GET /api/v1/projects`
    /// and return its base URL.
    async fn spawn_mock_mpm(projects: Value) -> String {
        async fn handle(State(projects): State<Value>) -> Json<Value> {
            Json(json!({"projects": projects, "count": 1}))
        }
        let app = Router::new()
            .route("/api/v1/projects", get(handle))
            .with_state(projects);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// When the mpm daemon is reachable, its registry data must be used as
    /// the merge's primary source end-to-end (real HTTP round trip, not just
    /// the pure `merge` unit).
    #[tokio::test]
    async fn merged_roster_with_uses_registry_when_daemon_reachable() {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("trusty-tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(home.path());
        let base_url = spawn_mock_mpm(json!([
            {"name": "trusty-tools", "repo_url": "https://github.com/bobmatnyc/trusty-tools"}
        ]))
        .await;

        let roster = merged_roster_with(
            &reqwest::Client::new(),
            &base_url,
            fs_roster,
            Some(home.path()),
        )
        .await;

        assert_eq!(roster.entries.len(), 1);
        assert!(roster.entries[0].registered);
    }

    /// When the mpm daemon is unreachable, the merge must degrade to the
    /// plain filesystem roster rather than erroring or returning an empty
    /// picker.
    #[tokio::test]
    async fn merged_roster_with_degrades_to_fs_only_when_daemon_unreachable() {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("trusty-tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(home.path());

        // Grab a port and immediately drop the listener so nothing answers —
        // a fast, deterministic "connection refused" rather than a real
        // timeout.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let dead_addr = listener.local_addr().expect("addr");
        drop(listener);
        let dead_url = format!("http://{dead_addr}");

        let roster = merged_roster_with(
            &reqwest::Client::new(),
            &dead_url,
            fs_roster,
            Some(home.path()),
        )
        .await;

        assert_eq!(roster.entries.len(), 1);
        assert!(
            !roster.entries[0].registered,
            "must degrade to fs-only (unregistered) view"
        );
    }

    // ── `mpm_daemon_url` ─────────────────────────────────────────────────

    /// Without `TRUSTY_MPM_URL` set, the default loopback URL is used.
    ///
    /// Why serial-unsafe env mutation is acceptable here: this crate's test
    /// binary runs each `#[test]`/`#[tokio::test]` in its own thread but
    /// shares process-wide env state; this test only READS the env var
    /// (never sets it), so it cannot race a hypothetical other test that
    /// does set it, and no other test in this module touches
    /// `TRUSTY_MPM_URL`.
    #[test]
    fn mpm_daemon_url_defaults_when_env_var_unset() {
        // Best-effort: only assert when the var is genuinely unset in this
        // process, so this test cannot flake under an operator's shell that
        // happens to export it.
        if std::env::var(MPM_DAEMON_URL_ENV).is_ok() {
            return;
        }
        assert_eq!(mpm_daemon_url(), DEFAULT_MPM_DAEMON_URL);
    }
}
