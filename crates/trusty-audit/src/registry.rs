//! The audit targets an operator has registered, and the file that holds them.
//!
//! Why: #5822. Before this, the only record of what an engagement audits was
//! `state/`[`crate::run::SELECTION_FILE`], which `crate::clone` writes as a
//! side effect of acquiring checkouts. That record has two limits. It cannot
//! express a JIRA project or a Linear team at all, and it only exists after a
//! clone has run — so there was no way to say "this engagement covers these
//! things" before, or independently of, getting source onto disk.
//!
//! This module is that record. Two properties define it:
//!
//! - **Additive.** Registering a target never disturbs the ones already there,
//!   and registering one twice is a no-op rather than a failure. An operator
//!   builds the set up over several invocations, which is how they actually
//!   work — not in one exhaustive command. [`register`] holds that against
//!   concurrent invocations too: the load-mutate-save runs under an exclusive
//!   lock, so two `taudit add` runs cannot discard each other's target.
//! - **Validated before persisted.** `crate::validate` reaches the target with
//!   the credential that will later read it, and a target that cannot be
//!   reached is refused at the moment it is registered. The alternative is
//!   finding out an hour into a sweep, which is the failure this exists to
//!   remove.
//!
//! ## Where the targets live (#5979)
//!
//! Owner ruling, 2026-08-18: `engagement.toml` DECLARES the targets, and
//! `state/`[`REGISTRY_FILE`] is a working copy rebuilt from it. Hand someone the
//! config and they have the key, the models and the scope — everything but the
//! clones. Before this the two facts lived apart: the config sat in the
//! recipient's directory and the target set sat inside the working directory,
//! alongside clones and tool binaries.
//!
//! Three consequences, each with a function that holds it:
//!
//! - [`engagement_targets`] is the one read. A config that declares a set wins;
//!   a config that declares none — including no config at all — falls back to
//!   the working copy, which is how an engagement registered before #5979 keeps
//!   every target it had. `targets = []` is a DECLARATION of zero and does not
//!   fall back.
//! - [`register`] and [`deregister`] write the CONFIG, then mirror the result
//!   into the working copy. That order is the fail-closed one: a config write
//!   that fails leaves both files untouched, so nothing can report a target as
//!   registered that the authoritative file does not carry.
//! - The config is written through [`crate::workdir::write_private_atomically`],
//!   so it keeps mode 0600. It carries the OpenRouter key, and a registration is
//!   now one of the paths that rewrites it.
//!
//! Nothing in the config replaces the working copy's `count` guard, and nothing
//! needs to. See [`REGISTRY_FILE`] for what protects each file.
//!
//! What: [`Target`], the two things that can be registered; [`Registry`], the
//! set and its `state/`[`REGISTRY_FILE`] persistence; [`engagement_targets`],
//! the authoritative read; and [`parse`], the one place a command-line spec
//! becomes a target.
//!
//! ## Relationship to the selection file
//!
//! This supersedes `selected-repos.toml` as the record of what an engagement
//! TARGETS. It does not replace it as the sweep's input: `crate::run` needs a
//! checkout path, which only `crate::clone` knows, so `clone` keeps writing the
//! selection and `run` keeps reading it, unchanged. [`legacy_selection`] is how
//! `taudit targets` names that file rather than leaving an operator with two
//! records and no statement of which is which.
//!
//! Test: `super::registry_tests`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trusty_common::file_lock::with_exclusive_lock;

use crate::clone;
use crate::config::{self, EngagementConfig};
use crate::error::AuditError;
use crate::local_repo;
use crate::run;
use crate::workdir::{self, Area, WorkDir};

/// File under `state/` holding the engagement's targets as a working copy.
///
/// Since #5979 this is DERIVED from `engagement.toml` rather than edited: it is
/// rebuilt from the config on every write, and read only when the config
/// declares no target set at all (see [`engagement_targets`]).
///
/// The same two obligations as [`crate::run::SELECTION_FILE`]: `count` is
/// declared ahead of the entries, and the file is written by rename
/// ([`workdir::write_atomically`]). A `count` that disagrees with the entries is
/// [`AuditError::TruncatedRegistry`], never a smaller set.
///
/// ## What protects the config's target list (#5979)
///
/// No `count`, and that is a decision rather than an omission. Three things
/// stand in its place, and one thing argues against adding it:
///
/// - Every write goes through [`crate::workdir::write_private_atomically`],
///   which creates a uniquely-named temporary with `O_EXCL` and renames it into
///   place. A reader observes the whole previous file or the whole new one —
///   there is no torn intermediate for a count to detect.
/// - [`crate::config::with_targets`] parses the rendered text back through
///   [`EngagementConfig::from_toml`] BEFORE the rename, so a substitution that
///   would produce an unloadable config fails without touching the file.
/// - `toml` renders `[[targets]]` ahead of `[tools]`, whose four pins are all
///   required. A truncation that loses the tail of the target list also loses a
///   required table, so the config refuses to load rather than reading as a
///   smaller-but-complete engagement. `config_tests::the_target_list_is_rendered_ahead_of_the_required_pins`
///   is what keeps that true.
///
/// Against: `engagement.toml` is the file a recipient reads and edits — that
/// readability is the transparency premise of the whole handoff (#5473). A
/// `count` they have to keep in step by hand turns adding one line to their own
/// file into an engagement that refuses to load, which is worse than the tear it
/// would guard. The working copy has no such reader, which is why it keeps its
/// count unchanged.
///
/// ```toml
/// # <work-dir>/state/audit-targets.toml
/// count = 2
///
/// [[targets]]
/// kind = "repo"
/// name_with_owner = "acme/api"
///
/// [[targets]]
/// kind = "board"
/// provider = "jira"
/// key = "ACME"
/// ```
///
/// No credential appears in this schema, and there is no field one could be
/// written into — the credential lives in the engagement config and is read at
/// validation time only.
pub const REGISTRY_FILE: &str = "audit-targets.toml";

/// How much to register, in the words every front end uses to ask.
///
/// Why: the sweep covers what is registered and nothing else, and it cannot
/// tell a repository the operator judged irrelevant from one they forgot. So
/// completeness is the operator's to supply, and a passive `add` verb leaves
/// the audit to degrade in silence. The wording is deliberately concrete:
/// an operator who would not describe their migrations repository as a
/// "relevant repository" still recognises "the repository holding your
/// database schema", and that is the omission the owner named.
///
/// What: one paragraph, wrapped by whoever prints it, naming the kinds that
/// get left out. It claims no detection — this client cannot see a target that
/// was never registered, and the text must never imply otherwise. It also
/// names no command, so the CLI, a chat wizard, or the Tauri shell can each
/// present it in their own idiom without re-authoring the substance.
///
/// The engagement is named neutrally on purpose. The client may be a company or
/// a single property, and more audit types are expected; today the wording is
/// pitched at a strategic technology assessment, which is the engagement shape
/// this client currently serves.
///
/// Naming that assessment's lens — how mature, how stable, how supportable and
/// how staffable the technology is — is what makes the breadth ask land. An
/// operator who reads only "register everything" hears an inventory chore; one
/// who reads what the assessment judges can see why a schema or infrastructure
/// repository changes the answer.
///
/// Test: `super::registry_tests::the_coverage_coaching_names_what_operators_leave_out`,
/// `super::registry_tests::the_coverage_coaching_states_the_assessment_lens`,
/// `super::registry_tests::the_coverage_coaching_claims_no_detection`.
pub const COVERAGE_COACHING: &str = "Register every repository and board this \
     engagement should cover — applications, the repository holding your database \
     schema or migrations, infrastructure and IaC, shared libraries and config \
     repositories, and every ticketing board in use. The assessment judges how \
     mature, how stable and how supportable the technology is, and it judges only \
     what you register, so anything left unregistered is simply absent from that \
     picture of the organization's technology estate.";

/// Which kind of target a command asked to register.
///
/// Why: `taudit add repo` and `taudit add board` are separate verbs, so the
/// caller's intent is known and does not have to be guessed from the spelling.
/// Passing it through means `add repo jira:ACME` is refused rather than
/// silently registering a board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetKind {
    /// A GitHub repository.
    Repo,
    /// A JIRA project or a Linear team.
    Board,
}

/// A board provider this client can validate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BoardProvider {
    /// JIRA Cloud, addressed by project key.
    Jira,
    /// Linear, addressed by team key or team id.
    Linear,
}

impl BoardProvider {
    /// The prefix an operator types, and the name that appears in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            BoardProvider::Jira => "jira",
            BoardProvider::Linear => "linear",
        }
    }

    /// The engagement-config field carrying this provider's credential.
    ///
    /// Named in [`AuditError::BoardCredentialMissing`] so an operator whose
    /// config says nothing about a provider is told exactly what to set.
    pub fn config_field(self) -> &'static str {
        match self {
            BoardProvider::Jira => "boards.jira",
            BoardProvider::Linear => "boards.linear",
        }
    }

    fn from_prefix(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "jira" => Some(BoardProvider::Jira),
            "linear" => Some(BoardProvider::Linear),
            _ => None,
        }
    }
}

/// One thing an engagement audits.
///
/// Why: the registry holds repositories and boards in one list because they are
/// one set from the operator's side — "what this engagement covers" — and
/// splitting them into two files would give `taudit targets` two answers.
/// What: an internally-tagged enum, so the TOML reads as `kind = "repo"` rather
/// than as a nested table an operator has to decode.
/// Test: `super::registry_tests::a_registry_round_trips_both_kinds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Target {
    /// A GitHub repository, validated with the recipient's `gh` credential.
    Repo {
        /// `owner/name`, the identity `gh` and [`crate::clone`] both take.
        name_with_owner: String,
    },
    /// A repository already on the operator's disk, validated by reading it
    /// (#6001).
    ///
    /// Why a variant rather than a `Repo` whose name happens to be a path: the
    /// two are validated differently (a GitHub probe versus
    /// [`crate::local_repo::inspect`]), acquired differently (`gh repo clone`
    /// versus `git clone <path>`), and one of them must never have its
    /// `owner/name` charset applied to it. Making that a field would put the
    /// same fork inside every reader instead of at the one match arm.
    LocalRepo {
        /// The absolute path to the checkout, with trailing separators trimmed.
        ///
        /// Read only, ever — see [`crate::local_repo`] for the invariant and the
        /// test that holds it.
        path: PathBuf,
    },
    /// A JIRA project or Linear team, validated with the configured credential.
    Board {
        /// Which provider.
        provider: BoardProvider,
        /// Project key, team key, or team id.
        key: String,
    },
}

impl Target {
    /// The canonical spelling: `owner/name`, an absolute path, or
    /// `provider:key`.
    ///
    /// This is what an operator types into `taudit remove`, and what the CLI
    /// prints, so the two are the same string by construction. It is also what
    /// [`crate::chain`] hands to [`crate::clone::clone_all`] — a local target's
    /// id is its path, which is exactly the spec that acquisition takes.
    pub fn id(&self) -> String {
        match self {
            Target::Repo { name_with_owner } => name_with_owner.clone(),
            Target::LocalRepo { path } => path.display().to_string(),
            Target::Board { provider, key } => format!("{}:{key}", provider.as_str()),
        }
    }

    /// Which kind this is.
    ///
    /// A local checkout reports [`TargetKind::Repo`]: it is a repository the
    /// engagement audits, and every count, filter and clone-versus-collect
    /// decision in this crate wants it on that side of the line. The kind is the
    /// VERB an operator used, and there is one verb for both — see [`parse`].
    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Repo { .. } | Target::LocalRepo { .. } => TargetKind::Repo,
            Target::Board { .. } => TargetKind::Board,
        }
    }

    /// Whether two targets name the same thing.
    ///
    /// Why: ASCII-case-insensitive, because GitHub owners and repository names,
    /// JIRA project keys and Linear team keys are all case-insensitive at their
    /// own APIs. `taudit add repo Acme/API` after `acme/api` must be the
    /// idempotent no-op the operator expects, not a second entry that then
    /// clones twice.
    /// What: compares [`Target::id`] with `eq_ignore_ascii_case`. The spelling
    /// the operator used is what gets STORED — only matching ignores case.
    /// Test: `super::registry_tests::case_differences_do_not_create_a_second_entry`.
    pub fn same_as(&self, other: &Target) -> bool {
        self.kind() == other.kind() && self.id().eq_ignore_ascii_case(&other.id())
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id())
    }
}

/// The shape a board spec must have, quoted back on a refusal.
const BOARD_SHAPE: &str = "expected jira:<PROJECT-KEY> or linear:<TEAM-KEY-or-ID>";

/// The longest team key tga's own extractor can produce. See
/// [`is_linear_team_key`].
const LINEAR_TEAM_KEY_MAX: usize = 10;

/// Whether a Linear board key is one the sweep can actually collect against.
///
/// Why: #5982. `crate::validate` accepts a team's short key or its internal id,
/// and only the short key ever reaches a collected issue — tga matches
/// `linear.team_keys` against the text before the hyphen in an identifier like
/// `ENG-1234`. A registered id validated green and collected nothing, and
/// nothing said so, because from the collector's side a team with no matching
/// issues looks exactly like a team with no issues. This is the one place that
/// question is answered, so `crate::validate` (which resolves an id to the key
/// before it is persisted) and `crate::run::boards` (which states the gap when a
/// key stored earlier cannot collect) cannot drift apart.
///
/// What: `[A-Z][A-Z0-9]{0,9}`, the prefix shape
/// `tga::classify::sources::linear::linear_key_regex` can capture. A team id
/// fails it on all three counts — 36 characters, lowercase hex, and hyphens.
///
/// Test: `super::registry_tests::a_team_id_is_not_a_key_the_sweep_can_collect`.
pub fn is_linear_team_key(key: &str) -> bool {
    (1..=LINEAR_TEAM_KEY_MAX).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Turn a command-line spec into a target.
///
/// Why: the one place argv becomes a [`Target`], so the CLI and the Tauri shell
/// cannot diverge on what `acme/api` means. It is here rather than in
/// [`crate::cli`] because `Cli::to_command` is a total function — a spec that is
/// not a target has to fail somewhere that can return an error, and that is
/// `Session::execute`.
/// What: `kind` is the verb the operator used; `None` accepts either shape and
/// is what `taudit remove` passes. A repository name is validated by
/// [`clone::split_name`], the one place this crate decides that charset —
/// registering a name `taudit clone` would later refuse is not a useful state to
/// be able to reach.
///
/// #6001: the `repo` verb takes TWO shapes, and [`local_repo::is_local_spec`]
/// decides which — an ABSOLUTE path is a checkout on disk, anything else is a
/// GitHub `owner/repo`. One verb rather than two because the operator's intent
/// is the same in both cases ("audit this repository"), and because the two
/// spellings cannot overlap: `clone::split_name` refuses an empty owner, so no
/// `owner/repo` is ever absolute. Nothing here touches the filesystem — whether
/// the path is USABLE is [`crate::validate`]'s question, asked where it can be
/// refused with the condition that failed.
/// Test: `super::registry_tests::a_spec_that_is_neither_shape_is_refused`,
/// `super::registry_tests::a_traversing_repo_name_is_never_registered`,
/// `super::registry_tests::an_absolute_path_registers_as_a_local_checkout`.
///
/// # Errors
///
/// [`AuditError::InvalidRepoName`] for a repository spec that is not a plain
/// `owner/name`, and [`AuditError::InvalidTarget`] for a board spec that names
/// no known provider or carries an unusable key.
pub fn parse(kind: Option<TargetKind>, spec: &str) -> Result<Target, AuditError> {
    let spec = spec.trim();
    // #6001: a path is checked for FIRST, so a path holding a colon is still a
    // path rather than a board with an unknown provider.
    let local = local_repo::is_local_spec(spec);
    let wanted = kind.unwrap_or(if !local && spec.contains(':') {
        TargetKind::Board
    } else {
        TargetKind::Repo
    });
    match wanted {
        TargetKind::Repo if local => Ok(Target::LocalRepo {
            path: local_repo::normalize(spec),
        }),
        TargetKind::Repo => {
            clone::split_name(spec)?;
            Ok(Target::Repo {
                name_with_owner: spec.to_owned(),
            })
        }
        TargetKind::Board => parse_board(spec),
    }
}

fn parse_board(spec: &str) -> Result<Target, AuditError> {
    let reject = || AuditError::InvalidTarget {
        spec: spec.to_owned(),
        expected: BOARD_SHAPE,
    };
    let (prefix, key) = spec.split_once(':').ok_or_else(reject)?;
    let provider = BoardProvider::from_prefix(prefix).ok_or_else(reject)?;
    let key = key.trim();
    // The key becomes part of a URL path (JIRA) or a GraphQL filter (Linear),
    // so the charset is closed rather than merely non-empty.
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(reject());
    }
    Ok(Target::Board {
        provider,
        key: key.to_owned(),
    })
}

/// The `state/audit-targets.toml` document. `count` is the truncation check.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    count: usize,
    #[serde(default)]
    targets: Vec<Target>,
}

/// The set of registered targets.
///
/// Why: a value rather than a file handle, so adding and removing are pure and
/// the one write happens at a point the caller chose — which is what lets a
/// refused validation leave the file untouched.
/// What: an ordered list, de-duplicated by [`Target::same_as`]. Registration
/// order is preserved because it is the order the operator built the set in.
/// Test: `super::registry_tests`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    targets: Vec<Target>,
}

impl Registry {
    /// Where the registry lives.
    pub fn path(work: &WorkDir) -> PathBuf {
        work.path(Area::State).join(REGISTRY_FILE)
    }

    /// Read the registry, or an empty one when nothing has been registered.
    ///
    /// Why: absence is the ordinary first state, so it is `Ok` with nothing in
    /// it — unlike [`crate::run::load_selection`], where absence means a sweep
    /// would audit nothing and must refuse. A file that is PRESENT and wrong
    /// still fails: a truncated write reads as a smaller-but-complete set, and
    /// acting on that would drop targets the operator registered.
    /// What: parses `state/`[`REGISTRY_FILE`] and checks `count`.
    /// Test: `super::registry_tests::an_absent_registry_is_empty_not_an_error`,
    /// `super::registry_tests::a_truncated_registry_is_refused`.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] for a read failure other than absence,
    /// [`AuditError::Parse`] when the file does not match the schema, and
    /// [`AuditError::TruncatedRegistry`] when `count` disagrees with the
    /// entries.
    pub fn load(work: &WorkDir) -> Result<Self, AuditError> {
        let path = Self::path(work);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(AuditError::Read { path, source }),
        };
        let document: Document = toml::from_str(&text).map_err(|source| AuditError::Parse {
            path: path.clone(),
            what: "audit target registry",
            source: Box::new(source),
        })?;
        if document.count != document.targets.len() {
            return Err(AuditError::TruncatedRegistry {
                path,
                declared: document.count,
                found: document.targets.len(),
            });
        }
        Ok(Self {
            targets: document.targets,
        })
    }

    /// Everything registered, in registration order.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// Whether this target is already registered.
    pub fn contains(&self, target: &Target) -> bool {
        self.targets.iter().any(|t| t.same_as(target))
    }

    /// Add a target to this in-memory set. `false` when it was already there.
    ///
    /// This appends and never rewrites what is already in the list. That makes
    /// the VALUE additive; it does not make the FILE additive, because a second
    /// process loading the same snapshot would still save over this one's work.
    /// [`register`] is the entry point that holds the additive property against
    /// concurrent writers, and the one every caller uses.
    pub fn insert(&mut self, target: Target) -> bool {
        if self.contains(&target) {
            return false;
        }
        self.targets.push(target);
        true
    }

    /// Drop a target. `false` when it was not registered.
    pub fn remove(&mut self, target: &Target) -> bool {
        let before = self.targets.len();
        self.targets.retain(|t| !t.same_as(target));
        before != self.targets.len()
    }

    /// Write the registry so no reader can observe a partial one.
    ///
    /// # Errors
    ///
    /// [`AuditError::WorkDir`] when the document cannot be rendered, the
    /// temporary file cannot be written, or the rename fails.
    pub fn save(&self, work: &WorkDir) -> Result<(), AuditError> {
        let path = Self::path(work);
        let document = Document {
            count: self.targets.len(),
            targets: self.targets.clone(),
        };
        let text = toml::to_string_pretty(&document).map_err(|e| AuditError::WorkDir {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        workdir::write_atomically(&path, &text)
    }
}

/// The engagement's targets: what the config declares, or the working copy.
///
/// Why: #5979 makes `engagement.toml` authoritative, and an operator who
/// registered ten repositories yesterday must not find zero today. The two
/// states a config can be in are therefore different answers, not one:
///
/// - `Some(declared)` — this file has said what the engagement covers, and that
///   is the answer. `Some(&[])` included: an operator who removed their last
///   target declared zero, and resurrecting the old working copy there would
///   undo the removal they just made.
/// - `None`, which covers a config written before #5979 AND no config at all —
///   nothing has ever declared a set, so `state/`[`REGISTRY_FILE`] is adopted.
///   That is the migration: an upgrading engagement keeps every target it had,
///   and the first [`register`] or [`deregister`] persists the adopted set into
///   the config, after which the config answers on its own.
///
/// The registry disagreeing with the config is expected rather than an error —
/// a working copy left by an earlier run is exactly what a declared set
/// supersedes.
/// What: a plain read. Nothing is written here, so reporting state never mutates
/// an engagement.
/// Test: `super::registry_tests::an_undeclared_config_adopts_the_existing_registry`,
/// `super::registry_tests::a_declared_empty_list_is_not_a_missing_one`,
/// `super::registry_tests::a_declared_list_supersedes_a_stale_working_copy`.
///
/// # Errors
///
/// Whatever [`Registry::load`] fails with, on the adoption path only.
pub fn engagement_targets(
    config: Option<&EngagementConfig>,
    work: &WorkDir,
) -> Result<Vec<Target>, AuditError> {
    match config.and_then(EngagementConfig::declared_targets) {
        Some(declared) => Ok(declared.to_vec()),
        None => Ok(Registry::load(work)?.targets().to_vec()),
    }
}

/// Declare `target` in the engagement config, serialised against every writer.
///
/// Why: #5822 for the lock, #5979 for the file. The read-modify-write moved from
/// `state/audit-targets.toml` to `engagement.toml` and is no less a
/// read-modify-write for having moved: two `taudit add` runs against one
/// engagement each load the same snapshot, each append their own target, and the
/// later save discards the earlier one's while both report success.
/// [`crate::workdir::write_private_atomically`] does not close that — it makes
/// one write untearable, not a load-mutate-save atomic.
/// What: the whole critical section under
/// [`trusty_common::file_lock::with_exclusive_lock`], the workspace's one
/// implementation of it (#5344), now guarding the config. Returns whether this
/// call is the one that appended; `false` means another writer had registered
/// the same target first, which is the ordinary idempotent no-op.
/// Test: `super::registry_tests::concurrent_registrations_keep_every_target`,
/// `super::registry_tests::a_registration_keeps_the_config_owner_only`.
///
/// Callers validate BEFORE calling this. Validation reaches the network under a
/// 30-second ceiling, and the lock is not reentrant — holding it across a
/// request would stall every other `add` for this engagement behind one
/// unreachable site. Re-reading the config here is what keeps that safe: the
/// append is decided against the snapshot that is current at write time, not the
/// one the caller validated against.
///
/// # Errors
///
/// [`AuditError::RegistryLock`] when the lock cannot be taken — never a
/// bypass — [`AuditError::NoEngagementConfig`] when there is no config to
/// declare anything in, plus whatever [`update`] fails with.
pub fn register(config_path: &Path, work: &WorkDir, target: &Target) -> Result<bool, AuditError> {
    locked(config_path, || {
        update(config_path, work, |registry| {
            registry.insert(target.clone())
        })
    })
}

/// Drop `target` from the engagement config, under the lock [`register`] takes.
///
/// Why: a removal is the same read-modify-write, so an unserialised one loses a
/// concurrent add just as readily (#5822).
/// What: returns whether the target was there to remove. Nothing is written when
/// it was not.
/// Test: `super::registry_tests::a_removal_holds_the_same_lock_an_add_does`.
///
/// # Errors
///
/// The same set as [`register`].
pub fn deregister(config_path: &Path, work: &WorkDir, target: &Target) -> Result<bool, AuditError> {
    locked(config_path, || {
        update(config_path, work, |registry| registry.remove(target))
    })
}

/// One read-modify-write over the engagement's target set. Caller holds the lock.
///
/// Why: the fail-closed order is the whole content of this function, so it is
/// written once rather than twice (#5979). The config is published FIRST and the
/// working copy mirrored SECOND, so a config write that fails leaves both files
/// exactly as they were — "the config write failed but the target looks
/// registered" is unreachable, in either file. A mirror write that fails is
/// still an error the caller sees; the config is authoritative by then, and the
/// next read rebuilds the mirror.
/// What: reads the config as TEXT as well as parsing it, because
/// [`crate::config::with_targets`] substitutes over the operator's own file
/// rather than re-rendering one from the schema — that is what keeps a
/// registration from dropping a field a newer generator added.
/// Test: `super::registry_tests::a_config_that_cannot_be_written_registers_nothing`,
/// `super::registry_tests::registering_without_a_config_is_refused`.
///
/// # Errors
///
/// [`AuditError::NoEngagementConfig`] when the config is absent,
/// [`AuditError::Read`] for any other read failure, [`AuditError::Parse`] or
/// [`AuditError::Render`] from the substitution, and [`AuditError::WorkDir`]
/// when either file cannot be published.
fn update(
    config_path: &Path,
    work: &WorkDir,
    mutate: impl FnOnce(&mut Registry) -> bool,
) -> Result<bool, AuditError> {
    let text = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuditError::NoEngagementConfig {
                path: config_path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(AuditError::Read {
                path: config_path.to_path_buf(),
                source,
            });
        }
    };
    let config = EngagementConfig::from_toml(&text, config_path)?;
    let mut registry = Registry {
        targets: engagement_targets(Some(&config), work)?,
    };
    if !mutate(&mut registry) {
        return Ok(false);
    }
    let rendered = config::with_targets(&text, registry.targets(), config_path)?;
    // 0600: the file this now rewrites carries the OpenRouter key (#5868).
    workdir::write_private_atomically(config_path, &rendered)?;
    registry.save(work)?;
    Ok(true)
}

/// Run `f` holding the engagement config's exclusive lock.
///
/// The lock guards `config_path` through its `.lock` sidecar, so it survives the
/// rename [`crate::workdir::write_private_atomically`] publishes with.
fn locked<T>(
    config_path: &Path,
    f: impl FnOnce() -> Result<T, AuditError>,
) -> Result<T, AuditError> {
    with_exclusive_lock(config_path, f).map_err(|source| AuditError::RegistryLock {
        path: config_path.to_path_buf(),
        source,
    })?
}

/// The repository selection `crate::clone` wrote, when there is one.
///
/// Why: the registry supersedes that file as the record of what an engagement
/// targets, and the sweep still reads it as the record of what is on disk. An
/// operator holding both files needs to be told which is which — printing the
/// registry alone would read as though their cloned repositories had vanished.
/// Nothing is copied across: a selection entry carries a checkout path, and
/// inventing a target from it would claim a validation that never happened.
/// What: the path and how many repositories it lists, or `None` when no sweep
/// input exists. A selection that is present and truncated still fails, for the
/// same reason it fails for the sweep.
/// Test: `super::registry_tests::the_listing_names_a_selection_file_it_did_not_write`.
///
/// # Errors
///
/// Whatever [`run::load_selection`] fails with, other than an absent or empty
/// selection, which is `Ok(None)`.
pub fn legacy_selection(work: &WorkDir) -> Result<Option<(PathBuf, usize)>, AuditError> {
    match run::load_selection(work) {
        Ok(repos) => Ok(Some((run::selection_path(work), repos.len()))),
        Err(AuditError::NoRepositoriesSelected { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// What one `add` did.
///
/// `already_registered` is the idempotency signal: a second `add` of the same
/// target reports `true`, changes nothing, and does not re-validate — a
/// registration that already passed must not start failing on a network blip.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Registration {
    /// What is now registered.
    pub target: Target,
    /// Whether it was already there before this call.
    pub already_registered: bool,
}

/// What one `remove` did. `was_registered` is `false` for a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Removal {
    /// What was asked for.
    pub target: Target,
    /// Whether it was registered before this call.
    pub was_registered: bool,
}

/// Everything `taudit targets` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetList {
    /// The registered targets, in registration order.
    pub targets: Vec<Target>,
    /// The sweep's own repository selection, when `crate::clone` left one.
    pub legacy_selection: Option<(PathBuf, usize)>,
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn work_in(dir: &std::path::Path) -> WorkDir {
        WorkDir::new(dir.join("work"))
    }

    fn repo(name: &str) -> Target {
        Target::Repo {
            name_with_owner: name.to_owned(),
        }
    }

    fn board(provider: BoardProvider, key: &str) -> Target {
        Target::Board {
            provider,
            key: key.to_owned(),
        }
    }

    /// The line between the two things `linear:<key>` has always accepted
    /// (#5982). A team id is a legal spec and a readable team; it is simply not
    /// a key any collected issue can carry.
    #[test]
    fn a_team_id_is_not_a_key_the_sweep_can_collect() {
        for collectable in ["ENG", "FE", "A", "X9", "ABCDEFGHIJ"] {
            assert!(is_linear_team_key(collectable), "{collectable}");
        }
        for not in [
            "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "eng",
            "ENG-1",
            "ENG_X",
            "9ENG",
            "ABCDEFGHIJK",
            "",
        ] {
            assert!(!is_linear_team_key(not), "{not}");
        }
    }

    /// An engagement config that declares no targets — the state every case
    /// here starts from unless it says otherwise.
    const ENGAGEMENT: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    /// Write [`ENGAGEMENT`] beside the working directory and hand back its path.
    fn engagement_in(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("engagement.toml");
        std::fs::write(&path, ENGAGEMENT).expect("seed an engagement config");
        path
    }

    /// What the engagement config declares, by id, in declaration order.
    fn declared_ids(config_path: &std::path::Path) -> Vec<String> {
        EngagementConfig::load(config_path)
            .expect("the config reads")
            .declared_targets()
            .expect("the config declares a set")
            .iter()
            .map(Target::id)
            .collect()
    }

    /// The owner named schema repositories specifically, because that is the
    /// kind operators forget. Abstract phrasing ("relevant repositories") is
    /// what the concrete list exists to replace, so the kinds are asserted
    /// rather than left to whoever next edits the string.
    #[test]
    fn the_coverage_coaching_names_what_operators_leave_out() {
        let text = COVERAGE_COACHING.to_lowercase();
        for kind in [
            "schema",
            "migrations",
            "infrastructure",
            "shared librar",
            "config repositor",
            "board",
        ] {
            assert!(
                text.contains(kind),
                "coaching omits {kind}: {COVERAGE_COACHING}"
            );
        }
    }

    /// Breadth without a reason reads as an inventory chore. The coaching says
    /// what the assessment judges, so an operator can tell why a schema or
    /// infrastructure repository changes the answer.
    #[test]
    fn the_coverage_coaching_states_the_assessment_lens() {
        let text = COVERAGE_COACHING.to_lowercase();
        for lens in ["mature", "stable", "supportable"] {
            assert!(
                text.contains(lens),
                "coaching omits the {lens} lens: {COVERAGE_COACHING}"
            );
        }
    }

    /// The coaching must not overstate what this client can see. Nothing here
    /// can observe an unregistered target, so any claim of detection would be
    /// false — that claim belongs only to a stage reading collected data.
    #[test]
    fn the_coverage_coaching_claims_no_detection() {
        let text = COVERAGE_COACHING.to_lowercase();
        for overclaim in [
            "detect",
            "finds missing",
            "will discover",
            "automatically add",
        ] {
            assert!(
                !text.contains(overclaim),
                "coaching claims {overclaim}: {COVERAGE_COACHING}"
            );
        }
    }

    /// #5885: whoever prints this appends nothing to it, so the paragraph has to
    /// close itself. It is the last line of the guided card, and a trailing
    /// colon there reads as a list that never arrives.
    #[test]
    fn the_coverage_coaching_closes_its_own_sentence() {
        assert!(COVERAGE_COACHING.ends_with('.'), "{COVERAGE_COACHING}");
        assert!(!COVERAGE_COACHING.ends_with(":."), "{COVERAGE_COACHING}");
    }

    #[test]
    fn a_registry_round_trips_both_kinds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let mut registry = Registry::default();
        assert!(registry.insert(repo("acme/api")));
        assert!(registry.insert(board(BoardProvider::Jira, "ACME")));
        assert!(registry.insert(board(BoardProvider::Linear, "ENG")));
        registry.save(&work).expect("writes");

        assert_eq!(Registry::load(&work).expect("reads back"), registry);
        let ids: Vec<String> = registry.targets().iter().map(Target::id).collect();
        assert_eq!(ids, vec!["acme/api", "jira:ACME", "linear:ENG"]);
    }

    /// The truncation contract: `count` is declared ahead of the entries, so a
    /// crashed write loses entries and keeps the count.
    #[test]
    fn a_saved_registry_declares_its_count_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut registry = Registry::default();
        registry.insert(repo("acme/api"));
        registry.insert(repo("acme/web"));
        registry.save(&work).expect("writes");

        let text = std::fs::read_to_string(Registry::path(&work)).expect("read");
        assert!(text.starts_with("count = 2"), "{text}");
        assert!(
            text.find("count").expect("count") < text.find("[[targets]]").expect("targets"),
            "{text}"
        );
    }

    /// How many writers race in the two concurrency tests. Enough that an
    /// unlocked load-mutate-save loses several, not just one.
    const WRITERS: usize = 16;

    /// Start every writer at the same instant, so their critical sections
    /// overlap rather than queueing behind thread spawn.
    fn race(work: &WorkDir, each: impl Fn(&WorkDir, usize) + Sync) {
        let gate = std::sync::Barrier::new(WRITERS);
        std::thread::scope(|scope| {
            for n in 0..WRITERS {
                let (gate, each) = (&gate, &each);
                scope.spawn(move || {
                    gate.wait();
                    each(work, n);
                });
            }
        });
    }

    fn registered_ids(work: &WorkDir) -> Vec<String> {
        let mut ids: Vec<String> = Registry::load(work)
            .expect("the registry reads")
            .targets()
            .iter()
            .map(Target::id)
            .collect();
        ids.sort();
        ids
    }

    /// The lost update `register` exists to remove (#5822): several `taudit
    /// add` runs against one working directory each load the same snapshot,
    /// each append their own target, and the later save discards the earlier
    /// one's while BOTH report success.
    ///
    /// Completeness is what is asserted, not merely that the file is untorn —
    /// `run_tests::racing_writers_never_leave_a_torn_selection` covers untorn,
    /// and a whole file holding four of sixteen targets still passes it.
    #[test]
    fn concurrent_registrations_keep_every_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config = engagement_in(tmp.path());

        race(&work, |work, n| {
            assert!(
                register(&config, work, &repo(&format!("acme/repo-{n:02}")))
                    .expect("a racing add succeeds"),
                "acme/repo-{n:02} was reported as already registered"
            );
        });

        let mut expected: Vec<String> = (0..WRITERS).map(|n| format!("acme/repo-{n:02}")).collect();
        expected.sort();
        // #5979: the CONFIG is what must be complete — it is what the sweep
        // reads. The working copy is asserted alongside it, because a mirror
        // that fell behind is a mirror nothing rebuilt.
        let mut declared = declared_ids(&config);
        declared.sort();
        assert_eq!(
            declared, expected,
            "a concurrently-registered target was discarded from the engagement"
        );
        assert_eq!(
            registered_ids(&work),
            expected,
            "the working copy did not track the engagement"
        );
    }

    /// The same read-modify-write, so the same lock: an unserialised removal
    /// writes back a snapshot still holding the targets other writers dropped.
    #[test]
    fn a_removal_holds_the_same_lock_an_add_does() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config = engagement_in(tmp.path());
        let seeded: Vec<Target> = (0..WRITERS)
            .map(|n| repo(&format!("acme/repo-{n:02}")))
            .collect();
        std::fs::write(
            &config,
            config::with_targets(ENGAGEMENT, &seeded, &config).expect("declares"),
        )
        .expect("seed the declared set");

        race(&work, |work, n| {
            assert!(
                deregister(&config, work, &repo(&format!("acme/repo-{n:02}")))
                    .expect("a racing remove"),
                "acme/repo-{n:02} was reported as not registered"
            );
        });

        assert!(
            declared_ids(&config).is_empty(),
            "a concurrent removal was undone: {:?}",
            declared_ids(&config)
        );
        assert!(registered_ids(&work).is_empty());
    }

    /// Registering the same target from every writer at once: exactly one call
    /// reports the append, and the file carries one entry.
    #[test]
    fn only_one_racing_writer_claims_a_duplicate_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config = engagement_in(tmp.path());
        let appended = std::sync::atomic::AtomicUsize::new(0);

        race(&work, |work, _| {
            if register(&config, work, &repo("acme/api")).expect("a racing add succeeds") {
                appended.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });

        assert_eq!(appended.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(declared_ids(&config), vec!["acme/api"]);
        assert_eq!(registered_ids(&work), vec!["acme/api"]);
    }

    /// 🔴 #5979's migration, stated as the case that produced it: an operator
    /// registered ten repositories before this change, so the working copy holds
    /// them and their config declares nothing. Every read must still answer ten.
    #[test]
    fn an_undeclared_config_adopts_the_existing_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config_path = engagement_in(tmp.path());
        let mut legacy = Registry::default();
        for n in 0..10 {
            legacy.insert(repo(&format!("acme/repo-{n}")));
        }
        legacy.save(&work).expect("the pre-#5979 registry");

        let config = EngagementConfig::load(&config_path).expect("loads");
        assert_eq!(config.declared_targets(), None, "nothing is declared yet");
        assert_eq!(
            engagement_targets(Some(&config), &work)
                .expect("reads")
                .len(),
            10,
            "an upgraded engagement must not read as empty"
        );

        // The first write persists the adopted set, after which the config
        // answers on its own.
        assert!(register(&config_path, &work, &repo("acme/eleventh")).expect("registers"));
        let declared = declared_ids(&config_path);
        assert_eq!(declared.len(), 11, "{declared:?}");
        assert_eq!(declared[10], "acme/eleventh");
        assert_eq!(declared[0], "acme/repo-0");
    }

    /// The other half of that decision: an operator who removed their last
    /// target declared ZERO, and the old working copy must not resurrect it.
    #[test]
    fn a_declared_empty_list_is_not_a_missing_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config_path = engagement_in(tmp.path());
        let mut stale = Registry::default();
        stale.insert(repo("acme/deleted"));
        stale.save(&work).expect("a stale working copy");

        std::fs::write(
            &config_path,
            config::with_targets(ENGAGEMENT, &[], &config_path).expect("declares zero"),
        )
        .expect("write");

        let config = EngagementConfig::load(&config_path).expect("loads");
        assert_eq!(config.declared_targets(), Some(&[][..]));
        assert!(
            engagement_targets(Some(&config), &work)
                .expect("reads")
                .is_empty(),
            "a declared-empty engagement adopted a deleted target"
        );
    }

    /// A working copy left by an earlier run is expected, not an error: the
    /// declared set supersedes it without complaint, and the next write rebuilds
    /// the mirror.
    #[test]
    fn a_declared_list_supersedes_a_stale_working_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config_path = engagement_in(tmp.path());
        let mut stale = Registry::default();
        stale.insert(repo("acme/from-last-week"));
        stale.save(&work).expect("a stale working copy");

        std::fs::write(
            &config_path,
            config::with_targets(ENGAGEMENT, &[repo("acme/api")], &config_path).expect("declares"),
        )
        .expect("write");

        let config = EngagementConfig::load(&config_path).expect("loads");
        assert_eq!(
            engagement_targets(Some(&config), &work)
                .expect("reads")
                .iter()
                .map(Target::id)
                .collect::<Vec<_>>(),
            vec!["acme/api"]
        );

        assert!(register(&config_path, &work, &repo("acme/web")).expect("registers"));
        assert_eq!(
            registered_ids(&work),
            vec!["acme/api", "acme/web"],
            "the mirror was not rebuilt from the declared set"
        );
    }

    /// 🔴 The file a registration now rewrites carries the OpenRouter key, so it
    /// must come back at 0600 — not at whatever the process umask allows.
    ///
    /// Seeded at 0644 on purpose: the assertion has to prove this write NARROWS
    /// the file, not that it happened to inherit a mode from somewhere.
    #[cfg(unix)]
    #[test]
    fn a_registration_keeps_the_config_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config = engagement_in(tmp.path());
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        assert!(register(&config, &work, &repo("acme/api")).expect("registers"));
        let mode = std::fs::metadata(&config)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);

        // And the credential survived the substitution — a registration that
        // dropped the key would leave the engagement unable to run.
        let text = std::fs::read_to_string(&config).expect("read");
        assert!(text.contains("sk-or-v1-not-a-real-key"), "{text}");

        assert!(deregister(&config, &work, &repo("acme/api")).expect("removes"));
        let mode = std::fs::metadata(&config)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a removal widened it: {:o}",
            mode & 0o777
        );
    }

    /// 🔴 The fail-open arm. A config that cannot be published must leave BOTH
    /// files as they were — "the config write failed but the target looks
    /// registered" is the shape this ordering exists to rule out.
    ///
    /// The write is made to fail by making the config's directory unwritable, so
    /// the temporary file cannot be created. The `.lock` sidecar is created
    /// FIRST: without it the lock itself cannot be opened and the refusal
    /// arrives as [`AuditError::RegistryLock`], which proves a different arm
    /// than the one under test. Skipped as root, for whom mode bits are
    /// advisory.
    #[cfg(unix)]
    #[test]
    fn a_config_that_cannot_be_written_registers_nothing() {
        use std::os::unix::fs::PermissionsExt as _;

        // SAFETY: `geteuid` reads this process's own effective uid and writes
        // through no pointer.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let held = tmp.path().join("held");
        std::fs::create_dir(&held).expect("mkdir");
        let config = engagement_in(&held);
        let before = std::fs::read_to_string(&config).expect("read");
        std::fs::write(trusty_common::file_lock::lock_path(&config), b"").expect("seed the lock");
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        let err = register(&config, &work, &repo("acme/api"))
            .expect_err("an unwritable engagement must not read as a registration");
        assert!(matches!(err, AuditError::WorkDir { .. }), "{err:?}");

        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            before,
            "the engagement was modified by a failed write"
        );
        assert!(
            !Registry::path(&work).exists(),
            "the working copy claims a target the engagement does not declare"
        );

        // Let the tempdir clean up after itself.
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");
    }

    /// 🔴 There is nowhere to declare a target without an engagement, so this is
    /// a refusal rather than a write to the working copy — which nothing now
    /// treats as authoritative.
    #[test]
    fn registering_without_a_config_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let missing = tmp.path().join("engagement.toml");

        for outcome in [
            register(&missing, &work, &repo("acme/api")),
            deregister(&missing, &work, &repo("acme/api")),
        ] {
            let err = outcome.expect_err("no engagement, nothing to declare");
            let AuditError::NoEngagementConfig { path } = &err else {
                panic!("expected NoEngagementConfig, got {err:?}");
            };
            assert_eq!(path, &missing);
            assert!(err.to_string().contains("nothing was registered"), "{err}");
        }
        assert!(!Registry::path(&work).exists());
        assert!(!missing.exists(), "a refusal must not create a config");
    }

    /// A config that is present and malformed is not an absent one: a write
    /// that would overwrite a hand-edited engagement is refused instead.
    #[test]
    fn a_malformed_config_is_not_overwritten_by_a_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let config = tmp.path().join("engagement.toml");
        std::fs::write(&config, "this is not toml = = =").expect("write");

        let err = register(&config, &work, &repo("acme/api"))
            .expect_err("a malformed engagement must not be rewritten");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "this is not toml = = ="
        );
    }

    /// Absence of a config is absence of a declaration, so a directory that is
    /// not an engagement still reports whatever working copy it has.
    #[test]
    fn no_config_at_all_reads_the_working_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut legacy = Registry::default();
        legacy.insert(board(BoardProvider::Linear, "ENG"));
        legacy.save(&work).expect("writes");

        assert_eq!(
            engagement_targets(None, &work)
                .expect("reads")
                .iter()
                .map(Target::id)
                .collect::<Vec<_>>(),
            vec!["linear:ENG"]
        );
    }

    #[test]
    fn an_absent_registry_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        assert!(
            Registry::load(&work)
                .expect("nothing registered yet is not a failure")
                .targets()
                .is_empty()
        );
    }

    #[test]
    fn a_truncated_registry_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let path = Registry::path(&work);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            "count = 3\n\n[[targets]]\nkind = \"repo\"\nname_with_owner = \"acme/api\"\n",
        )
        .expect("write");

        let err = Registry::load(&work).expect_err("a partial write must not read as a small set");
        let AuditError::TruncatedRegistry {
            declared, found, ..
        } = &err
        else {
            panic!("expected TruncatedRegistry, got {err:?}");
        };
        assert_eq!((*declared, *found), (3, 1));
    }

    /// The additive property: a second `add` leaves the first alone.
    #[test]
    fn adding_a_target_leaves_the_others_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());

        let mut first = Registry::default();
        first.insert(repo("acme/api"));
        first.save(&work).expect("writes");

        let mut second = Registry::load(&work).expect("reads");
        second.insert(board(BoardProvider::Jira, "ACME"));
        second.save(&work).expect("writes");

        let ids: Vec<String> = Registry::load(&work)
            .expect("reads")
            .targets()
            .iter()
            .map(Target::id)
            .collect();
        assert_eq!(ids, vec!["acme/api", "jira:ACME"]);
    }

    #[test]
    fn re_adding_an_existing_target_is_idempotent() {
        let mut registry = Registry::default();
        assert!(registry.insert(repo("acme/api")));
        assert!(!registry.insert(repo("acme/api")));
        assert_eq!(registry.targets().len(), 1);
    }

    /// GitHub, JIRA and Linear are all case-insensitive on these identifiers,
    /// so a re-add that differs only in case must not clone the repository
    /// twice under two names.
    #[test]
    fn case_differences_do_not_create_a_second_entry() {
        let mut registry = Registry::default();
        assert!(registry.insert(repo("acme/api")));
        assert!(!registry.insert(repo("Acme/API")));
        assert!(registry.insert(board(BoardProvider::Jira, "ACME")));
        assert!(!registry.insert(board(BoardProvider::Jira, "acme")));
        assert_eq!(registry.targets().len(), 2);
        // The spelling the operator used is what is stored.
        assert_eq!(registry.targets()[0].id(), "acme/api");
    }

    /// A JIRA project and a Linear team may share a key; they are two targets.
    #[test]
    fn the_same_key_at_two_providers_is_two_targets() {
        let mut registry = Registry::default();
        assert!(registry.insert(board(BoardProvider::Jira, "ENG")));
        assert!(registry.insert(board(BoardProvider::Linear, "ENG")));
        assert_eq!(registry.targets().len(), 2);
    }

    #[test]
    fn removing_a_target_that_is_not_registered_changes_nothing() {
        let mut registry = Registry::default();
        registry.insert(repo("acme/api"));
        assert!(!registry.remove(&repo("acme/web")));
        assert!(registry.remove(&repo("ACME/API")));
        assert!(registry.targets().is_empty());
    }

    #[test]
    fn parsing_accepts_both_shapes_and_records_the_verb() {
        assert_eq!(
            parse(Some(TargetKind::Repo), " acme/api ").expect("parses"),
            repo("acme/api")
        );
        assert_eq!(
            parse(Some(TargetKind::Board), "JIRA:ACME").expect("parses"),
            board(BoardProvider::Jira, "ACME")
        );
        assert_eq!(
            parse(Some(TargetKind::Board), "linear:a1b2-c3").expect("parses"),
            board(BoardProvider::Linear, "a1b2-c3")
        );
        // `remove` passes None and takes either shape.
        assert_eq!(parse(None, "acme/api").expect("parses"), repo("acme/api"));
        assert_eq!(
            parse(None, "jira:ACME").expect("parses"),
            board(BoardProvider::Jira, "ACME")
        );
    }

    #[test]
    fn a_spec_that_is_neither_shape_is_refused() {
        for spec in ["", "acme", "github:acme/api", "jira:", "jira:AC ME", "::"] {
            let err = parse(None, spec).expect_err("{spec} must not register");
            assert!(
                matches!(
                    err,
                    AuditError::InvalidTarget { .. } | AuditError::InvalidRepoName { .. }
                ),
                "{spec}: {err:?}"
            );
        }
    }

    /// The verb is honoured: `add repo jira:ACME` is a mistake, not a board.
    #[test]
    fn the_verb_decides_the_shape_rather_than_the_spelling() {
        assert!(parse(Some(TargetKind::Repo), "jira:ACME").is_err());
        assert!(parse(Some(TargetKind::Board), "acme/api").is_err());
    }

    /// 🔴 #6001: an absolute path registers as a checkout on disk rather than
    /// being run through the `owner/name` charset, which refuses a leading `/`.
    ///
    /// Against `7eef4bb9b` every one of these is `InvalidRepoName`, which is the
    /// whole reason a 1.4 GB checkout with full history could not be audited.
    #[test]
    fn an_absolute_path_registers_as_a_local_checkout() {
        for spec in ["/srv/apex", " /srv/apex ", "/srv/apex/", "/srv/apex.git"] {
            let target = parse(Some(TargetKind::Repo), spec).expect("{spec} must register");
            let Target::LocalRepo { path } = &target else {
                panic!("{spec} registered as {target:?}, not a local checkout");
            };
            assert!(path.is_absolute(), "{spec}");
            // The kind is the verb, and there is one verb for both shapes — so
            // every count and filter that asks "is this a repository" still
            // says yes.
            assert_eq!(target.kind(), TargetKind::Repo);
            // The id round-trips: `taudit remove <the path you typed>` works.
            assert_eq!(parse(None, &target.id()).expect("round-trips"), target);
        }
    }

    /// The two shapes cannot overlap, so `add repo` needs no second verb: a
    /// relative `owner/repo` is never read as a path, and a path is never run
    /// through the GitHub charset.
    #[test]
    fn a_relative_spec_is_still_a_github_repository() {
        assert_eq!(
            parse(Some(TargetKind::Repo), "acme/api").expect("parses"),
            repo("acme/api")
        );
        // A path holding a colon is still a path, not a board with an unknown
        // provider — `parse(None, …)` is what `taudit remove` passes.
        let target = parse(None, "/srv/odd:name").expect("parses");
        assert_eq!(target.kind(), TargetKind::Repo);
    }

    /// A local checkout survives the config round trip, so an engagement that
    /// declares one reads it back as one.
    #[test]
    fn a_local_checkout_round_trips_through_the_engagement_config() {
        let path = PathBuf::from("engagement.toml");
        let local = parse(Some(TargetKind::Repo), "/srv/apex").expect("parses");
        let text = config::with_targets(ENGAGEMENT, std::slice::from_ref(&local), &path)
            .expect("declares");
        let declared = EngagementConfig::from_toml(&text, &path)
            .expect("loads")
            .declared_targets()
            .expect("declares a set")
            .to_vec();
        assert_eq!(declared, vec![local]);
        assert!(
            text.contains("local_repo"),
            "the kind tag must be stable: {text}"
        );
    }

    /// A registered name becomes a path under the working directory when it is
    /// cloned, so the containment argument in `clone::split_name` applies here.
    ///
    /// #6001 moved `/abs/path` out of this list — an absolute path is now a
    /// LOCAL checkout rather than a malformed `owner/repo`. Containment is not
    /// weakened by that, and the second half is where it is now proved: a local
    /// target's DESTINATION is `repos/local/<basename>`, built by
    /// `local_repo::derive_name`, which maps every character outside the
    /// checkout charset to `-`. The operator's path is read; it never becomes
    /// one this crate writes to.
    #[test]
    fn a_traversing_repo_name_is_never_registered() {
        for spec in ["../etc/passwd", "acme/../../etc", "acme/a/b"] {
            assert!(
                parse(Some(TargetKind::Repo), spec).is_err(),
                "{spec} was accepted"
            );
        }

        let work = WorkDir::new("/engagement/work");
        for spec in ["/abs/path", "/../../etc/passwd", "/srv/a/../../../etc"] {
            let target = parse(Some(TargetKind::Repo), spec).expect("a path registers");
            let name = local_repo::derive_name(&PathBuf::from(spec)).expect("a checkout name");
            let dest = clone::destination(&work, &name).expect("a destination");
            assert!(
                dest.starts_with(work.path(Area::Repos).join(local_repo::LOCAL_OWNER)),
                "{spec} escaped to {}",
                dest.display()
            );
            assert_eq!(target.kind(), TargetKind::Repo);
        }
    }

    #[test]
    fn the_listing_names_a_selection_file_it_did_not_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        assert!(legacy_selection(&work).expect("no selection yet").is_none());

        run::save_selection(
            &work,
            &[run::SelectedRepo {
                name: "acme/api".to_owned(),
                path: PathBuf::from("repos/acme/api"),
                github_slug: None,
                github_absent: None,
            }],
        )
        .expect("writes");

        let (path, count) = legacy_selection(&work)
            .expect("reads")
            .expect("a selection exists");
        assert_eq!(path, run::selection_path(&work));
        assert_eq!(count, 1);
        // Nothing was copied across: the registry is still empty.
        assert!(Registry::load(&work).expect("reads").targets().is_empty());
    }
}
