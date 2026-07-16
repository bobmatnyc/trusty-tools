//! Project binding — the ONE typed object `task.run` and `session.create`
//! converge on (spec DOC-39 §4.2/§5.5, ACs 2.1–2.4 and 16.1–16.3).
//!
//! Why: "project" was one concept split across two disagreeing API surfaces.
//! `task.run` took a REQUIRED `project: PathBuf` (`task/protocol.rs:48`), which
//! made a projectless workstream — Bob-mandated, and the entry state screen 7a
//! renders — literally impossible to express. `session.create` took an untyped,
//! free-form `project: Option<String>` label (`session/protocol.rs:157`) that was
//! never validated, never bound, and disconnected from the path `task.run`
//! demanded. A label that cannot be indexed and a path that cannot be omitted are
//! two halves of one missing object. [`ProjectBinding`] is that object.
//!
//! What: a three-state enum, matching the spec's §4.2 table exactly:
//!
//! | State | Indexing | Git affordances |
//! |---|---|---|
//! | [`ProjectBinding::None`] — projectless | none | none |
//! | [`ProjectBinding::Directory`] — bound, non-git | **yes** | none |
//! | [`ProjectBinding::GitRepo`] — bound, git worktree | yes | full |
//!
//! **Binding is NOT gated on `.git`.** The design proposal this spec corrects
//! claimed work binds "the moment work touches files in a git repo"; that rule
//! would exclude the non-git working dirs (#2728) and OS temp dirs (#2747) we
//! deliberately support and have already shipped. `Directory` is a first-class
//! BOUND state that indexes — it is emphatically not a synonym for projectless.
//! The git/non-git split exists only to decide which *git affordances* are
//! offered, never whether the project binds or indexes.
//!
//! Test: `binding::tests::*`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

/// A workstream's project binding — zero or one project, in one of three states.
///
/// Why: see the module docs — this is the single typed object that replaces
/// `task.run`'s un-omittable path and `session.create`'s un-indexable label.
/// What: [`None`](ProjectBinding::None) is projectless (chat/planning; no index,
/// no diff target, no project-scoped memory palace). The two bound variants both
/// carry a canonical root and both index; they differ ONLY in whether git
/// affordances are available.
/// Test: `binding::tests::*`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProjectBinding {
    /// Projectless — no directory bound. Chat/planning only.
    #[default]
    None,
    /// Bound to a plain directory that is NOT a git worktree (#2728/#2747).
    /// Indexes; git affordances are hidden, never faked.
    Directory(PathBuf),
    /// Bound to a git worktree. Indexes; full git affordances.
    GitRepo(PathBuf),
}

/// The wire `state` discriminant for [`ProjectBinding::None`].
pub const STATE_PROJECTLESS: &str = "projectless";
/// The wire `state` discriminant for [`ProjectBinding::Directory`].
pub const STATE_DIRECTORY: &str = "directory";
/// The wire `state` discriminant for [`ProjectBinding::GitRepo`].
pub const STATE_GIT_REPO: &str = "git_repo";

/// Why a path could not be resolved into a bound [`ProjectBinding`].
///
/// Why: `session.create`'s old free-form label accepted anything — `"my-app"`
/// bound nothing and indexed nothing, and the caller was never told. Now that the
/// label IS the binding, a path that cannot bind must say so loudly rather than
/// degrade into a lie. Callers map this onto `-32003 invalid_argument`.
/// What: the two ways a supplied path fails to name a bindable project root.
/// Test: `binding::tests::resolve_rejects_missing_path`,
/// `binding::tests::resolve_rejects_file_path`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindingError {
    /// The path does not exist on disk.
    #[error("project path does not exist: {0}")]
    NotFound(PathBuf),
    /// The path exists but is not a directory.
    #[error("project path is not a directory: {0}")]
    NotADirectory(PathBuf),
}

impl ProjectBinding {
    /// Classify an optional project path into one of the three binding states.
    ///
    /// Why: the single entry point every API surface funnels through, so
    /// `task.run` and `session.create` can never again disagree about what a
    /// project is or how one is recognised.
    /// What: `None` ⇒ [`ProjectBinding::None`] (projectless — the supported,
    /// non-error case). `Some(p)` ⇒ an existing directory classified as
    /// [`GitRepo`](ProjectBinding::GitRepo) when [`is_git_worktree`] says so,
    /// else [`Directory`](ProjectBinding::Directory) — a BOUND, indexing state,
    /// not a fallback to projectless. A `Some(p)` naming a nonexistent path or a
    /// non-directory is a [`BindingError`], never a silent downgrade to
    /// projectless: omitting a project is a deliberate choice, whereas naming one
    /// that cannot bind is a caller mistake, and collapsing the two would hide it.
    /// The root is canonicalised when the filesystem allows, so two spellings of
    /// one project (`.`, `/abs/path`, a symlink) yield one binding.
    /// Test: `binding::tests::resolve_none_is_projectless`,
    /// `binding::tests::resolve_plain_dir_binds_as_directory`,
    /// `binding::tests::resolve_git_repo_binds_as_git_repo`,
    /// `binding::tests::resolve_rejects_missing_path`,
    /// `binding::tests::resolve_rejects_file_path`.
    pub fn resolve(path: Option<PathBuf>) -> Result<Self, BindingError> {
        let Some(path) = path else {
            return Ok(Self::None);
        };
        if !path.exists() {
            return Err(BindingError::NotFound(path));
        }
        if !path.is_dir() {
            return Err(BindingError::NotADirectory(path));
        }
        // Canonicalise best-effort: a failure here (permissions, a race) is not
        // a reason to reject a directory we already confirmed exists.
        let root = path.canonicalize().unwrap_or(path);
        if is_git_worktree(&root) {
            Ok(Self::GitRepo(root))
        } else {
            Ok(Self::Directory(root))
        }
    }

    /// The bound project root, or `None` when projectless.
    ///
    /// Why: every consumer that needs a real directory (fs tools, `CLAUDE.md`
    /// loading, indexing) asks here, and the `Option` makes "there may be no
    /// project" impossible to forget at the type level — which is exactly the
    /// bug the old required `PathBuf` encoded.
    /// What: `Some(&Path)` for both bound variants; `None` for projectless.
    /// Test: `binding::tests::root_is_none_only_when_projectless`.
    pub fn root(&self) -> Option<&Path> {
        match self {
            Self::None => Option::None,
            Self::Directory(p) | Self::GitRepo(p) => Some(p),
        }
    }

    /// Whether a project is bound at all (either bound state).
    ///
    /// Why: the index/diff/palace decisions key off "is there a project", NOT
    /// off "is it git" — conflating those two is precisely the proposal's error.
    /// What: `false` only for [`ProjectBinding::None`].
    /// Test: `binding::tests::non_git_dir_is_bound_and_indexes`.
    pub fn is_bound(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether git affordances (branch, PR, dirty-tree) are available.
    ///
    /// Why: the ONLY question the git/non-git split is allowed to answer. It
    /// must never be used to decide whether to bind or index (AC-2.2/AC-2.3).
    /// What: `true` only for [`ProjectBinding::GitRepo`].
    /// Test: `binding::tests::non_git_dir_is_bound_and_indexes`,
    /// `binding::tests::resolve_git_repo_binds_as_git_repo`.
    pub fn is_git(&self) -> bool {
        matches!(self, Self::GitRepo(_))
    }

    /// Whether this binding's project should be indexed (AC-2.3).
    ///
    /// Why: names the rule in one place so no call site re-derives it and
    /// accidentally reintroduces the git-only gate.
    /// What: identical to [`is_bound`](Self::is_bound) — **both** bound states
    /// index, git or not. Projectless does not.
    /// Test: `binding::tests::non_git_dir_is_bound_and_indexes`,
    /// `binding::tests::projectless_does_not_index`.
    pub fn should_index(&self) -> bool {
        self.is_bound()
    }

    /// The wire `state` discriminant for this binding.
    ///
    /// Why: the SPA renders three distinct states (§4.2) and needs to tell them
    /// apart without inferring from the presence of a path.
    /// What: one of [`STATE_PROJECTLESS`], [`STATE_DIRECTORY`], [`STATE_GIT_REPO`].
    /// Test: `binding::tests::wire_shape_round_trips_each_state`.
    pub fn state(&self) -> &'static str {
        match self {
            Self::None => STATE_PROJECTLESS,
            Self::Directory(_) => STATE_DIRECTORY,
            Self::GitRepo(_) => STATE_GIT_REPO,
        }
    }

    /// A human display label for the bound project, or `None` when projectless.
    ///
    /// Why: preserves `Session.project`'s existing `Option<String>` wire field —
    /// which is now DERIVED from the binding rather than being an independent,
    /// unvalidated source of truth. That derivation is the whole reconciliation:
    /// the label still exists for humans, but it can no longer disagree with the
    /// path, because there is only one path and the label is computed from it.
    /// What: the root's display string; `None` for projectless.
    /// Test: `binding::tests::label_is_derived_from_root`.
    pub fn label(&self) -> Option<String> {
        self.root().map(|p| p.display().to_string())
    }

    /// The agents directory this binding resolves `<agent>.toml` from.
    ///
    /// Why: a projectless daemon still runs a PM and still needs agent configs;
    /// with no project root there is no project-scoped `.claude/agents` to read,
    /// so it must fall back rather than fail. Falling back to the process CWD
    /// would silently bind to a directory the operator never chose — the exact
    /// implicit-binding bug this type exists to prevent — so it falls back to the
    /// USER-level location instead.
    /// What: bound ⇒ [`crate::agents::locate_agents_dir`] on the project root
    /// (the `.claude/agents` → `.open-mpm/agents` convention). Projectless ⇒ the
    /// same convention rooted at `$HOME`, i.e. `~/.claude/agents`, matching
    /// Claude Code's user-level agents location. With no resolvable home
    /// directory, degrades to a bare relative `.claude/agents` — which simply
    /// will not exist, surfacing as a normal "PM config error" rather than a panic.
    /// Test: `binding::tests::agents_dir_uses_project_root_when_bound`,
    /// `binding::tests::projectless_agents_dir_is_user_level`.
    pub fn agents_dir(&self) -> PathBuf {
        match self.root() {
            Some(root) => crate::agents::locate_agents_dir(root),
            Option::None => match dirs::home_dir() {
                Some(home) => crate::agents::locate_agents_dir(&home),
                Option::None => PathBuf::from(".claude").join("agents"),
            },
        }
    }

    /// The JSON wire representation: `{ "state": …, "root": … }`.
    ///
    /// Why: the SPA needs the state discriminant explicitly rather than
    /// inferring projectless from a null path (AC-2.1) — an absent `root` and a
    /// `projectless` state are the same fact today, but only the discriminant
    /// stays honest if a future state carries no path for a different reason.
    /// What: `root` is `null` for projectless, the display path otherwise.
    /// Test: `binding::tests::wire_shape_round_trips_each_state`.
    pub fn to_json(&self) -> Value {
        json!({ "state": self.state(), "root": self.label() })
    }
}

impl Serialize for ProjectBinding {
    /// Serialise via [`ProjectBinding::to_json`] so the wire shape is defined
    /// in exactly one place.
    ///
    /// Why: `Session` derives `Serialize`; without this the enum would emit
    /// serde's default externally-tagged shape and drift from `to_json`.
    /// What: delegates to [`ProjectBinding::to_json`].
    /// Test: `binding::tests::serialize_matches_to_json`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

/// The `{ state, root }` wire shape, used only as [`ProjectBinding`]'s
/// deserialization intermediate.
#[derive(Deserialize)]
struct BindingWire {
    state: String,
    #[serde(default)]
    root: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for ProjectBinding {
    /// Reconstruct a binding from its wire shape.
    ///
    /// Why: `Session` derives `Deserialize`, so its `binding` field needs one.
    /// What: reconstructs from the EXPLICIT `state` discriminant and does NOT
    /// re-probe the filesystem — deserializing a value must not depend on
    /// whether a path still exists or is still a git repo on THIS machine, which
    /// would make a round-trip lossy (a `GitRepo` read back on a host without
    /// the repo would silently become a `Directory`). A bound state missing its
    /// `root`, or an unknown discriminant, is a hard error rather than a silent
    /// downgrade to projectless.
    /// Test: `binding::tests::deserialize_round_trips_without_touching_disk`,
    /// `binding::tests::deserialize_rejects_bound_state_without_root`.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = BindingWire::deserialize(deserializer)?;
        let root = |w: Option<PathBuf>| {
            w.ok_or_else(|| {
                D::Error::custom(format!("binding state `{}` requires a root", wire.state))
            })
        };
        match wire.state.as_str() {
            STATE_PROJECTLESS => Ok(Self::None),
            STATE_DIRECTORY => Ok(Self::Directory(root(wire.root.clone())?)),
            STATE_GIT_REPO => Ok(Self::GitRepo(root(wire.root.clone())?)),
            other => Err(D::Error::custom(format!(
                "unknown project binding state `{other}` (expected one of \
                 `{STATE_PROJECTLESS}`, `{STATE_DIRECTORY}`, `{STATE_GIT_REPO}`)"
            ))),
        }
    }
}

/// Whether `root` is inside a git worktree.
///
/// Why: the ONE git-detection implementation in the crate. `run_task::diff`'s
/// snapshot logic needs the identical answer this binding classifies on; two
/// independent probes could disagree and produce a `GitRepo` binding whose diff
/// silently took the non-git file-map path.
/// What: shells out to `git -C <root> rev-parse --is-inside-work-tree`. Fail-open
/// to `false` — no git binary, a non-repo, or any error means "no git
/// affordances", which is a fully supported bound state (§4.2), never an error.
/// Test: `binding::tests::resolve_git_repo_binds_as_git_repo`,
/// `binding::tests::resolve_plain_dir_binds_as_directory`.
pub fn is_git_worktree(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
