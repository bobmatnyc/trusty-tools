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
//! What: [`pinned_gh_env_with`] reads `<registry_dir>/projects.json`, collects
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
//! An account pinned WITHOUT a `config_dir` never mints a token, for the #5851
//! reason: `gh auth token -u <account>` does not discriminate between logged-in
//! accounts on a keyring-backed host, so it would return the globally-active
//! account's credential — the very substitution this module exists to prevent.
//! #8510: such a pin borrows a candidate config dir ([`AccountDirSources`])
//! whose local `hosts.yml` names the pinned account as its ACTIVE `user:`, and
//! fails closed when none does. The repository owner never picks the account.
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
use crate::core::trusty_tools_config::{GithubConfig, TrustyToolsConfig};
use crate::project::record::{Project, repo_url_matches};

/// The file [`crate::project::store::ProjectStore`] publishes under the
/// registry data directory.
///
/// Why: named once here rather than re-spelled inline, so a rename of the
/// store's own file is a single-line fix on this side too.
const REGISTRY_FILE: &str = "projects.json";

/// Directory under the tm state root holding one `gh` config dir per account
/// (#7166); the daemon's `account_config_dir` bootstrap names it from here.
pub(crate) const GH_ACCOUNTS_DIR_NAME: &str = "gh-accounts";

/// The config dirs an account-only pin may borrow (#8510).
///
/// Why: #8416 put the registry ahead of the static config, so a record with
/// `gh_account` but no `github.config_dir` refused even when the static config
/// binds a dir for that origin. Borrowing a dir is safe only when the dir
/// itself selects the pinned account, so each candidate is verified.
/// What: the static config's per-project `config_dir` for this origin (the
/// global binding is not "for this origin"), then `<state_root>/gh-accounts/
/// <login>`. `Default` names neither: the pre-#8510 refusal.
/// Test: `account_dir_sources_take_only_this_origins_static_binding`.
#[derive(Debug, Default, Clone)]
pub(crate) struct AccountDirSources {
    /// The static `projects[].github.config_dir` bound to this origin.
    pub(crate) static_config_dir: Option<PathBuf>,
    /// tm's state root, whose `gh-accounts/<login>` is the second candidate.
    pub(crate) state_root: Option<PathBuf>,
}

impl AccountDirSources {
    /// The candidates for `origin`: its static binding and tm's own account dirs.
    /// Test: `account_dir_sources_take_only_this_origins_static_binding`.
    pub(crate) fn for_origin(
        config: &TrustyToolsConfig,
        origin: &str,
        state_root: PathBuf,
    ) -> Self {
        let static_config_dir = config
            .projects
            .iter()
            .find(|p| repo_url_matches(&p.repo_url, origin))
            .and_then(|p| p.github.as_ref())
            .and_then(selected_config_dir);
        Self {
            static_config_dir,
            state_root: Some(state_root),
        }
    }

    /// Every candidate dir for `login`, in order, or why the tm dir is skipped.
    /// Test: `an_account_only_pin_refuses_an_unsafe_login_segment`.
    fn candidates(&self, login: &str) -> Vec<Result<PathBuf, String>> {
        let mut out: Vec<Result<PathBuf, String>> =
            self.static_config_dir.iter().cloned().map(Ok).collect();
        if let Some(root) = &self.state_root {
            let safe = !matches!(login, "" | "." | "..") && !login.contains(['/', '\\']);
            out.push(if safe {
                Ok(root.join(GH_ACCOUNTS_DIR_NAME).join(login))
            } else {
                Err(format!("'{login}' is not a valid gh-accounts path segment"))
            });
        }
        out
    }

    /// The first candidate whose active `user:` is `login`, else every reason.
    /// Test: `an_account_only_pin_borrows_a_static_dir_whose_active_user_matches`,
    /// `an_account_only_pin_falls_back_to_tms_own_account_dir`.
    fn verified_dir(&self, login: &str) -> Result<PathBuf, Vec<String>> {
        let mut reasons = Vec::new();
        for candidate in self.candidates(login) {
            match candidate.and_then(|dir| verify_active_user(&dir, login).map(|()| dir)) {
                Ok(dir) => return Ok(dir),
                Err(reason) => reasons.push(reason),
            }
        }
        Err(reasons)
    }
}

/// Does `dir`'s local `hosts.yml` name `login` as its ACTIVE github.com user?
///
/// Why (#8510): only the active `user:` is who `gh` acts as under
/// `GH_CONFIG_DIR=dir`; a dir that merely lists the account would probe as
/// someone else. Read locally, never over the network.
/// What: `Ok(())` on a case-insensitive match (GitHub logins are); otherwise
/// `Err` naming the dir and the failure — missing dir, unreadable or
/// unparsable `hosts.yml`, no active user, or a different active user.
/// Test: `an_account_only_pin_refuses_a_static_dir_active_as_another_account`,
/// `an_account_only_pin_refuses_a_dir_listing_it_but_active_as_another`,
/// `an_account_only_pin_refuses_a_missing_candidate_dir`,
/// `an_account_only_pin_refuses_an_unreadable_hosts_yml`,
/// `an_account_only_pin_refuses_a_malformed_hosts_yml`,
/// `an_account_only_pin_refuses_a_hosts_yml_with_no_active_user`,
/// `an_account_only_pin_never_resolves_from_the_repository_owner`.
fn verify_active_user(dir: &Path, login: &str) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} does not exist", dir.display()));
    }
    let hosts = dir.join("hosts.yml");
    let text = std::fs::read_to_string(&hosts)
        .map_err(|e| format!("{} could not be read ({e})", hosts.display()))?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| format!("{} did not parse ({e})", hosts.display()))?;
    let host = doc.get("github.com");
    let active = host
        .and_then(|h| h.get("user"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty());
    match active {
        Some(user) if user.eq_ignore_ascii_case(login) => Ok(()),
        Some(user) => {
            let lists = host
                .and_then(|h| h.get("users"))
                .and_then(serde_yaml::Value::as_mapping)
                .is_some_and(|users| {
                    users
                        .keys()
                        .filter_map(serde_yaml::Value::as_str)
                        .any(|k| k.trim().eq_ignore_ascii_case(login))
                });
            let how = if lists {
                format!("lists '{login}' but is active as")
            } else {
                "is active as".to_string()
            };
            Err(format!("{} {how} '{user}'", hosts.display()))
        }
        None => Err(format!(
            "{} names no active github.com user",
            hosts.display()
        )),
    }
}

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
#[cfg(test)]
pub(crate) fn pinned_gh_env_in(registry_dir: &Path, origin: &str) -> Result<Option<GhEnv>, String> {
    pinned_gh_env_with(registry_dir, origin, &AccountDirSources::default())
}

/// `pinned_gh_env_in` with the config dirs an account-only pin may borrow.
///
/// Why (#8510): the production call site knows the static config and tm's
/// state root; tests pass fixture dirs so no `$HOME` state leaks in.
/// What: the same outcomes as `pinned_gh_env_in`, except an account-only pin
/// resolves to the first `sources` dir whose active `user:` is that account.
/// Test: `an_account_only_pin_borrows_a_static_dir_whose_active_user_matches`.
pub(crate) fn pinned_gh_env_with(
    registry_dir: &Path,
    origin: &str,
    sources: &AccountDirSources,
) -> Result<Option<GhEnv>, String> {
    match read_pin(registry_dir, origin)? {
        None => Ok(None),
        Some(pin) => resolve_pin(&pin, sources),
    }
}

/// What a registered project pins, or `None` when nothing matches.
///
/// Why: separating "what does the registry say" from "what env does that mean"
/// keeps the file-shaped failures (unreadable, unparsable) apart from the
/// credential-shaped ones, which need different wording for the operator.
/// What: the matched record's `gh_account` (blank treated as unset) and its own
/// `github:` binding, both exactly as persisted.
/// Test: the arms listed on `pinned_gh_env_in`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RegistryPin {
    /// `Project::gh_account` — the login this project's sessions run as.
    pub(crate) account: Option<String>,
    /// `Project::github` — the per-project binding, `config_dir` included.
    pub(crate) github: Option<GithubConfig>,
    /// The record's registry name and `repo_url`, for the #8510 fix command.
    pub(crate) record: Option<(String, String)>,
}

impl RegistryPin {
    /// The pin a registry record carries, with a blank `gh_account` unset.
    pub(crate) fn from_project(project: &Project) -> Self {
        Self {
            record: Some((project.name.clone(), project.repo_url.clone())),
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
    /// Test: `a_host_only_binding_is_not_a_pin`, `a_record_pinning_nothing_is_not_a_pin`,
    /// `a_host_only_duplicate_does_not_block_the_pinned_record`.
    fn is_empty(&self) -> bool {
        self.account.is_none() && self.github.as_ref().is_none_or(|g| !names_identity(g))
    }

    /// The login this pin selects: `gh_account`, else `github.account`.
    /// Test: `same_login_in_a_different_case_agrees`.
    fn login(&self) -> Option<&str> {
        self.account.as_deref().or_else(|| {
            self.github
                .as_ref()?
                .account
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
        })
    }

    /// The account name to NAME in a failure, when one is known.
    fn who(&self) -> String {
        match self.login() {
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
/// What: drops records that pin nothing. Agreement is judged on the identity a
/// pin selects, not on the whole record: every pin that NAMES a login must name
/// the same one (case-insensitive; a pin with no login sits out), and no two may
/// set different non-empty `config_dir` or `token_env` values — otherwise `Err`
/// names two records and the field that conflicts. `Ok(Some(pin))` is the most
/// specific agreeing pin (a `config_dir` outranks a `token_env`, which outranks
/// neither), ties broken by record name, so the answer is order-independent; a
/// winner with no login inherits the agreed one. `Ok(None)` when no pin is left.
/// Test: `an_unpinned_record_does_not_shadow_a_pinned_one`,
/// `two_disagreeing_pins_for_one_repository_fail_closed`,
/// `same_login_pins_prefer_the_one_with_a_config_dir`,
/// `same_login_in_a_different_case_agrees`,
/// `same_login_with_conflicting_config_dirs_fails_closed`,
/// `a_host_only_duplicate_does_not_block_the_pinned_record`,
/// `a_no_login_pin_agrees_with_a_login_pin_on_the_same_config_dir`,
/// `a_no_login_pin_with_a_different_config_dir_fails_closed`,
/// `find_pinned_gh_identity_inherits_the_login_for_a_no_login_config_dir_pin`,
/// `find_pinned_gh_identity_skips_an_unpinned_duplicate`,
/// `find_pinned_gh_identity_prefers_the_config_dir_pin_for_one_login`,
/// `find_pinned_gh_identity_refuses_disagreeing_pins`.
pub(crate) fn select_pin(
    candidates: Vec<(String, RegistryPin)>,
) -> Result<Option<RegistryPin>, String> {
    let mut pinned: Vec<(String, RegistryPin)> = candidates
        .into_iter()
        .filter(|(_, pin)| !pin.is_empty())
        .collect();
    pinned.sort_by(|a, b| a.0.cmp(&b.0));
    let login = |pin: &RegistryPin| pin.login().map(str::to_ascii_lowercase);
    let config_dir = |pin: &RegistryPin| pin.github.as_ref().and_then(selected_config_dir);
    let token_env = |pin: &RegistryPin| {
        pin.github
            .as_ref()
            .and_then(named_token_env)
            .map(str::to_string)
    };
    // #5850: a pin with no login (`github: {config_dir}` from seed_from_config or
    // `--gh-config-dir`) sits out the login comparison; the dir checks still apply.
    if let Some((a, b)) = first_disagreement(&pinned, login) {
        return Err(disagreement(a, b, "log in as different gh accounts"));
    }
    if let Some((a, b)) = first_disagreement(&pinned, config_dir) {
        return Err(disagreement(a, b, "pin different gh config dirs"));
    }
    if let Some((a, b)) = first_disagreement(&pinned, token_env) {
        return Err(disagreement(
            a,
            b,
            "pin different `github.token_env` variables",
        ));
    }
    let specificity = |pin: &RegistryPin| (config_dir(pin).is_some(), token_env(pin).is_some());
    // The login every record that names one agrees on, in its written case.
    let agreed = pinned
        .iter()
        .find_map(|(_, pin)| pin.login().map(str::to_string));
    // `rev` makes `max_by_key` keep the FIRST record by name on a tie.
    Ok(pinned
        .into_iter()
        .rev()
        .max_by_key(|(_, pin)| specificity(pin))
        .map(|(_, mut pin)| {
            // #5850: a winner with no login inherits the agreed one, so
            // `configured_account_pair` enforcement stays armed.
            if pin.login().is_none() {
                pin.account = agreed;
            }
            pin
        }))
}

/// The first pair of records whose `field` values are both set and differ.
/// Test: `same_login_with_conflicting_config_dirs_fails_closed`.
fn first_disagreement<'a, T: PartialEq>(
    pinned: &'a [(String, RegistryPin)],
    field: impl Fn(&RegistryPin) -> Option<T>,
) -> Option<(&'a str, &'a str)> {
    let mut seen: Option<(&'a str, T)> = None;
    for (name, pin) in pinned {
        let Some(value) = field(pin) else { continue };
        if let Some((first, first_value)) = &seen {
            if *first_value != value {
                return Some((first, name));
            }
        } else {
            seen = Some((name, value));
        }
    }
    None
}

/// The refusal for two registry records that answer the identity differently.
/// Test: `two_disagreeing_pins_for_one_repository_fail_closed`.
fn disagreement(a: &str, b: &str, how: &str) -> String {
    format!(
        "the project registry records '{a}' and '{b}' both name this repository but {how} — \
         refusing to probe it as either, or as this machine's global gh account (#5850). \
         Remove one of the records or make their gh_account/github settings agree."
    )
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
/// otherwise. An account-only pin (no `config_dir`, no `token_env`) first
/// borrows the dir [`AccountDirSources::verified_dir`] verifies (#8510).
/// Test: `registry_pin_resolves_the_projects_scoped_config_dir`,
/// `a_host_only_binding_is_not_a_pin`,
/// `a_pinned_config_dir_without_a_credential_fails_closed`,
/// `an_account_only_pin_fails_closed_naming_the_account`,
/// `an_account_only_pin_borrows_a_static_dir_whose_active_user_matches`,
/// `an_unset_token_env_pin_fails_closed`.
fn resolve_pin(pin: &RegistryPin, sources: &AccountDirSources) -> Result<Option<GhEnv>, String> {
    // The record's own binding, with `gh_account` supplying `account` when the
    // binding does not name one itself — the same two keys
    // `gh_account::find_pinned_gh_identity` reads for a session spawn.
    let mut cfg = pin.github.clone().unwrap_or_default();
    if cfg.account.is_none() {
        cfg.account = pin.account.clone();
    }
    // #8510: an account-only pin borrows a dir whose ACTIVE user is that account
    // — never one picked from the repository owner — or refuses naming the fix.
    let mut borrow_failures = Vec::new();
    if let Some(login) = pin.login()
        && selected_config_dir(&cfg).is_none()
        && named_token_env(&cfg).is_none()
    {
        match sources.verified_dir(login) {
            Ok(dir) => cfg.config_dir = Some(dir),
            Err(reasons) => borrow_failures = reasons,
        }
    }
    let env = match gh_identity::resolve_gh_env(Some(&cfg)) {
        Ok(env) => env,
        // #5851: `gh auth token -u <account>` does not select an account on a
        // keyring-backed host, so there is no safe way to honour this pin.
        Err(GhIdentityError::AccountStrategyUnsupported(account)) => {
            return Err(account_only_refusal(pin, &account, &borrow_failures));
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

/// The refusal for an account pin no candidate config dir verifies (#5851, #8510).
///
/// Why: the operator needs the one command that pins a dir for this record,
/// and why each candidate was rejected, not a bare "refusing".
/// What: names the account, every candidate's failure, and
/// `tm projects register <name> --repo-url <url> --gh-account <login>
/// --gh-config-dir <dir>` with the record's own name and URL filled in.
/// Test: `an_account_only_pin_fails_closed_naming_the_account`,
/// `an_account_only_pin_refuses_a_static_dir_active_as_another_account`.
fn account_only_refusal(pin: &RegistryPin, account: &str, failures: &[String]) -> String {
    let (name, url) = pin
        .record
        .as_ref()
        .map_or(("<name>", "<url>"), |(n, u)| (n.as_str(), u.as_str()));
    let checked = if failures.is_empty() {
        String::new()
    } else {
        format!(
            " No candidate gh config dir is active as it: {}.",
            failures.join("; ")
        )
    };
    format!(
        "this repository is pinned to gh account '{account}' with no `github.config_dir`, \
         and `gh auth token -u {account}` does not discriminate between logged-in accounts \
         on a keyring-backed host (#5851) — refusing to probe it as whichever account is \
         globally active.{checked} Pin a gh config dir whose active user is '{account}': \
         `tm projects register {name} --repo-url {url} --gh-account {account} \
         --gh-config-dir <dir>` (#8510)."
    )
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
