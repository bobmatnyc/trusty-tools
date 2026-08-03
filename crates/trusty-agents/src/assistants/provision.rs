//! Startup provisioning of assistant home directories (#4325).
//!
//! Why: the home is only useful if it EXISTS before anything reaches for it, and
//! the owner's answer (2026-08-01) is to create it at app startup rather than on
//! first use. That makes provisioning a boot-path concern, which changes its
//! risk profile completely: a boot-path filesystem call that can fail is a boot
//! path that can fail. It must not be. A read-only filesystem, a denied
//! permission, a full disk, or a user who left a FILE where `okg/` belongs are
//! all states the app has to start in — degraded and able to explain itself, but
//! started. So nothing in this module returns `Result`: every failure becomes a
//! [`HomeIssue`] on the report, which is what the [`super::health`] narration
//! seam was built for.
//!
//! What: [`provision`] provisions ONE instance — `ensure()` then `inspect()`,
//! never failing. [`provision_all`] runs it across a roster and returns a
//! [`StartupProvisioning`] the caller logs. Running `inspect` even on the
//! success path is deliberate: `ensure` reports what it CREATED, not whether the
//! result is sound, and a home containing a user's malformed `config.toml` is
//! healthy from `ensure`'s point of view and broken from the user's.
//!
//! Idempotence is load-bearing here in a way it was not before. This runs on
//! EVERY launch, so `ensure`'s never-overwrite semantics are now what stands
//! between a user's edited `instructions.md` and losing it on next boot — see
//! `super::tests::provision_tests::repeated_startup_never_disturbs_user_edits`.
//!
//! Scope: creation only. No migration, no relocation, no cleanup of anything
//! already on disk — relocating an existing home needs a migration process the
//! owner has explicitly deferred.
//!
//! Test: `super::tests::provision_tests` — the whole module.

use std::path::{Path, PathBuf};

use super::error::AssistantError;
use super::health::{HomeHealth, HomeIssue, HomeIssueKind, inspect};
use super::home::AssistantHome;
use super::instance::AssistantInstanceId;
use super::roster::discover_instances;

/// The outcome of provisioning ONE assistant instance's home at startup.
///
/// Why/What/Test: see this module's doc comment. There is no error variant —
/// a failed provision is a report with issues, not an absent report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedHome {
    /// The instance this home belongs to.
    pub id: AssistantInstanceId,
    /// The home directory.
    pub home: PathBuf,
    /// Paths this launch actually created; empty on every launch after the
    /// first. Also empty when creation FAILED, even if some entries were made
    /// before the failure — `ensure` discards its progress record on error, so
    /// `health` (not this field) is the authority on the resulting state.
    pub created: Vec<PathBuf>,
    /// The state of the home AFTER provisioning. Healthy on the happy path.
    pub health: HomeHealth,
}

impl ProvisionedHome {
    /// Whether this launch generated anything.
    ///
    /// Why: the startup log should say something the first time and nothing on
    /// every launch after — a line per boot that never changes is noise.
    /// Test: `super::tests::provision_tests::second_startup_creates_nothing`.
    pub fn created_anything(&self) -> bool {
        !self.created.is_empty()
    }

    /// Whether the home is usable after provisioning.
    ///
    /// Test: `super::tests::provision_tests::startup_creates_a_missing_home`.
    pub fn is_healthy(&self) -> bool {
        self.health.is_healthy()
    }
}

/// The outcome of startup provisioning across every known instance.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupProvisioning {
    pub homes: Vec<ProvisionedHome>,
}

impl StartupProvisioning {
    /// Every home that came out of provisioning with a problem.
    ///
    /// Why: the caller logs these at warn level and the concierge narrates them
    /// on demand; the healthy ones need no attention.
    /// Test: `super::tests::provision_tests::startup_survives_a_home_it_cannot_create`.
    pub fn degraded(&self) -> impl Iterator<Item = &ProvisionedHome> {
        self.homes.iter().filter(|h| !h.is_healthy())
    }

    /// Every home this launch actually created something for.
    ///
    /// Test: `super::tests::provision_tests::second_startup_creates_nothing`.
    pub fn newly_created(&self) -> impl Iterator<Item = &ProvisionedHome> {
        self.homes.iter().filter(|h| h.created_anything())
    }
}

/// Provision one instance's home. Never fails.
///
/// Why: see this module's doc comment — this is the call that must not be able
/// to take startup down.
/// What: `ensure()` then `inspect()`. On an `ensure` failure the reason is
/// preserved as a leading [`HomeIssueKind::NotCreatable`] issue and inspection
/// still runs, so the report says both WHY creation failed and WHAT state the
/// home was left in. `AssistantError`'s message is used verbatim — it is
/// already written for a person.
/// Test: `super::tests::provision_tests::startup_creates_a_missing_home`,
/// `super::tests::provision_tests::startup_survives_a_home_it_cannot_create`,
/// `super::tests::provision_tests::startup_fills_only_the_gaps`.
pub fn provision(home: &AssistantHome) -> ProvisionedHome {
    let (created, failure) = match home.ensure() {
        Ok(created) => (created.paths, None),
        Err(err) => (Vec::new(), Some(err)),
    };
    let mut health = inspect(home);
    if let Some(err) = failure {
        health.issues.insert(0, creation_failure(home, &err));
    }
    ProvisionedHome {
        id: home.id().clone(),
        home: home.path().to_path_buf(),
        created,
        health,
    }
}

/// Provision every instance in `roster`, in order. Never fails.
///
/// Why: startup knows a set of assistant instances, not one — and a single
/// unwritable home must not stop the others from being provisioned. Each is
/// independent.
/// What: [`provision`] per instance, collected. An empty roster yields an empty
/// report, which is a normal state (no assistants configured yet).
/// Test: `super::tests::provision_tests::provisions_every_instance_independently`,
/// `super::tests::provision_tests::an_empty_roster_is_not_an_error`.
pub fn provision_all(roster: impl IntoIterator<Item = AssistantHome>) -> StartupProvisioning {
    StartupProvisioning {
        homes: roster.into_iter().map(|home| provision(&home)).collect(),
    }
}

/// Provision every Assistant instance found in `agent_dirs`, under `root`.
///
/// Why: the hermetic core, taking both roots explicitly so tests point at a
/// tempdir instead of a developer's real `$HOME` — the same
/// `ensure_bundled_agents_deployed_in` shape this crate already uses for
/// boot-time provisioning.
/// What: discovers instances ([`super::roster::discover_instances`]) and
/// provisions each. Never fails; never logs (the caller decides).
/// Test: `super::tests::provision_tests::startup_provisions_the_discovered_roster`.
pub fn provision_startup_homes_in(root: &Path, agent_dirs: &[PathBuf]) -> StartupProvisioning {
    let roster = discover_instances(agent_dirs)
        .into_iter()
        .map(|id| AssistantHome::under(root, id));
    provision_all(roster)
}

/// The startup call: provision every Assistant instance's home (#4325).
///
/// Why: the owner's answer (2026-08-01) — the home layout exists before
/// anything needs it, created at app startup. This is the ONE function the boot
/// path calls, and it is infallible by construction so the boot path cannot
/// acquire a new way to die. A `$HOME` that cannot be resolved is the one case
/// with no home to even report against, so it degrades to an empty report plus
/// a warning.
/// What: resolves [`super::home::assistants_root`] and
/// `crate::agents::agents_dir_candidates()`, provisions each discovered
/// instance, and LOGS — `info` naming what this launch created (first launch
/// only; silent on every launch after), `warn` per degraded home carrying the
/// reason and the remedy. Never panics, never returns `Err`.
/// Test: `super::tests::provision_tests::startup_provisions_the_discovered_roster`
/// (the hermetic core; this wrapper only adds `$HOME` resolution and logging).
pub fn provision_startup_homes() -> StartupProvisioning {
    let root = match super::home::assistants_root() {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(error = %err, "skipped assistant home provisioning");
            return StartupProvisioning::default();
        }
    };
    let report = provision_startup_homes_in(&root, &crate::agents::agents_dir_candidates());

    for home in report.newly_created() {
        tracing::info!(
            assistant = home.id.as_str(),
            home = %home.home.display(),
            created = home.created.len(),
            "created assistant home directory"
        );
    }
    for home in report.degraded() {
        for issue in &home.health.issues {
            tracing::warn!(
                assistant = home.id.as_str(),
                path = %issue.path.display(),
                detail = %issue.detail,
                remedy = %issue.remedy,
                "assistant home needs attention"
            );
        }
    }
    report
}

/// Turn a failed `ensure()` into a reportable finding.
///
/// Why: the error carries the only thing the user needs and `inspect` cannot
/// recover — WHY the write failed. "Your home directory is missing" is much less
/// useful than "…because the filesystem is read-only".
fn creation_failure(home: &AssistantHome, err: &AssistantError) -> HomeIssue {
    HomeIssue {
        kind: HomeIssueKind::NotCreatable,
        entry: "",
        path: home.path().to_path_buf(),
        detail: err.to_string(),
        remedy: "the app started without it, so this assistant's own files are unavailable \
                 until it is fixed — check the permissions and free space on that path, or \
                 move aside anything sitting where a directory belongs"
            .to_string(),
    }
}
