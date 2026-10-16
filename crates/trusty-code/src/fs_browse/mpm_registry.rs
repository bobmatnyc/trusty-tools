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
//! call to the mpm daemon's registry — NOT a direct read of `projects.json`,
//! because there is no workspace precedent for one daemon reading a sibling
//! daemon's on-disk store. #6288 PR 1b moved the call from loopback HTTP
//! (`GET http://127.0.0.1:7880/api/v1/projects`, `TRUSTY_MPM_URL`) onto the
//! daemon's Unix socket: `mpm.projects.registry.list`, the same route served
//! as a JSON-RPC method, on the socket `trusty_common::mpm_rpc::mpm_socket`
//! resolves — honouring `TRUSTY_MPM_SOCKET`, the one knob every mpm client
//! reads. PR 1a moved every tm client there with no TCP fallback, and PR 2
//! retires the daemon's 7880 bind.
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
//! (`super::protocol::list_projects`) calls — it resolves the mpm daemon socket,
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
use serde_json::json;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use super::is_git_repo;
use super::roster::{MAX_ENTRIES, ProjectCandidate, ProjectRoster, RosterSource};

/// The registry method: `GET /api/v1/projects`, served on the daemon socket
/// (#6288; `trusty-mpm`'s `daemon::rpc::registry::projects`).
const METHOD_REGISTRY_LIST: &str = "mpm.projects.registry.list";

/// Timeout budget for the registry call.
///
/// Why: this method backs an interactive picker modal — the operator must
/// never be left staring at a spinner because a sibling daemon is wedged
/// rather than simply down. An absent socket fails near-instantly; this
/// bound only matters for a daemon that accepted the connection but never
/// answers.
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

/// `mpm.projects.registry.list` on `socket` — the one daemon call this module
/// makes.
///
/// Why: isolated from [`merge`] so the merge logic is testable without any
/// I/O, and this call is testable without any filesystem setup — see module
/// docs.
/// What: a dial failure, a timeout, a JSON-RPC error frame, or a result that
/// does not decode collapses to a single `Err(String)` — the caller
/// ([`merged_roster_with`]) treats every failure mode identically (degrade to
/// fs-only, `RosterSource::FsOnly`), so there is no value in a typed error.
/// Test: `tests::merged_roster_with_uses_registry_when_daemon_reachable`,
/// `tests::merged_roster_with_degrades_to_fs_only_when_daemon_unreachable`,
/// `tests::merged_roster_with_degrades_when_the_daemon_answers_an_error`.
async fn fetch_registry_projects(socket: &Path) -> Result<Vec<RegistryProject>, String> {
    let request = json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": 1,
        "method": METHOD_REGISTRY_LIST,
        "params": {},
    });
    let response: RpcResponse = send_framed_request(socket, &request, REGISTRY_FETCH_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    match (response.result, response.error) {
        (_, Some(e)) => Err(format!(
            "{METHOD_REGISTRY_LIST} answered error {}: {}",
            e.code, e.message
        )),
        (Some(result), None) => serde_json::from_value::<RegistryProjectsResponse>(result)
            .map(|body| body.projects)
            .map_err(|e| e.to_string()),
        (None, None) => Err(format!(
            "{METHOD_REGISTRY_LIST} answered neither a result nor an error"
        )),
    }
}

/// Parse `(owner, repo)` out of a registry `repo_url`.
///
/// Why: a registry entry names a project by its `repo_url`, and the picker
/// needs the `<owner>/<repo>` pair the workspace's own
/// `~/trusty-mpm-projects/<owner>/<repo>` layout convention takes to resolve a
/// local path (see [`resolve_local_path`]).
/// What: delegates to `trusty_common::github_path::parse_remote_url`, the
/// workspace's one git-remote-URL parser (#6657). It accepts the HTTPS and SSH
/// forms `trusty_mpm::project::record::Project::repo_url`'s own docs give as
/// examples, trims a trailing slash and `.git`, and returns `None` for anything
/// that does not resolve to a non-empty `owner` and `repo` — a malformed or
/// unexpected `repo_url` shape must never panic or fabricate a bogus path.
///
/// **Only an EXACT `<owner>/<repo>` pair is supported** (code-critic PR
/// #3439 review, MEDIUM 1): a compound remainder after the host — a GitLab
/// subgroup path (`gitlab.com/group/subgroup/repo`) — is rejected outright
/// rather than absorbed into a multi-segment "owner". Letting that through
/// would hand [`resolve_local_path`]'s `Path::join` an owner string containing
/// `/`, fabricating a 3+-level lookup path no `repo_url` of this shape was ever
/// meant to produce. A port-qualified host (`git.example.com:8443/owner/repo`)
/// used to be rejected for the same reason, because this function's own
/// `split_once(['/', ':'])` split on the PORT's colon; the shared parser splits
/// the authority correctly, so that URL now yields the pair it names.
/// Test: `tests::parse_owner_repo_handles_https_url`,
/// `tests::parse_owner_repo_handles_https_url_with_git_suffix`,
/// `tests::parse_owner_repo_handles_ssh_url`,
/// `tests::parse_owner_repo_rejects_malformed_url`,
/// `tests::parse_owner_repo_rejects_gitlab_style_subgroup_path`,
/// `tests::parse_owner_repo_handles_port_qualified_host`.
fn parse_owner_repo(repo_url: &str) -> Option<(String, String)> {
    let remote = trusty_common::github_path::parse_remote_url(repo_url).ok()?;
    Some((remote.owner, remote.repo))
}

/// Resolve `<workspace_root>/<owner>/<repo>` to an existing local git-repo
/// checkout, or `None` if it isn't one.
///
/// Why: a registry entry with no local checkout on THIS machine has nothing
/// the picker could bind to — see module docs on why such an entry is
/// skipped, not fabricated as a dead roster row.
/// What: reuses [`super::is_git_repo`] — the identical discriminator
/// `super::roster`'s own scan already filters on, so a registered project is
/// held to the same "must be a real git checkout" bar as a discovered one.
///
/// #5203 changed the first parameter from `home` to the RESOLVED workspace
/// root. Joining `trusty-mpm-projects` onto `home` here was the second half of
/// the bug: with a retargeted root every registered project failed this
/// existence check and was silently dropped from the picker.
/// Test: `tests::merge_marks_registered_projects_with_local_checkout`,
/// `tests::merge_skips_registry_projects_without_local_checkout`.
fn resolve_local_path(workspace_root: &Path, owner: &str, repo: &str) -> Option<PathBuf> {
    let candidate = workspace_root.join(owner).join(repo);
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
    workspace_root: Option<&Path>,
) -> ProjectRoster {
    let mut entries = fs_roster.entries;
    // #5203: resolve registry entries under the CONFIGURED workspace root.
    if let Some(workspace_root) = workspace_root {
        let mut path_index: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (normalize_for_comparison(Path::new(&e.path)), i))
            .collect();
        for rp in registry_projects {
            let Some((owner, repo)) = parse_owner_repo(&rp.repo_url) else {
                continue;
            };
            let Some(path) = resolve_local_path(workspace_root, &owner, &repo) else {
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
/// `RosterSource::Registry`). On ANY failure (absent or refused socket, an
/// error answer, a malformed body), logs a `tracing::warn!` and returns `fs_roster`
/// unchanged — already all `registered: false` AND already
/// `RosterSource::FsOnly` (`super::roster::list_projects_in` sets it), i.e.
/// exactly the graceful-degradation view module docs describe.
/// Test: `tests::merged_roster_with_uses_registry_when_daemon_reachable`,
/// `tests::merged_roster_with_degrades_to_fs_only_when_daemon_unreachable`,
/// `tests::merged_roster_with_degrades_when_the_daemon_answers_an_error`.
async fn merged_roster_with(
    socket: &Path,
    fs_roster: ProjectRoster,
    workspace_root: Option<&Path>,
) -> ProjectRoster {
    match fetch_registry_projects(socket).await {
        Ok(registry_projects) => merge(fs_roster, &registry_projects, workspace_root),
        Err(error) => {
            tracing::warn!(
                error = %error,
                mpm_socket = %socket.display(),
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
/// every real input (the mpm daemon socket, the real filesystem scan, the
/// real home directory) so the RPC handler itself stays a one-liner,
/// exactly like `super::roster::list_projects`'s own shape.
/// What: delegates to [`merged_roster_with`].
/// Test: exercised indirectly via `super::protocol::tests::
/// list_projects_returns_entries_array` (this function's own behavior beyond
/// delegation is untestable without touching the real home directory and a
/// real mpm daemon; [`merged_roster_with`] carries the real coverage).
pub async fn merged_roster() -> ProjectRoster {
    merged_roster_with(
        // #6288: the daemon socket, as every mpm client resolves it.
        &trusty_common::mpm_rpc::mpm_socket(),
        super::roster::list_projects(),
        // #5203: the same resolver `roster::list_projects` scans with.
        Some(trusty_common::workspace_layout::workspace_root().as_path()),
    )
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use trusty_common::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};

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

    /// A port-qualified host (`host:PORT/owner/repo`) must yield the pair the
    /// URL names. The local parser this replaced split on the PORT's `:` and
    /// so had to reject the whole shape to avoid fabricating
    /// `owner = "PORT/owner"` (code-critic PR #3439 review, MEDIUM 1); the
    /// shared parser splits the authority correctly, and the invariant that
    /// guard protected — an owner never containing `/` — still holds, proved
    /// by the subgroup case above.
    #[test]
    fn parse_owner_repo_handles_port_qualified_host() {
        assert_eq!(
            parse_owner_repo("https://git.example.com:8443/owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
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
        let fs_roster = super::super::roster::list_projects_in(
            home.path(),
            &home.path().join("trusty-mpm-projects"),
        );
        let registry = vec![RegistryProject {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        }];

        let merged = merge(
            fs_roster,
            &registry,
            Some(&home.path().join("trusty-mpm-projects")),
        );

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
        let fs_roster = super::super::roster::list_projects_in(
            home.path(),
            &home.path().join("trusty-mpm-projects"),
        );

        let merged = merge(
            fs_roster,
            &[],
            Some(&home.path().join("trusty-mpm-projects")),
        );

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
        let fs_roster = super::super::roster::list_projects_in(
            home.path(),
            &home.path().join("trusty-mpm-projects"),
        );
        let registry = vec![RegistryProject {
            name: "ghost-project".to_string(),
            repo_url: "https://github.com/someone/ghost-project".to_string(),
        }];

        let merged = merge(
            fs_roster,
            &registry,
            Some(&home.path().join("trusty-mpm-projects")),
        );

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
        let fs_roster = super::super::roster::list_projects_in(
            home.path(),
            &home.path().join("trusty-mpm-projects"),
        );
        assert_eq!(fs_roster.entries.len(), 1, "fs scan must find the repo");

        // The registry's repo_url carries a DIFFERENT casing for the same repo.
        let registry = vec![RegistryProject {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
        }];

        let merged = merge(
            fs_roster,
            &registry,
            Some(&home.path().join("trusty-mpm-projects")),
        );

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
        let fs_roster = super::super::roster::list_projects_in(
            real_home.path(),
            &real_home.path().join("trusty-mpm-projects"),
        );
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
        let merged = merge(
            fs_roster,
            &registry,
            Some(&alias.join("trusty-mpm-projects")),
        );
        let _ = std::fs::remove_file(&alias);

        assert_eq!(
            merged.entries.len(),
            1,
            "must dedupe to exactly one row, not two (DOC-39 AC-20.7/20.8)"
        );
        assert!(merged.entries[0].registered);
    }

    // ── `merged_roster_with` (socket integration) ───────────────────────

    /// A mock mpm daemon on a temp socket answering
    /// `mpm.projects.registry.list` with `answer`. Returns the socket path and
    /// the temp dir that holds it.
    fn spawn_mock_mpm(answer: Result<Value, RpcError>) -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("socket dir");
        let socket = dir.path().join("s").join("trusty-mpm.sock");
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("mkdir");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let router = RpcRouter::new().typed(METHOD_REGISTRY_LIST, move |_params: Value| {
            let answer = answer.clone();
            async move { answer }
        });
        tokio::spawn(async move {
            serve_until(
                &listener,
                std::sync::Arc::new(router),
                RpcServeOptions::default(),
                std::future::pending::<()>(),
            )
            .await;
        });
        (socket, dir)
    }

    /// A one-checkout fs roster under a temp home, plus that home.
    fn one_checkout_roster() -> (ProjectRoster, tempfile::TempDir) {
        let home = tempfile::tempdir().expect("tempdir");
        mk_git_repo(
            &home
                .path()
                .join("trusty-mpm-projects")
                .join("bobmatnyc")
                .join("trusty-tools"),
        );
        let fs_roster = super::super::roster::list_projects_in(
            home.path(),
            &home.path().join("trusty-mpm-projects"),
        );
        (fs_roster, home)
    }

    /// When the mpm daemon answers on its socket, its registry data must be
    /// used as the merge's primary source end-to-end (#6288: a real socket
    /// round trip, not just the pure `merge` unit).
    #[tokio::test]
    async fn merged_roster_with_uses_registry_when_daemon_reachable() {
        let (fs_roster, home) = one_checkout_roster();
        let (socket, _dir) = spawn_mock_mpm(Ok(json!({
            "projects": [
                {"name": "trusty-tools", "repo_url": "https://github.com/bobmatnyc/trusty-tools"}
            ],
            "count": 1,
        })));

        let roster = merged_roster_with(
            &socket,
            fs_roster,
            Some(&home.path().join("trusty-mpm-projects")),
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

    /// When no daemon serves the socket, the merge must degrade to the plain
    /// filesystem roster rather than erroring or returning an empty picker.
    #[tokio::test]
    async fn merged_roster_with_degrades_to_fs_only_when_daemon_unreachable() {
        let (fs_roster, home) = one_checkout_roster();
        let absent = home.path().join("absent.sock");

        let roster = merged_roster_with(
            &absent,
            fs_roster,
            Some(&home.path().join("trusty-mpm-projects")),
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

    /// #6288 Fail-Open Check: a daemon that answers with an error frame is not
    /// a registry answer. The roster must degrade and say so (`FsOnly`), never
    /// report `Registry` over an empty list.
    #[tokio::test]
    async fn merged_roster_with_degrades_when_the_daemon_answers_an_error() {
        let (fs_roster, home) = one_checkout_roster();
        let (socket, _dir) = spawn_mock_mpm(Err(RpcError::internal("registry lock poisoned")));

        let roster = merged_roster_with(
            &socket,
            fs_roster,
            Some(&home.path().join("trusty-mpm-projects")),
        )
        .await;

        assert_eq!(roster.source, RosterSource::FsOnly);
        assert!(!roster.entries[0].registered);
    }
}
