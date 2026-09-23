//! The ProjectRegistry's pinned `gh` identity, read synchronously for a
//! daemon-side checkout (#5850).
//!
//! Why: `tm --account <login> <url>` (#7166) persists the selected account onto
//! the project's REGISTRY record — `Project::gh_account` plus
//! `Project::github.config_dir` — and the session-spawn path reads it back
//! through [`crate::core::gh_account::resolve_gh_account_env_for_registry`].
//! The daemon's housekeeping path never did: `worktree_reclaim_gh::
//! resolve_daemon_gh_env` resolved only the STATIC `TrustyToolsConfig`
//! `projects[].github` list, which none of the operator-facing pinning paths
//! writes. A repository only the pinned account can see was therefore probed
//! by `gh pr list` as whichever account the machine's global config names, the
//! call came back "Could not resolve to a Repository", and every branch under
//! that worktree blocked. This module is the missing read.
//!
//! What: [`pinned_gh_env_in`] reads `<registry_dir>/projects.json`, collects
//! EVERY record whose `repo_url` matches the checkout's `origin` under
//! [`repo_url_matches`], picks the one pin among them with [`select_pin`], and
//! turns that record's `github:` binding into a
//! [`GhEnv`] through the ONE existing precedence engine
//! ([`gh_identity::resolve_gh_env`]) — no second copy of `config_dir >
//! token_env > account`. `Ok(None)` means "no pin recorded", the only outcome
//! that may fall through to the static config.
//!
//! ## Fail-CLOSED, not fail-open
//!
//! Every other outcome is an `Err`. A registry that cannot be read, a matching
//! record that cannot be parsed, and a pin whose credential is unusable all
//! leave "which account may see this repository?" UNANSWERED, and answering it
//! with the machine's global account is exactly the wrong-identity probe #5850
//! reports. The caller turns the `Err` into the `BranchPrState::LookupFailed`
//! the survey already surfaces, so the operator reads the pinned account's name
//! instead of a bare "no pull request found".
//!
//! An account pinned WITHOUT a `config_dir` fails closed for the #5851 reason:
//! `gh auth token -u <account>` does not discriminate between logged-in
//! accounts on a keyring-backed host, so minting a token from it here would
//! return the globally-active account's credential — the very substitution this
//! module exists to prevent.
//!
//! ## Cost
//!
//! One `std::fs::read_to_string` of a local JSON file. `ProjectStore` publishes
//! by atomic rename and documents that readers take NO file lock (see its
//! module `Concurrency` note), so this cannot block on a writer and needs no
//! bound of its own; the `gh` child it precedes stays bounded by
//! [`crate::session_manager::worktree_reclaim_gh::GH_TIMEOUT`], and the
//! `worktree_reclaim_gh_gate` single-flight guard means at most one read per
//! registry root is ever in flight.
//!
//! Test: `gh_account_registry_tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::gh_identity::{self, GhEnv, GhIdentityError};
use crate::core::trusty_tools_config::GithubConfig;
use crate::project::record::{Project, repo_url_matches};

/// The file [`crate::project::store::ProjectStore`] publishes under the
/// registry data directory.
///
/// Why: named once here rather than re-spelled inline, so a rename of the
/// store's own file is a single-line fix on this side too.
const REGISTRY_FILE: &str = "projects.json";

/// The one shape this module needs out of `projects.json`.
///
/// Why: deserialising straight into `HashMap<String, Project>` would make ONE
/// malformed record poison every other project's lookup. Holding the records as
/// raw values defers the strict parse to the record that actually matches, so a
/// broken record blocks only its own repository — and blocks it loudly.
/// What: the store's `projects` map, values left unparsed. The key is REQUIRED,
/// as it is in `ProjectStore`'s own document: `{}` is not a registry this
/// process understands, so it fails closed rather than reading as "no pin".
/// Test: `a_malformed_matching_record_fails_closed`,
/// `a_malformed_unrelated_record_does_not_block_a_match`,
/// `a_registry_without_a_projects_key_fails_closed`.
#[derive(Debug, Deserialize)]
struct RegistrySnapshot {
    /// Every registered project, keyed by registry name, still unparsed.
    projects: BTreeMap<String, serde_json::Value>,
}

/// Resolve the registry's pinned `gh` identity for `origin`, reading the
/// registry at `registry_dir` (#5850).
///
/// Why: the daemon's `gh` spawn sites are SYNCHRONOUS and hold only a working
/// directory, so they cannot await [`crate::project::ProjectRegistry::list`].
/// Taking the directory as a parameter is also what makes every arm below
/// testable against a registry a fixture wrote, with no daemon and no `$HOME`.
/// What: `Ok(Some(env))` when a matching record pins a usable identity,
/// `Ok(None)` when nothing is pinned (an absent registry, no matching record,
/// or a record that binds nothing) — the ONLY fallthrough — and `Err(reason)`
/// for every unanswerable case, with `reason` naming the pinned account.
/// Test: `registry_pin_resolves_the_projects_scoped_config_dir`,
/// `registry_pin_is_absent_for_an_unregistered_origin`,
/// `an_absent_registry_file_is_not_a_pin`,
/// `an_unreadable_registry_fails_closed`,
/// `a_malformed_matching_record_fails_closed`,
/// `a_pinned_config_dir_without_a_credential_fails_closed`,
/// `an_account_only_pin_fails_closed_naming_the_account`,
/// `an_unset_token_env_pin_fails_closed`.
pub(crate) fn pinned_gh_env_in(registry_dir: &Path, origin: &str) -> Result<Option<GhEnv>, String> {
    match read_pin(registry_dir, origin)? {
        None => Ok(None),
        Some(pin) => resolve_pin(&pin),
    }
}

/// What a registered project pins, or `None` when nothing matches.
///
/// Why: separating "what does the registry say" from "what env does that mean"
/// keeps the file-shaped failures (unreadable, unparsable) apart from the
/// credential-shaped ones, which need different wording for the operator.
/// What: the matched record's `gh_account` (blank treated as unset) and its own
/// `github:` binding, both exactly as persisted.
/// Test: the arms listed on [`pinned_gh_env_in`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RegistryPin {
    /// `Project::gh_account` — the login this project's sessions run as.
    pub(crate) account: Option<String>,
    /// `Project::github` — the per-project binding, `config_dir` included.
    pub(crate) github: Option<GithubConfig>,
}

impl RegistryPin {
    /// The pin a registry record carries, with a blank `gh_account` unset.
    pub(crate) fn from_project(project: &Project) -> Self {
        Self {
            account: project
                .gh_account
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_string),
            github: project.github.clone(),
        }
    }

    /// Does this record name no identity? A host-only `github:` names none.
    /// Test: `a_host_only_binding_is_not_a_pin`, `a_record_pinning_nothing_is_not_a_pin`.
    fn is_empty(&self) -> bool {
        self.account.is_none() && self.github.as_ref().is_none_or(|g| !names_identity(g))
    }

    /// The account name to NAME in a failure, when one is known.
    fn who(&self) -> String {
        match self
            .account
            .as_deref()
            .or_else(|| self.github.as_ref()?.account.as_deref())
        {
            Some(account) => format!("gh account '{account}'"),
            None => "the pinned gh identity".to_string(),
        }
    }
}

/// Read `origin`'s record out of `<registry_dir>/projects.json`.
///
/// Why: the three file-shaped outcomes are decided here and nowhere else — an
/// ABSENT registry is a legitimate "nothing is pinned" (a host that has never
/// registered a project), while an unreadable or unparsable one is a refusal.
/// Folding the two together is how a pinned project would silently fall back.
/// What: `Ok(None)` for an absent file, no matching record, or matching records
/// that pin nothing; `Err` for an I/O failure, a document that does not parse,
/// ANY matching record that does not parse as a [`Project`], or matching records
/// that pin different identities — see [`select_pin`].
/// Test: `an_absent_registry_file_is_not_a_pin`,
/// `an_unreadable_registry_fails_closed`,
/// `a_malformed_matching_record_fails_closed`,
/// `a_malformed_second_matching_record_fails_closed`,
/// `a_malformed_unrelated_record_does_not_block_a_match`.
fn read_pin(registry_dir: &Path, origin: &str) -> Result<Option<RegistryPin>, String> {
    let path = registry_dir.join(REGISTRY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // A host that has registered nothing pins nothing — the pre-#5850
        // behaviour, unchanged.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unanswerable(&path, &format!("could not be read ({e})"))),
    };
    let snapshot: RegistrySnapshot = serde_json::from_str(&text)
        .map_err(|e| unanswerable(&path, &format!("did not parse ({e})")))?;
    // #5850: EVERY matching record, not the first — an unpinned duplicate that
    // sorts earlier must not shadow the pinned one.
    let mut candidates = Vec::new();
    for (name, raw) in snapshot.projects.iter().filter(|(_, raw)| {
        raw.get("repo_url")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|url| repo_url_matches(url, origin))
    }) {
        let project: Project = serde_json::from_value(raw.clone()).map_err(|e| {
            format!(
                "the project registry record '{name}' names this repository but did not parse \
                 ({e}), so whether it pins a gh account is unknown — refusing to probe the \
                 repository as this machine's global gh account (#5850)"
            )
        })?;
        candidates.push((name.clone(), RegistryPin::from_project(&project)));
    }
    select_pin(candidates)
}

/// Choose the one pin among every registry record that names a repository.
///
/// Why: the registry can hold two records for one origin (a re-registration
/// under a different name, a URL spelled with `.git`). Taking the first match
/// lets an unpinned record hide a pinned one, and picking between two pins by
/// position guesses at an account. The daemon's housekeeping probe and the
/// session-spawn path both call this, so they cannot choose differently.
/// What: drops records that pin nothing; `Ok(None)` when none are left,
/// `Ok(Some(pin))` when every remaining record pins the same identity, and
/// `Err` naming two records when they pin different ones. Order-independent.
/// Test: `an_unpinned_record_does_not_shadow_a_pinned_one`,
/// `two_disagreeing_pins_for_one_repository_fail_closed`,
/// `find_pinned_gh_identity_skips_an_unpinned_duplicate`,
/// `find_pinned_gh_identity_refuses_disagreeing_pins`.
pub(crate) fn select_pin(
    candidates: Vec<(String, RegistryPin)>,
) -> Result<Option<RegistryPin>, String> {
    let mut pinned: Vec<(String, RegistryPin)> = candidates
        .into_iter()
        .filter(|(_, pin)| !pin.is_empty())
        .collect();
    pinned.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((first_name, first)) = pinned.first() else {
        return Ok(None);
    };
    if let Some((other_name, _)) = pinned.iter().find(|(_, pin)| pin != first) {
        return Err(format!(
            "the project registry records '{first_name}' and '{other_name}' both name this \
             repository but pin different gh identities — refusing to probe it as either, or \
             as this machine's global gh account (#5850). Remove one of the records or make \
             their gh_account/github settings agree."
        ));
    }
    Ok(Some(first.clone()))
}

/// Turn a pinned record into the `gh` overrides the session-spawn path injects.
///
/// Why: the precedence that turns a `github:` binding into env vars is written
/// ONCE, in [`gh_identity::resolve_gh_env`]; this adds only the two refusals a
/// housekeeping probe needs on top of it. A `config_dir` that holds no
/// credential and an account with no `config_dir` both mean "no credential for
/// the pinned account is available", and both must block rather than let `gh`
/// answer as somebody else.
/// What: `Ok(Some(env))` for a usable binding, `Ok(None)` when the resolved env
/// sets neither `GH_CONFIG_DIR` nor `GH_TOKEN` (a host-only binding), `Err`
/// otherwise.
/// Test: `registry_pin_resolves_the_projects_scoped_config_dir`,
/// `a_host_only_binding_is_not_a_pin`,
/// `a_pinned_config_dir_without_a_credential_fails_closed`,
/// `an_account_only_pin_fails_closed_naming_the_account`,
/// `an_unset_token_env_pin_fails_closed`.
fn resolve_pin(pin: &RegistryPin) -> Result<Option<GhEnv>, String> {
    // The record's own binding, with `gh_account` supplying `account` when the
    // binding does not name one itself — the same two keys
    // `gh_account::find_pinned_gh_identity` reads for a session spawn.
    let mut cfg = pin.github.clone().unwrap_or_default();
    if cfg.account.is_none() {
        cfg.account = pin.account.clone();
    }
    let env = match gh_identity::resolve_gh_env(Some(&cfg)) {
        Ok(env) => env,
        // #5851: `gh auth token -u <account>` does not select an account on a
        // keyring-backed host, so there is no safe way to honour this pin.
        Err(GhIdentityError::AccountStrategyUnsupported(account)) => {
            return Err(format!(
                "this repository is pinned to gh account '{account}' with no \
                 `github.config_dir`, and `gh auth token -u {account}` does not \
                 discriminate between logged-in accounts on a keyring-backed host \
                 (#5851) — refusing to probe it as whichever account is globally \
                 active. Re-run `tm --user {account} <url>` once to build this \
                 project's scoped gh config dir, or set `github.config_dir` for it."
            ));
        }
    };
    // #5850: `resolve_gh_env` skips a `token_env` it cannot read, which leaves
    // NO identity selected — reading that as "unpinned" is the fallback.
    if let Some(var) = named_token_env(&cfg)
        && selected_config_dir(&cfg).is_none()
        && !env.vars().iter().any(|(key, _)| key == "GH_TOKEN")
    {
        let who = pin.who();
        return Err(format!(
            "this repository is pinned to {who} via `github.token_env` '{var}', which is \
             unset or empty in this process — refusing to fall back to this machine's \
             global gh account (#5850). Export {var} where the daemon runs, or set \
             `github.config_dir` for this project."
        ));
    }
    // #5850: `GH_HOST` alone selects no identity, so an env carrying neither
    // identity var is not a pin — let the static tier answer.
    if !env
        .vars()
        .iter()
        .any(|(key, _)| key == "GH_CONFIG_DIR" || key == "GH_TOKEN")
    {
        return Ok(None);
    }
    if let Some(dir) = selected_config_dir(&cfg)
        && !crate::core::gh_account::config_dir_has_credential(&dir)
    {
        let who = pin.who();
        let dir = dir.display();
        return Err(format!(
            "this repository is pinned to {who} via gh config dir {dir}, which holds no \
             github.com credential ({dir}/hosts.yml is missing or names no account) — \
             refusing to fall back to this machine's global gh account (#5850). Run \
             `GH_CONFIG_DIR={dir} gh auth login` to authenticate inside it."
        ));
    }
    Ok(Some(env))
}

/// The `config_dir` [`gh_identity::resolve_gh_env`] would select, if any.
///
/// Why: the credential check must apply to the directory that ACTUALLY won the
/// precedence race, not to a `config_dir` key a higher-precedence strategy
/// overrode. Today `config_dir` IS the top of that chain, so this is a trim —
/// but reading it back through one helper keeps the check honest if the chain
/// ever changes.
/// What: the trimmed, non-empty `config_dir`.
/// Test: `a_pinned_config_dir_without_a_credential_fails_closed`.
fn selected_config_dir(cfg: &GithubConfig) -> Option<PathBuf> {
    cfg.config_dir
        .as_deref()
        .map(|p| p.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Does `cfg` name an identity — a `config_dir`, a `token_env`, or an account?
/// Test: `a_host_only_binding_is_not_a_pin`.
fn names_identity(cfg: &GithubConfig) -> bool {
    selected_config_dir(cfg).is_some()
        || named_token_env(cfg).is_some()
        || cfg.account.as_deref().is_some_and(|a| !a.trim().is_empty())
}

/// The trimmed, non-empty `token_env` variable name the binding names, if any.
/// Test: `an_unset_token_env_pin_fails_closed`.
fn named_token_env(cfg: &GithubConfig) -> Option<&str> {
    cfg.token_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The refusal for a registry this process could not interrogate.
///
/// Why: both file-shaped failures need the same three facts — which file, what
/// went wrong, and that the consequence is a REFUSAL rather than a fallback.
/// Writing the sentence once keeps the two arms from drifting.
/// Test: `an_unreadable_registry_fails_closed`,
/// `a_malformed_registry_document_fails_closed`.
fn unanswerable(path: &Path, what: &str) -> String {
    format!(
        "the project registry at {} {what}, so whether this repository pins a gh account \
         is unknown — refusing to probe it as this machine's global gh account (#5850)",
        path.display()
    )
}

#[cfg(test)]
#[path = "gh_account_registry_tests.rs"]
mod gh_account_registry_tests;
