//! `tm secrets assign` — one vault key into a named project's scope (#11015).
//!
//! Why: the owner wants a key of one vault placed in another project's
//! vault without anyone seeing the value. `copy --from-vault` (#10998)
//! already moves a value inside the server, but only into the caller's own
//! checkout; `assign` names the destination project instead.
//! What: [`parse_source`] reads SRC (`trusty/<owner>[/<repo>]:KEY` or a
//! `secret://` reference that pins its vault); [`ProjectName`] reads
//! `--project owner/repo`; a [`ProjectResolver`] maps it to a checkout
//! directory; [`assign`] checks the owner and, for an agent other than the
//! Architect ([`judge_caller`]), the project, then sends one `secrets.copy` whose `project` field is that
//! directory and whose `source` is the vault key. The value moves inside
//! the server, which applies the agent deny, the Architect ticket (keyed on
//! the destination's root) and the audit. tm prints names only.
//! Until #10368's project registry lands, the only resolver is
//! [`RegistryPending`], which refuses every project.
//! Test: `assign_tests.rs` (a child of `tests.rs`).
//!
//! Governing document: DOC-74 §9, §15.2
//! (`docs/specs/DOC-74-secrets-integration.md`).

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, bail};
use serde_json::{Value, json};
use trusty_secrets::ancestry::{OsProcessTable, ProcessTable, has_agent_ancestor};
use trusty_secrets::api::methods::{CopyResponse, CopySource, ScopeKind, ScopesResponse, method};
use trusty_secrets::{
    BackendId, OwnerName, RepoName, SecretKey, SecretRef, SecretsError, VaultName,
};

use super::Ctx;
use super::verbs::report_copy;

/// The refusal for a SRC in neither form; never quotes the argument.
pub(crate) const SRC_FORMS: &str =
    "tm secrets assign: SRC is `trusty/<owner>[/<repo>]:KEY` or `secret://<owner>[/<repo>]/KEY`";

/// The refusal for a `secret://KEY` source, which pins no vault.
pub(crate) const SRC_UNPINNED: &str =
    "tm secrets assign: `secret://KEY` names no vault; name its owner or project";

/// The refusal for a `--project` that is not `owner/repo`.
pub(crate) const PROJECT_FORM: &str =
    "tm secrets assign: --project is `owner/repo`, never a path; nothing was written";

/// The refusal of [`RegistryPending`]: no project resolves before #10368.
pub(crate) const NOT_REGISTERED: &str =
    "tm secrets assign: project not registered (#10368); nothing was written";

/// The refusal for a destination under another owner than the source's (D2).
pub(crate) const OTHER_OWNER: &str = "tm secrets assign: the project belongs to another owner than the source vault; nothing was written";

/// The refusal for an agent naming a project other than its own (D2, Dfx).
pub(crate) const AGENT_OTHER_PROJECT: &str = "tm secrets assign: an agent other than the Architect may name only its own checkout's project; nothing was written";

/// The refusal when the resolved checkout's remote is not the named owner.
pub(crate) const CHECKOUT_MISMATCH: &str =
    "tm secrets assign: the checkout found for the project has another owner; nothing was written";

/// The refusal when either checkout reports no owner scope.
pub(crate) const NO_OWNER_SCOPE: &str =
    "tm secrets assign: the server reported no owner scope; nothing was written";

/// A `--project owner/repo` argument, validated.
///
/// Why: D1 — the project is named by its GitHub identity, never by a path,
/// so a typo cannot redirect the write to an arbitrary directory.
/// What: exactly one `/` between an [`OwnerName`] and a [`RepoName`]; any
/// other shape is [`PROJECT_FORM`].
/// Test: `assign_project_name_accepts_owner_repo_only`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectName {
    /// The owner.
    pub(crate) owner: OwnerName,
    /// The repository.
    pub(crate) repo: RepoName,
}

impl ProjectName {
    /// Parse `owner/repo`; see the type docs.
    pub(crate) fn parse(raw: &str) -> anyhow::Result<Self> {
        let refuse = || anyhow!(PROJECT_FORM);
        let (owner, repo) = raw.split_once('/').ok_or_else(refuse)?;
        Ok(Self {
            owner: OwnerName::new(owner).map_err(|_| refuse())?,
            repo: RepoName::new(repo).map_err(|_| refuse())?,
        })
    }
}

/// Maps a project name to the checkout directory the server resolves.
///
/// Why: D1 — the mapping belongs to #10368's project registry, which does
/// not exist yet; the seam lets the verb ship and be tested without it.
/// What: the checkout's absolute directory, or an error with fixed text
/// that names no path the caller did not give.
pub(crate) trait ProjectResolver: Sync {
    /// The checkout directory for `project`.
    fn checkout(&self, project: &ProjectName) -> anyhow::Result<PathBuf>;
}

/// The resolver until #10368: every project is unregistered.
///
/// Test: `assign_unknown_project_is_a_fixed_error_and_writes_nothing`.
pub(crate) struct RegistryPending;

impl ProjectResolver for RegistryPending {
    fn checkout(&self, _project: &ProjectName) -> anyhow::Result<PathBuf> {
        // #11015: the registry-backed resolver lands with #10368.
        bail!(NOT_REGISTERED)
    }
}

/// Who runs `assign`, for the project rule (owner ruling Dfx, 2026-11-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// No Claude Code above this process: any project of the source's owner.
    Person,
    /// The bound Architect session's main thread: likewise.
    Architect,
    /// Any other agent, a PM or a subagent: its own checkout's project only.
    Agent,
}

/// The parts of `assign` a test replaces: the resolver and the caller check.
#[derive(Clone, Copy)]
pub(crate) struct Seam<'a> {
    /// Maps `--project` to a checkout.
    pub(crate) resolver: &'a dyn ProjectResolver,
    /// Judges the caller.
    pub(crate) caller: fn() -> Caller,
}

/// The production seam: [`RegistryPending`] and [`judge_caller`].
pub(crate) const PRODUCTION: Seam<'static> = Seam {
    resolver: &RegistryPending,
    caller: judge_caller,
};

/// This process's [`Caller`]: [`caller_from`] over the live process table
/// and [`this_session_is_architect`].
pub(crate) fn judge_caller() -> Caller {
    caller_from(
        agent_in(&OsProcessTable, std::process::id()),
        this_session_is_architect,
    )
}

/// The [`Caller`] for an agent-ancestor answer and an Architect check.
///
/// What: [`Caller::Person`] when `agent` is false, without running
/// `architect`; [`Caller::Architect`] when `architect` answers `Ok(true)`;
/// [`Caller::Agent`] when it answers `Ok(false)` or an error (fail closed).
/// Test: `assign_caller_from_maps_every_row_and_fails_closed`.
pub(crate) fn caller_from(agent: bool, architect: impl FnOnce() -> anyhow::Result<bool>) -> Caller {
    if !agent {
        return Caller::Person;
    }
    match architect() {
        Ok(true) => Caller::Architect,
        Ok(false) | Err(_) => Caller::Agent,
    }
}

/// Whether this session is the bound Architect, as `tm env` judges it.
///
/// What: `tm fleet status`'s `this_session` check — the Architect's stamp,
/// profile grant and launch record; an error finding the home or the
/// Architect directory is returned for [`caller_from`] to judge.
/// Limit: the only subagent marker visible here is `CLAUDE_MPM_SUB_AGENT`,
/// set on trusty-agents' spawned processes only. An in-process Task
/// subagent of the Architect session shares its environment and judges as
/// the Architect. `tm env`'s binding check has the same limit, which it
/// closes with a pm-guard grant (#8939); `assign` has no such grant.
/// See #11015.
fn this_session_is_architect() -> anyhow::Result<bool> {
    use crate::commands::env_file::trusted_home;
    use crate::commands::fleet::resolve_dir;
    use crate::commands::fleet::status::{this_session_check, this_session_env};
    use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, HookEnv};
    use trusty_mpm::core::config::MpmConfig;
    let home = trusted_home()?;
    let dir = resolve_dir(None, &home)?;
    let root = home.join(ANCHOR_ROOT);
    let env = HookEnv {
        home: Some(home),
        ..this_session_env()
    };
    Ok(this_session_check(&dir, env, || MpmConfig::load(&root)).ok)
}

/// Whether `pid` or a process above it in `table` is Claude Code.
///
/// What: `has_agent_ancestor`; an unreadable table answers `true` (fail
/// closed), so an agent cannot escape D2 by hiding its ancestry.
/// Test: `assign_agent_judgement_fails_closed`.
pub(crate) fn agent_in(table: &dyn ProcessTable, pid: u32) -> bool {
    has_agent_ancestor(table, pid).unwrap_or(true)
}

/// The source vault and key SRC names.
///
/// Why: D4 — SRC names the source only; one parser keeps the two forms in
/// step with `SecretRef::parse`.
/// What: a `secret://` text goes through `SecretRef::parse` and must pin a
/// vault ([`SRC_UNPINNED`] otherwise); any other text is `VAULT:KEY`, split
/// at the first `:`, each half validated. Every refusal is [`SRC_FORMS`]
/// plus the broken rule, never the input.
/// Test: `assign_parse_source_accepts_both_forms_and_refuses_bad_ones`.
pub(crate) fn parse_source(raw: &str) -> anyhow::Result<(VaultName, SecretKey)> {
    let refuse = |e: SecretsError| anyhow!("{SRC_FORMS}: {e}");
    if raw.starts_with(trusty_secrets::api::SCHEME) {
        let reference = SecretRef::parse(raw).map_err(refuse)?;
        let vault = reference
            .pinned_vault()
            .ok_or_else(|| anyhow!(SRC_UNPINNED))?;
        return Ok((vault, reference.key().clone()));
    }
    let (vault, key) = raw.split_once(':').ok_or_else(|| anyhow!(SRC_FORMS))?;
    Ok((
        VaultName::new(vault).map_err(refuse)?,
        SecretKey::new(key).map_err(refuse)?,
    ))
}

/// The owner vault `trusty/<owner>` of any vault, `None` if it has none.
fn owner_vault_of(vault: &VaultName) -> Option<VaultName> {
    let owner = vault.as_str().split('/').nth(1)?;
    OwnerName::new(owner).ok().map(|o| VaultName::owner(&o))
}

/// The parsed `assign` command line.
pub(crate) struct AssignArgs<'a> {
    /// SRC as typed.
    pub(crate) src: &'a str,
    /// `--project` as typed.
    pub(crate) project: &'a str,
    /// `--as`, the destination key.
    pub(crate) dest: Option<&'a str>,
    /// `--from-backend`.
    pub(crate) from: &'a str,
    /// `--to-backend`.
    pub(crate) to: &'a str,
}

/// A checkout's project and owner vaults, from `secrets.scopes`.
struct Vaults {
    project: VaultName,
    owner: VaultName,
}

/// `secrets.scopes` for `ctx`'s directory; no owner scope is a refusal.
async fn vaults(ctx: &Ctx<'_>) -> anyhow::Result<Vaults> {
    let scopes: ScopesResponse = ctx.call(method::SCOPES, Value::Null).await?;
    let pick = |kind| {
        scopes
            .scopes
            .iter()
            .find(|scope| scope.kind == kind)
            .map(|scope| scope.vault.clone())
    };
    let project = pick(ScopeKind::Project)
        .ok_or_else(|| anyhow!("tm secrets: the server reported no project scope"))?;
    let owner = pick(ScopeKind::Owner).ok_or_else(|| anyhow!(NO_OWNER_SCOPE))?;
    Ok(Vaults { project, owner })
}

/// `assign SRC --project owner/repo [--as KEY]`: copy one vault key into the
/// named project's scope (#11015).
///
/// Why: D1-D5 of the #11015 plan — client-side only, the value moves inside
/// the server, and every assign stays inside one remote owner. Owner ruling
/// Dfx (2026-11-12): only a person or the Architect assigns across
/// projects; any other agent only into its own session's project.
/// What, in order, each refusal writing nothing: SRC ([`parse_source`]),
/// `--as` (or the source key), the backends and `--project` are validated
/// locally; the resolver maps the project to a checkout; `secrets.scopes`
/// for that checkout must report the named owner ([`CHECKOUT_MISMATCH`]),
/// and the source vault's owner ([`OTHER_OWNER`]); a [`Caller::Agent`]
/// must name the cwd checkout's own project ([`AGENT_OTHER_PROJECT`]; a cwd
/// the server cannot resolve refuses too). Then one `secrets.copy` with
/// `project` set to the checkout and `source` the vault key: the server's
/// agent deny, ticket and audit apply there, and a re-run replaces the
/// destination value as `copy` does. Prints `assign to VAULT` then
/// `report_copy`'s lines.
/// Test: `assign_copies_vault_key_into_project_without_printing_value`,
/// `assign_accepts_secret_ref_and_vault_key_forms`,
/// `assign_defaults_dest_to_source_key_and_honours_as`,
/// `assign_rerun_follows_copy_conflict_behaviour`,
/// `assign_unknown_project_is_a_fixed_error_and_writes_nothing`,
/// `assign_never_touches_clipboard`,
/// `assign_denied_source_is_refused_for_an_agent`,
/// `assign_by_an_agent_cannot_widen_its_write_reach`,
/// `assign_by_the_architect_reaches_another_project`,
/// `assign_to_another_owner_is_refused`,
/// `assign_to_an_unresolvable_checkout_is_refused`,
/// `assign_by_an_agent_from_an_unresolvable_cwd_is_refused`.
pub(crate) async fn assign(
    ctx: &Ctx<'_>,
    seam: Seam<'_>,
    args: AssignArgs<'_>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let fail = |e: SecretsError| anyhow!("tm secrets assign: {e}");
    let (vault, key) = parse_source(args.src)?;
    let dest = match args.dest {
        Some(dest) => SecretKey::new(dest).map_err(fail)?,
        None => key.clone(),
    };
    let from = BackendId::new(args.from).map_err(fail)?;
    let to = BackendId::new(args.to).map_err(fail)?;
    let project = ProjectName::parse(args.project)?;
    let dir = seam.resolver.checkout(&project)?;
    // #11015: the write names the resolved checkout, never the cwd.
    let target = Ctx {
        project: &dir,
        ..*ctx
    };
    let there = vaults(&target).await?;
    if there.owner != VaultName::owner(&project.owner) {
        bail!(CHECKOUT_MISMATCH);
    }
    if owner_vault_of(&vault).as_ref() != Some(&there.owner) {
        bail!(OTHER_OWNER);
    }
    // #11015 Dfx: only a person or the Architect reaches another project.
    if (seam.caller)() == Caller::Agent && vaults(ctx).await?.project != there.project {
        bail!(AGENT_OTHER_PROJECT);
    }
    let source = CopySource::vault(vault, key);
    let params =
        json!({ "from_backend": from, "to_backend": to, "keys": [dest], "source": source });
    let response: CopyResponse = target.call(method::COPY, params).await?;
    writeln!(out, "assign to {}", there.project)?;
    report_copy(&from, &to, &response, out)
}
