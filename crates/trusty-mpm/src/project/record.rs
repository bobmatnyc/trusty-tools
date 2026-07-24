//! Project record type for the project registry.
//!
//! Why: the project registry needs a canonical, serializable representation of
//! every known project so it can survive daemon restarts and be exchanged over
//! MCP tools without ambiguity.
//! What: defines [`Project`] — the persisted project record — with all fields
//! needed to identify a project and seed session spawns.
//! Test: serde round-trips are verified in `project_serde_round_trip`; missing
//! optional fields default correctly in `project_without_optionals`.

use serde::{Deserialize, Serialize};

use crate::core::trusty_tools_config::GithubConfig;

/// A project entry in the project registry.
///
/// Why: the registry tracks repositories an operator works with so that session
/// spawns can reference them by name and auto-registration from session history
/// can populate the registry without operator effort.
/// What: captures the project name (registry key), repository URL,
/// default branch, and optional descriptive metadata.
/// Test: `project_serde_round_trip`, `project_without_optionals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Short name used as the registry key (e.g. `trusty-tools`).
    ///
    /// Why: a human-readable stable key enables lookups by name rather than by
    /// repo URL, and matches the convention of deriving names from repo basenames.
    /// What: must be non-empty; upsert is keyed on this field.
    pub name: String,

    /// Full repository URL (e.g. `https://github.com/owner/trusty-tools`).
    ///
    /// Why: the session spawner needs the URL to clone a workspace; the registry
    /// provides it so callers do not have to supply it on every `session_new`.
    pub repo_url: String,

    /// The project's default branch (e.g. `main` or `develop`).
    ///
    /// Why: session spawns default to this branch when no explicit ref is given,
    /// reducing the caller's boilerplate.
    pub default_branch: String,

    /// Optional technology-stack hint for the project (e.g. `rust`, `python`).
    ///
    /// Why: agents that auto-configure their tooling can use this hint to select
    /// the right skill set without introspecting the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_hint: Option<String>,

    /// Classification tags for the project (e.g. `["backend", "production"]`).
    ///
    /// Why: tags let operators filter the project list and let agents select
    /// projects by domain without parsing descriptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Free-form human-readable description of the project.
    ///
    /// Why: helps operators identify a project from the registry list, especially
    /// when the name alone is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Preferred GitHub account login for `gh` operations on this project (#2081).
    ///
    /// Why: a host may have several `gh`-authenticated accounts; the operator
    /// who has admin rights on THIS project's repo may not be the ambient
    /// active `gh` account. #2081's motivating incident: a delegated agent's
    /// active account (`bob-duetto`) lacked admin rights on
    /// `bobmatnyc/trusty-tools`, so `gh pr merge --admin` failed until the
    /// account was switched by hand. Storing the preferred login on the
    /// project record lets callers resolve a per-invocation, non-mutating `gh`
    /// identity for it (see [`crate::core::gh_account::resolve_gh_account_env`])
    /// instead of relying on a per-session reminder.
    /// What: `None` → no preference; `gh` calls made for this project use
    /// whatever identity is ambient (no regression). `Some(login)` → the
    /// login callers should scope `gh` operations to, resolved WITHOUT
    /// mutating the shared `gh auth switch` state (concurrency-safe: two
    /// projects with different `gh_user` values can run `gh` calls at the
    /// same time without interfering with each other).
    /// Test: `project_serde_round_trip`, `project_without_optionals`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_user: Option<String>,

    /// GitHub account login pinned for THIS project's spawned sessions (#3025).
    ///
    /// Why: an operator with several `gh`-authenticated accounts on one host
    /// (e.g. a personal account and a work account) is bitten when `gh auth
    /// switch` — a single GLOBAL pointer — flips under a concurrently-running
    /// session in a different project tree, breaking `gh pr merge --admin` and
    /// similar calls until the account is switched back by hand (the
    /// motivating 2026-07-18 incident). Unlike `gh_user` (#2081, which only
    /// documents intent for the non-mutating `ensure_gh_account_for_project`
    /// enforcement path and requires an already-isolated `GH_CONFIG_DIR`),
    /// `gh_account` drives spawn-time `GH_TOKEN` minting: [`crate::core::
    /// gh_account::resolve_gh_account_env`] runs `gh auth token -u
    /// <gh_account>` and the resolved token/login are injected directly into
    /// the spawned session's environment, so every `gh` call the session
    /// makes (including from a Claude Code Bash tool child process) is
    /// deterministic regardless of the ambient/global `gh auth switch` state.
    /// Only the ACCOUNT NAME is ever persisted here — never a token.
    /// What: `None` → no pinning; sessions spawned for this project inherit
    /// whatever `gh` identity is ambient (no regression). `Some(login)` →
    /// resolved at every spawn/relaunch via `gh auth token -u <login>`; a
    /// resolution failure (gh missing, account not logged in) is logged as a
    /// warning and the spawn proceeds WITHOUT `GH_TOKEN` rather than failing.
    /// Test: `project_serde_round_trip`, `project_without_optionals`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_account: Option<String>,

    /// Per-project `gh`-CLI identity binding, mirrored from
    /// [`crate::core::trusty_tools_config::ProjectConfig::github`] (#2184).
    ///
    /// Why: `seed_from_config` upserts static `projects:` entries into the
    /// registry; carrying the binding onto the registry record lets
    /// `project_get`/`project_list` surface it via MCP without a separate
    /// config-file read. Resolution (project > global > ambient) is applied by
    /// [`crate::core::gh_identity::select_github_config`], not here.
    /// What: `None` → no per-project override (falls back to the global
    /// `github:` section, then ambient). `Some(cfg)` → the full `gh` identity
    /// binding for this project.
    /// Test: `project_serde_round_trip`, `project_without_optionals`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubConfig>,

    /// Preferred git commit author name for this project, mirrored from
    /// [`crate::core::trusty_tools_config::ProjectConfig::commit_name`] (#2184).
    /// `None` → no override; applied via
    /// [`crate::core::git_identity::GitIdentity::commit_config_args`] when set.
    /// Test: `project_serde_round_trip`, `project_without_optionals`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_name: Option<String>,

    /// Preferred git commit author email for this project, mirrored from
    /// [`crate::core::trusty_tools_config::ProjectConfig::commit_email`]
    /// (#2184). See [`Self::commit_name`].
    /// Test: `project_serde_round_trip`, `project_without_optionals`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_email: Option<String>,

    /// Whether managed sessions for this project provision an isolated
    /// per-session git worktree (#3455 — "launch on main" opt-out).
    ///
    /// Why: `tm` unconditionally provisions a `.worktrees/<session>` git
    /// worktree on a `session/<name>` branch for every managed session,
    /// which is exactly wrong for a project with a direct-main workflow (no
    /// PR flow, no isolation requirement) — the worktree sits unused at a
    /// stale commit, the session's cwd points away from the real checkout,
    /// and agents must `cd` out of it on every command.
    /// What: `None`/`Some(true)` → default, no regression: every session
    /// gets its own worktree exactly as before. `Some(false)` → the matched
    /// project's managed sessions run directly in the resolved local
    /// checkout instead — no clone, no worktree, no dedicated branch. See
    /// [`Self::worktree_enabled`] for the resolved-default accessor and
    /// `daemon::managed_routes::lifecycle::spawn_managed_on_main` for the
    /// runtime effect. **Caveat**: unlike a worktree, the live checkout is
    /// NOT isolated — running more than one session against it concurrently
    /// is a real hazard (uncommitted-change races, concurrent git
    /// operations); the spawn path warns (never refuses) when a second
    /// Active session targets the same checkout.
    /// Test: `project_serde_round_trip`, `project_without_optionals`,
    /// `worktree_enabled_defaults_true`, `worktree_enabled_honors_false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
}

impl Project {
    /// Resolve this project's effective worktree-isolation setting (#3455).
    ///
    /// Why: the single place callers ask "should THIS project's sessions get
    /// a per-session worktree?" so the `None` → `true` default (no
    /// regression) is asserted in exactly one spot.
    /// What: `self.worktree.unwrap_or(true)`.
    /// Test: `worktree_enabled_defaults_true`, `worktree_enabled_honors_false`.
    pub fn worktree_enabled(&self) -> bool {
        self.worktree.unwrap_or(true)
    }
}

/// Compare two repository URLs for identity, tolerating scheme/`.git`/
/// trailing-slash differences (#2184).
///
/// Why: `resolve_for_config`'s project lookup must match
/// `https://github.com/owner/repo`, `https://github.com/owner/repo.git`, and
/// `git@github.com:owner/repo.git` as the SAME project — operators write
/// `repo_url` in whichever form is convenient, and the URL `spawn_managed`
/// receives (or the CLI's detected origin) may use a different form than the
/// one declared in `config.projects`.
/// What: parses both sides via [`trusty_common::github_path::parse_github_path`]
/// and compares the slugified `owner`/`repo` case-insensitively when BOTH
/// parse; falls back to a normalised (trailing-slash- and `.git`-stripped)
/// exact string comparison when either side fails to parse (e.g. a bare local
/// path), so non-GitHub-shaped inputs still get a sensible exact-match result
/// instead of silently never matching.
/// Test: `repo_url_matches_https_vs_git_suffix`, `repo_url_matches_ssh_form`,
/// `repo_url_matches_case_insensitive`, `repo_url_matches_rejects_different_repo`,
/// `repo_url_matches_falls_back_to_string_compare`.
pub fn repo_url_matches(a: &str, b: &str) -> bool {
    match (
        trusty_common::github_path::parse_github_path(a),
        trusty_common::github_path::parse_github_path(b),
    ) {
        (Some(pa), Some(pb)) => {
            pa.owner.eq_ignore_ascii_case(&pb.owner) && pa.repo.eq_ignore_ascii_case(&pb.repo)
        }
        _ => normalise_repo_url(a) == normalise_repo_url(b),
    }
}

/// Normalise a repo URL for the exact-string fallback in [`repo_url_matches`].
///
/// Why: isolated so the fallback rule (trim trailing `/`, strip trailing
/// `.git`, lowercase) is a single tested transform.
/// What: returns a lowercased copy with a trailing `/` and `.git` suffix
/// stripped.
/// Test: covered via `repo_url_matches_falls_back_to_string_compare`.
fn normalise_repo_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase()
}

/// Derive a project name from a repository URL by stripping the `.git` suffix
/// and taking the last path segment.
///
/// Why: auto-registration from session history should produce a sensible name
/// without operator input; the repo basename is the conventional choice.
/// What: handles both HTTPS (`https://host/owner/repo`) and SSH
/// (`git@host:owner/repo.git`) URL forms. For HTTPS, requires at least three
/// non-empty slash-segments so that a bare host URL (`https://github.com/`)
/// returns `None`. For SSH, splits on `:` first, then on `/` within the path
/// component. Strips a trailing `.git` from the chosen segment.
/// Test: `derive_name_from_url_basic`, `derive_name_from_url_with_git_suffix`,
/// `derive_name_from_url_ssh`, `derive_name_from_url_no_path`.
pub fn derive_name_from_url(repo_url: &str) -> Option<String> {
    // SSH form: "git@github.com:owner/repo.git" — no "://" present but ":" is.
    if !repo_url.contains("://") && repo_url.contains(':') {
        let path_part = repo_url.split_once(':')?.1;
        let segment = path_part.trim_end_matches('/').split('/').next_back()?;
        if segment.is_empty() {
            return None;
        }
        return Some(segment.strip_suffix(".git").unwrap_or(segment).to_string());
    }
    // HTTPS / git:// form — collect non-empty slash-segments.
    // Require at least 3 (scheme "https:", host, repo) so that
    // "https://github.com/" (only 2 non-empty segments) returns None.
    let segments: Vec<&str> = repo_url.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }
    let segment = segments.last()?;
    Some(segment.strip_suffix(".git").unwrap_or(segment).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_serde_round_trip() {
        let p = Project {
            name: "trusty-tools".into(),
            repo_url: "https://github.com/owner/trusty-tools".into(),
            default_branch: "main".into(),
            stack_hint: Some("rust".into()),
            tags: vec!["backend".into(), "oss".into()],
            description: Some("the unified trusty workspace".into()),
            gh_user: Some("bobmatnyc".into()),
            gh_account: Some("bobmatnyc".into()),
            github: Some(GithubConfig {
                config_dir: Some("/home/bob/.config/gh-work".into()),
                token_env: None,
                account: None,
                host: None,
            }),
            commit_name: Some("Bob".into()),
            commit_email: Some("bob@example.com".into()),
            worktree: Some(false),
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(json.contains("gh_user"), "json: {json}");
        assert!(json.contains("gh_account"), "json: {json}");
        assert!(json.contains("github"), "json: {json}");
        assert!(json.contains("commit_name"), "json: {json}");
        assert!(json.contains("commit_email"), "json: {json}");
        assert!(json.contains("worktree"), "json: {json}");
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn project_without_optionals() {
        // Optional fields must default to empty/None and must not appear in JSON.
        let p = Project {
            name: "minimal".into(),
            repo_url: "https://github.com/owner/minimal".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(
            !json.contains("stack_hint"),
            "absent optional must not serialise: {json}"
        );
        assert!(
            !json.contains("worktree"),
            "absent worktree must not serialise: {json}"
        );
        assert!(
            !json.contains("gh_user"),
            "absent gh_user must not serialise: {json}"
        );
        assert!(
            !json.contains("gh_account"),
            "absent gh_account must not serialise: {json}"
        );
        assert!(
            !json.contains("tags"),
            "empty tags must not serialise: {json}"
        );
        assert!(
            // NOTE: `repo_url`'s value legitimately contains the substring
            // "github" (github.com), so the check must look for the JSON KEY
            // `"github":`, not the bare substring.
            !json.contains("\"github\":"),
            "absent github binding must not serialise: {json}"
        );
        assert!(
            !json.contains("commit_name") && !json.contains("commit_email"),
            "absent commit identity must not serialise: {json}"
        );
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.stack_hint, None);
        assert!(back.tags.is_empty());
    }

    // ── repo_url_matches (#2184) ─────────────────────────────────────────────

    /// Why: the SAME repo declared with and without a `.git` suffix must match.
    /// Test: itself.
    #[test]
    fn repo_url_matches_https_vs_git_suffix() {
        assert!(repo_url_matches(
            "https://github.com/acme/widget",
            "https://github.com/acme/widget.git"
        ));
    }

    /// Why: an SSH-form remote must match its HTTPS equivalent.
    /// Test: itself.
    #[test]
    fn repo_url_matches_ssh_form() {
        assert!(repo_url_matches(
            "git@github.com:acme/widget.git",
            "https://github.com/acme/widget"
        ));
    }

    /// Why: owner/repo comparison must be case-insensitive (GitHub logins are).
    /// Test: itself.
    #[test]
    fn repo_url_matches_case_insensitive() {
        assert!(repo_url_matches(
            "https://github.com/Acme/Widget",
            "https://github.com/acme/widget"
        ));
    }

    /// Why: a different repo (even same owner) must NOT match.
    /// Test: itself.
    #[test]
    fn repo_url_matches_rejects_different_repo() {
        assert!(!repo_url_matches(
            "https://github.com/acme/widget",
            "https://github.com/acme/gadget"
        ));
    }

    /// Why: an input `parse_github_path` cannot derive an identity from (e.g.
    /// an empty string or a bare host-only URL) must fall back to normalised
    /// exact-string comparison rather than panicking or always matching.
    /// Test: itself.
    #[test]
    fn repo_url_matches_falls_back_to_string_compare() {
        assert!(repo_url_matches("", ""));
        assert!(!repo_url_matches("", "https://github.com/acme/widget"));
        assert!(!repo_url_matches(
            "https://github.com/",
            "https://gitlab.com/"
        ));
    }

    #[test]
    fn derive_name_from_url_basic() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/my-repo"),
            Some("my-repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_with_git_suffix() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/trusty-tools.git"),
            Some("trusty-tools".into())
        );
    }

    #[test]
    fn derive_name_from_url_ssh() {
        assert_eq!(
            derive_name_from_url("git@github.com:owner/repo.git"),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_trailing_slash() {
        assert_eq!(
            derive_name_from_url("https://github.com/owner/repo/"),
            Some("repo".into())
        );
    }

    #[test]
    fn derive_name_from_url_no_path() {
        // A bare host with no path segment has no valid name.
        assert_eq!(derive_name_from_url("https://github.com/"), None);
    }

    fn minimal_project() -> Project {
        Project {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        }
    }

    /// Why (#3455): a project that has never set `worktree` must keep the
    /// pre-#3455 behaviour — every session gets its own worktree.
    #[test]
    fn worktree_enabled_defaults_true() {
        assert!(minimal_project().worktree_enabled());
    }

    /// Why (#3455): `worktree: Some(false)` is the "launch on main" opt-out.
    #[test]
    fn worktree_enabled_honors_false() {
        let mut p = minimal_project();
        p.worktree = Some(false);
        assert!(!p.worktree_enabled());
    }
}
