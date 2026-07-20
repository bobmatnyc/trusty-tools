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
//! already documents. Critically, this degraded roster is
//! INDISTINGUISHABLE from "the registry has nothing registered" by its
//! entries alone — the #3363 lesson is that silently collapsing "empty" and
//! "unreachable" into the same shape hides a real outage from the operator.
//! [`super::roster::ProjectRoster::source`] (`RosterSource::Registry` vs.
//! `RosterSource::FsOnly`, code-critic PR #3439 review, HIGH 2) is the
//! caller-visible signal that tells the two apart; the GUI renders a banner
//! when `source` is `fs_only`.
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
use super::roster::{MAX_ENTRIES, ProjectCandidate, ProjectRoster, RosterSource};

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
///
/// **Only an EXACT `<owner>/<repo>` pair is supported** (code-critic PR
/// #3439 review, MEDIUM 1): a compound remainder after the host — a GitLab
/// subgroup path (`gitlab.com/group/subgroup/repo`) or a port-qualified host
/// that this function's simple `split_once(['/', ':'])` mis-splits (e.g.
/// `git.example.com:8443/owner/repo` splits on the PORT's `:`, leaving
/// `8443/owner/repo`) — is rejected outright rather than silently absorbed
/// into a multi-segment "owner" via `rsplit_once`. Letting that through
/// would hand [`resolve_local_path`]'s `Path::join` an owner string
/// containing `/`, fabricating a 3+-level lookup path no `repo_url` of this
/// shape was ever meant to produce.
/// Test: `tests::parse_owner_repo_handles_https_url`,
/// `tests::parse_owner_repo_handles_https_url_with_git_suffix`,
/// `tests::parse_owner_repo_handles_ssh_url`,
/// `tests::parse_owner_repo_rejects_malformed_url`,
/// `tests::parse_owner_repo_rejects_gitlab_style_subgroup_path`,
/// `tests::parse_owner_repo_rejects_port_qualified_host_misparse`.
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
    // Reject anything but an EXACT `<owner>/<repo>` pair — see the docs
    // above for why a compound remainder must not fall through to
    // `rsplit_once` below.
    if after_host.matches('/').count() != 1 {
        return None;
    }
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

/// Normalize `path` into the string [`merge`] indexes and compares on.
///
/// Why (code-critic PR #3439 review, HIGH 1): the fs scan's `path` string
/// carries whatever casing `std::fs::read_dir` reported for the on-disk
/// entry, while [`resolve_local_path`]'s candidate carries whatever casing
/// the registry's `repo_url` happened to use. On a case-insensitive,
/// case-preserving filesystem (default macOS APFS — the dev platform)
/// `Path::is_dir` succeeds for BOTH castings of the identical directory, so
/// exact-string equality between the two would miss the match and double
/// the row: one `registered: false` (from the fs scan) and one
/// `registered: true` (freshly appended from the registry) for the SAME
/// checkout — precisely what DOC-39 §5.8.1 AC-20.7/AC-20.8 forbid. The same
/// mismatch can also happen via symlink indirection (a registry-derived
/// path reached through a symlinked alias of `home`) on ANY platform.
/// What: `std::fs::canonicalize` resolves both symlinks and (on the
/// platforms where the kernel's `realpath` does so, including macOS) case,
/// collapsing every string representation of one real directory to the
/// SAME canonical string. Degrades to `path` verbatim if canonicalization
/// fails (a TOCTOU race, or a permission refusal) — matching this module's
/// best-effort philosophy rather than dropping a candidate over a
/// transient stat failure.
/// Test: `tests::merge_dedupes_paths_reached_via_a_symlinked_home_alias`,
/// `tests::merge_dedupes_case_differing_paths_on_case_insensitive_filesystems`.
fn normalize_for_comparison(path: &Path) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
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
/// resolves ([`normalize_for_comparison`]) to that SAME real directory, it is
/// promoted in place (`registered: true`, `name` and `owner` set from the
/// registry's canonical values); otherwise a new entry is appended with
/// `registered: true`, its `path` the normalized (canonical) string.
/// Filesystem-scanned entries with no matching registry project are kept
/// as-is (`registered: false` — the local-only, unregistered secondary
/// view). `home: None` (no resolvable home directory) short-circuits to the
/// filesystem roster untouched, since no registry project could resolve a
/// local path either. Result is re-sorted case-insensitively by `name`,
/// re-capped at [`MAX_ENTRIES`] (a registered project can newly enter the
/// roster, so the cap must be re-applied after merging, not just inherited
/// from the fs scan), and stamped [`RosterSource::Registry`] — this
/// function is only ever called after a successful registry fetch (see
/// [`merged_roster_with`]).
/// Test: `tests::merge_marks_registered_projects_with_local_checkout`,
/// `tests::merge_marks_unregistered_local_only_projects`,
/// `tests::merge_skips_registry_projects_without_local_checkout`,
/// `tests::merge_dedupes_paths_reached_via_a_symlinked_home_alias`,
/// `tests::merge_dedupes_case_differing_paths_on_case_insensitive_filesystems`.
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
            .map(|(i, e)| (normalize_for_comparison(Path::new(&e.path)), i))
            .collect();
        for rp in registry_projects {
            let Some((owner, repo)) = parse_owner_repo(&rp.repo_url) else {
                continue;
            };
            let Some(path) = resolve_local_path(home, &owner, &repo) else {
                continue;
            };
            let normalized = normalize_for_comparison(&path);
            if let Some(&idx) = path_index.get(&normalized) {
                entries[idx].registered = true;
                entries[idx].name = rp.name.clone();
                entries[idx].owner = Some(owner);
            } else {
                let idx = entries.len();
                entries.push(ProjectCandidate {
                    name: rp.name.clone(),
                    path: normalized.clone(),
                    owner: Some(owner),
                    registered: true,
                });
                path_index.insert(normalized, idx);
            }
        }
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries.truncate(MAX_ENTRIES);
    ProjectRoster {
        entries,
        source: RosterSource::Registry,
    }
}

/// Fetch the registry (if reachable) and merge it with an already-computed
/// filesystem roster.
///
/// Why: the testable core of [`merged_roster`] — takes `fs_roster`/`home` as
/// plain arguments (rather than re-scanning/re-resolving internally) so
/// tests supply hand-built inputs instead of touching the real filesystem or
/// spawning a real mpm daemon, mirroring `super::roster`'s
/// `list_projects`/`list_projects_in` split.
/// What: on a successful fetch, delegates to [`merge`] (stamps
/// `RosterSource::Registry`). On ANY failure (unreachable daemon, non-2xx,
/// malformed body), logs a `tracing::warn!` and returns `fs_roster`
/// unchanged — already all `registered: false` AND already
/// `RosterSource::FsOnly` (`super::roster::list_projects_in` sets it), i.e.
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

    /// A GitLab-style subgroup path (`group/subgroup/repo`) must be
    /// rejected, not absorbed into a bogus multi-segment "owner" (code-critic
    /// PR #3439 review, MEDIUM 1).
    #[test]
    fn parse_owner_repo_rejects_gitlab_style_subgroup_path() {
        assert_eq!(
            parse_owner_repo("https://gitlab.com/group/subgroup/repo"),
            None
        );
    }

    /// A port-qualified host (`host:PORT/owner/repo`) must be rejected —
    /// this function's simple `split_once(['/', ':'])` splits on the PORT's
    /// `:`, not a host/path boundary, so without the compound-remainder
    /// guard this would silently fabricate `owner = "PORT/owner"` (code-critic
    /// PR #3439 review, MEDIUM 1).
    #[test]
    fn parse_owner_repo_rejects_port_qualified_host_misparse() {
        assert_eq!(
            parse_owner_repo("https://git.example.com:8443/owner/repo"),
            None
        );
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

    /// Best-effort detection of a case-insensitive, case-preserving
    /// filesystem (default macOS APFS, NTFS) — used to gate the case-folding
    /// regression test below, which can only reproduce on such a volume.
    fn fs_is_case_insensitive(dir: &Path) -> bool {
        let probe = dir.join("CaseProbeDir3435");
        if std::fs::create_dir(&probe).is_err() {
            return false;
        }
        let insensitive = dir.join("caseprobedir3435").is_dir();
        let _ = std::fs::remove_dir(&probe);
        insensitive
    }

    /// Code-critic PR #3439 review, HIGH 1: on a case-insensitive,
    /// case-preserving filesystem, a fs-scanned entry's on-disk casing and
    /// the registry-derived candidate's `repo_url`-sourced casing must still
    /// dedupe to exactly ONE roster row, not two. Gated to actually run only
    /// when the temp filesystem is case-insensitive (CI's Linux ext4
    /// typically is not); `merge_dedupes_paths_reached_via_a_symlinked_home_alias`
    /// below covers the same `normalize_for_comparison` mechanism in a way
    /// that reproduces on every platform.
    #[test]
    fn merge_dedupes_case_differing_paths_on_case_insensitive_filesystems() {
        let home = tempfile::tempdir().expect("tempdir");
        if !fs_is_case_insensitive(home.path()) {
            return;
        }

        // The fs scan observes the on-disk casing "Trusty-Tools" — same
        // characters as the registry's "trusty-tools" below, differing ONLY
        // in case (not, e.g., hyphenation — a hyphen is not a case fold).
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("Trusty-Tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(home.path());
        assert_eq!(fs_roster.entries.len(), 1, "fs scan must find the repo");

        // The registry's repo_url carries a DIFFERENT casing for the same repo.
        let registry = vec![RegistryProject {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        }];

        let merged = merge(fs_roster, &registry, Some(home.path()));

        assert_eq!(
            merged.entries.len(),
            1,
            "must dedupe to exactly one row, not two (DOC-39 AC-20.7/20.8)"
        );
        assert!(merged.entries[0].registered);
    }

    /// Code-critic PR #3439 review, HIGH 1: a portable (OS-independent)
    /// equivalent of the case-folding scenario above. Filesystem case
    /// sensitivity varies by platform, but symlink resolution does not: the
    /// fs scan observes the repo under the REAL home dir, while `merge` is
    /// given a symlinked ALIAS of that same home dir — two different path
    /// strings, one real directory. Exact-string dedup (pre-fix) would
    /// double the row; canonicalizing (post-fix) must not.
    #[cfg(unix)]
    #[test]
    fn merge_dedupes_paths_reached_via_a_symlinked_home_alias() {
        let real_home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &real_home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("trusty-tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(real_home.path());
        assert_eq!(fs_roster.entries.len(), 1, "fs scan must find the repo");

        let parent = real_home.path().parent().expect("tempdir has a parent");
        let alias = parent.join(format!(
            "alias-of-{}",
            real_home
                .path()
                .file_name()
                .expect("tempdir has a name")
                .to_string_lossy()
        ));
        std::os::unix::fs::symlink(real_home.path(), &alias).expect("symlink alias");

        let registry = vec![RegistryProject {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        }];

        // `merge` resolves the registry entry against the ALIAS, not the
        // real path the fs scan used.
        let merged = merge(fs_roster, &registry, Some(&alias));
        let _ = std::fs::remove_file(&alias);

        assert_eq!(
            merged.entries.len(),
            1,
            "must dedupe to exactly one row, not two (DOC-39 AC-20.7/20.8)"
        );
        assert!(merged.entries[0].registered);
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
        assert_eq!(
            roster.source,
            RosterSource::Registry,
            "a reachable registry must be signaled via `source` (code-critic PR #3439, HIGH 2)"
        );
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
        assert_eq!(
            roster.source,
            RosterSource::FsOnly,
            "an unreachable registry must be signaled via `source`, distinct from \
             'the registry legitimately has nothing registered' (code-critic PR #3439, HIGH 2)"
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
