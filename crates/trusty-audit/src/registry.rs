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
//! What: [`Target`], the two things that can be registered; [`Registry`], the
//! set and its `state/`[`REGISTRY_FILE`] persistence; and [`parse`], the one
//! place a command-line spec becomes a target.
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
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use trusty_common::file_lock::with_exclusive_lock;

use crate::clone;
use crate::error::AuditError;
use crate::run;
use crate::workdir::{self, Area, WorkDir};

/// File under `state/` holding the registered audit targets.
///
/// The same two obligations as [`crate::run::SELECTION_FILE`]: `count` is
/// declared ahead of the entries, and the file is written by rename
/// ([`workdir::write_atomically`]). A `count` that disagrees with the entries is
/// [`AuditError::TruncatedRegistry`], never a smaller set.
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
    /// A JIRA project or Linear team, validated with the configured credential.
    Board {
        /// Which provider.
        provider: BoardProvider,
        /// Project key, team key, or team id.
        key: String,
    },
}

impl Target {
    /// The canonical spelling: `owner/name`, or `provider:key`.
    ///
    /// This is what an operator types into `taudit remove`, and what the CLI
    /// prints, so the two are the same string by construction.
    pub fn id(&self) -> String {
        match self {
            Target::Repo { name_with_owner } => name_with_owner.clone(),
            Target::Board { provider, key } => format!("{}:{key}", provider.as_str()),
        }
    }

    /// Which kind this is.
    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Repo { .. } => TargetKind::Repo,
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
/// Test: `super::registry_tests::a_spec_that_is_neither_shape_is_refused`,
/// `super::registry_tests::a_traversing_repo_name_is_never_registered`.
///
/// # Errors
///
/// [`AuditError::InvalidRepoName`] for a repository spec that is not a plain
/// `owner/name`, and [`AuditError::InvalidTarget`] for a board spec that names
/// no known provider or carries an unusable key.
pub fn parse(kind: Option<TargetKind>, spec: &str) -> Result<Target, AuditError> {
    let spec = spec.trim();
    let wanted = kind.unwrap_or(if spec.contains(':') {
        TargetKind::Board
    } else {
        TargetKind::Repo
    });
    match wanted {
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

/// Append `target` to the registry, serialised against every other writer.
///
/// Why: #5822. [`Registry::load`] → [`Registry::insert`] → [`Registry::save`] is
/// a read-modify-write, and nothing made it indivisible. Two `taudit add` runs
/// against one working directory each load the same snapshot, each append their
/// own target, and the later save discards the earlier one's — with both
/// reporting success. [`workdir::write_atomically`] does not close that: it
/// makes one write untearable, not a load-mutate-save atomic.
/// What: runs the whole critical section under
/// [`trusty_common::file_lock::with_exclusive_lock`], the workspace's one
/// implementation of it — extracted for this exact failure after
/// `trusty-search`'s `indexes.toml` lost updates the same way (#5344). Returns
/// whether this call is the one that appended; `false` means another writer had
/// registered the same target first, which is the ordinary idempotent no-op.
/// Test: `super::registry_tests::concurrent_registrations_keep_every_target`.
///
/// Callers validate BEFORE calling this. Validation reaches the network under a
/// 30-second ceiling, and the lock is not reentrant — holding it across a
/// request would stall every other `add` in the working directory behind one
/// unreachable site. Re-reading the file here is what keeps that safe: the
/// append is decided against the snapshot that is current at write time, not
/// the one the caller validated against.
///
/// # Errors
///
/// [`AuditError::RegistryLock`] when the lock cannot be taken — never a
/// bypass — plus whatever [`Registry::load`] and [`Registry::save`] fail with.
pub fn register(work: &WorkDir, target: &Target) -> Result<bool, AuditError> {
    locked(work, || {
        let mut registry = Registry::load(work)?;
        if !registry.insert(target.clone()) {
            return Ok(false);
        }
        registry.save(work)?;
        Ok(true)
    })
}

/// Drop `target` from the registry, under the same lock [`register`] takes.
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
pub fn deregister(work: &WorkDir, target: &Target) -> Result<bool, AuditError> {
    locked(work, || {
        let mut registry = Registry::load(work)?;
        if !registry.remove(target) {
            return Ok(false);
        }
        registry.save(work)?;
        Ok(true)
    })
}

/// Run `f` holding the registry's exclusive lock.
///
/// The lock guards [`Registry::path`] through its `.lock` sidecar, so it
/// survives the rename [`Registry::save`] publishes with.
fn locked<T>(work: &WorkDir, f: impl FnOnce() -> Result<T, AuditError>) -> Result<T, AuditError> {
    let path = Registry::path(work);
    with_exclusive_lock(&path, f).map_err(|source| AuditError::RegistryLock { path, source })?
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

        race(&work, |work, n| {
            assert!(
                register(work, &repo(&format!("acme/repo-{n:02}"))).expect("a racing add succeeds"),
                "acme/repo-{n:02} was reported as already registered"
            );
        });

        let mut expected: Vec<String> = (0..WRITERS).map(|n| format!("acme/repo-{n:02}")).collect();
        expected.sort();
        assert_eq!(
            registered_ids(&work),
            expected,
            "a concurrently-registered target was discarded"
        );
    }

    /// The same read-modify-write, so the same lock: an unserialised removal
    /// writes back a snapshot still holding the targets other writers dropped.
    #[test]
    fn a_removal_holds_the_same_lock_an_add_does() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let mut seeded = Registry::default();
        for n in 0..WRITERS {
            seeded.insert(repo(&format!("acme/repo-{n:02}")));
        }
        seeded.save(&work).expect("writes");

        race(&work, |work, n| {
            assert!(
                deregister(work, &repo(&format!("acme/repo-{n:02}"))).expect("a racing remove"),
                "acme/repo-{n:02} was reported as not registered"
            );
        });

        assert!(
            registered_ids(&work).is_empty(),
            "a concurrent removal was undone: {:?}",
            registered_ids(&work)
        );
    }

    /// Registering the same target from every writer at once: exactly one call
    /// reports the append, and the file carries one entry.
    #[test]
    fn only_one_racing_writer_claims_a_duplicate_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let appended = std::sync::atomic::AtomicUsize::new(0);

        race(&work, |work, _| {
            if register(work, &repo("acme/api")).expect("a racing add succeeds") {
                appended.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });

        assert_eq!(appended.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(registered_ids(&work), vec!["acme/api"]);
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

    /// A registered name becomes a path under the working directory when it is
    /// cloned, so the containment argument in `clone::split_name` applies here.
    #[test]
    fn a_traversing_repo_name_is_never_registered() {
        for spec in ["../etc/passwd", "acme/../../etc", "/abs/path", "acme/a/b"] {
            assert!(
                parse(Some(TargetKind::Repo), spec).is_err(),
                "{spec} was accepted"
            );
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
