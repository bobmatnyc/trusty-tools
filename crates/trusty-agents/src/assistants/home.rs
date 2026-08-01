//! The app-generated per-assistant home directory (#4325).
//!
//! Why: An Assistant INSTANCE owns three things the owner named on 2026-07-30 —
//! a home directory, an OKG store and a persona — and before this module none
//! of them were per-instance. A persona sat in the shared agents directory, the
//! OKG tree sat in a flat `<knowledge_dir>/<slug(agent)>` pool addressed by
//! naming convention, and `[[stores]]` had no field naming a per-assistant root
//! at all (#3890's data-model gap, restated as requirement 1 of #4325). Adding
//! an instance therefore meant adding entries to three shared pools, which is
//! why the current model reads as "more agent types" rather than "more
//! instances of one type".
//!
//! The home is DOTLESS and human-browsable by deliberate decision (#4325's
//! "Open Questions Resolved" — confirmed, not open). It is the user's, not the
//! app's private state: users will edit, rename and delete inside it, and the
//! system's obligation is to DETECT that and help ([`super::health`]), never to
//! defend against it or silently rewrite it.
//!
//! What: [`assistants_root`] resolves the dotless root holding one home per
//! instance. [`AssistantHome`] is one instance's home — the five entries
//! #4325 specifies ([`INSTRUCTIONS_FILE`], [`CONFIG_FILE`], [`AGENTS_DIR`],
//! [`OKG_DIR`], [`ATTACHMENTS_DIR`]) as typed accessors, plus
//! [`AssistantHome::ensure`], the app-generated creation the ticket's
//! "AUTO-CREATED, APP-GENERATED" wording names. `ensure` is additive and
//! idempotent: it creates what is missing and never overwrites what is there,
//! because overwriting is precisely the external-change intolerance #4325
//! rules out. [`AssistantHome::store_root`] resolves a `[[stores]]` binding's
//! new `root` field against the home, confined inside it.
//!
//! Scope: this module OWNS the layout and its creation. It deliberately does
//! NOT migrate anything out of the existing dotted `~/.trusty-agents` tree
//! (#4325: "The system is NOT required to auto-migrate or auto-relocate"), and
//! nothing on the boot path calls [`AssistantHome::ensure`] yet — provisioning
//! is the caller's deliberate act, exactly like OKG extraction (#4283).
//!
//! Test: `super::tests::home_tests` — the whole module.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::AssistantError;
use super::instance::AssistantInstanceId;
use crate::stores::AgentStoreBinding;

/// Environment override for the assistants root.
///
/// Why: tests, and any operator who keeps the tree somewhere other than their
/// home directory. Named in the `NoUserHome` error so a user with no `$HOME`
/// is told the way out rather than just the problem.
/// Test: `super::tests::home_tests::env_override_wins_over_the_user_home`.
pub const ASSISTANTS_DIR_ENV: &str = "TAGENT_ASSISTANTS_DIR";

/// The DOTLESS product directory under the user's home (#4325).
///
/// Why: the home is user-facing and browsable "just like trusty-mpm-projects"
/// — a leading dot would hide it from Finder and from a plain `ls`, which is
/// the opposite of the decision. The existing dotted `~/.trusty-agents` tree is
/// app-private config and is untouched by this module.
/// Test: `super::tests::home_tests::default_root_is_dotless_under_the_user_home`.
pub const ASSISTANTS_DIR_NAME: &str = "trusty-agents";

/// The subdirectory of [`ASSISTANTS_DIR_NAME`] holding one home per instance.
///
/// Test: `super::tests::home_tests::default_root_is_dotless_under_the_user_home`.
pub const ASSISTANTS_SUBDIR: &str = "assistants";

/// The instance's own instructions (#4325). Test: `super::tests::home_tests::layout_matches_the_ticket`.
pub const INSTRUCTIONS_FILE: &str = "instructions.md";

/// The instance's own configuration, TOML (#4325 owner clarification
/// 2026-07-29: TOML throughout, no format migration).
/// Test: `super::tests::home_tests::layout_matches_the_ticket`.
pub const CONFIG_FILE: &str = "config.toml";

/// Custom agents scoped to this instance (#4325). Test: `super::tests::home_tests::layout_matches_the_ticket`.
pub const AGENTS_DIR: &str = "agents";

/// This instance's knowledge graph (#4325). Test: `super::tests::home_tests::layout_matches_the_ticket`.
pub const OKG_DIR: &str = "okg";

/// Binary files attached to this instance's chat (#4325). Test: `super::tests::home_tests::layout_matches_the_ticket`.
pub const ATTACHMENTS_DIR: &str = "attachments";

/// The dotless root holding one home directory per assistant instance.
///
/// Why: one resolver, so a home created by provisioning and a home inspected by
/// the concierge are the same directory. See [`ASSISTANTS_DIR_NAME`] for why it
/// is dotless.
/// What: `$TAGENT_ASSISTANTS_DIR` verbatim when set and non-blank, else
/// `<user home>/trusty-agents/assistants`. `Err(NoUserHome)` when neither
/// `$HOME` nor `$USERPROFILE` is set — resolving to the process CWD instead
/// would scatter user-owned homes wherever the binary happened to start.
/// Test: `super::tests::home_tests::default_root_is_dotless_under_the_user_home`,
/// `super::tests::home_tests::env_override_wins_over_the_user_home`.
pub fn assistants_root() -> Result<PathBuf, AssistantError> {
    if let Some(dir) = std::env::var_os(ASSISTANTS_DIR_ENV)
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .ok_or(AssistantError::NoUserHome {
            env: ASSISTANTS_DIR_ENV,
        })?;
    Ok(PathBuf::from(home)
        .join(ASSISTANTS_DIR_NAME)
        .join(ASSISTANTS_SUBDIR))
}

/// One Assistant-type INSTANCE's home directory.
///
/// Why/What/Test: see this module's doc comment. The value is pure path
/// arithmetic — constructing it touches no filesystem, so a caller can describe
/// a home that does not exist yet (which is the normal state before
/// [`Self::ensure`], and a reportable state after a user deletes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantHome {
    id: AssistantInstanceId,
    path: PathBuf,
}

impl AssistantHome {
    /// The home for `id` under an explicit assistants root.
    ///
    /// Why: the injectable form — tests and any caller that already resolved a
    /// root use this, so [`assistants_root`]'s environment read happens exactly
    /// once per call chain instead of once per accessor.
    /// What: `<root>/<id>`. Never touches the filesystem.
    /// Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn under(root: impl Into<PathBuf>, id: AssistantInstanceId) -> Self {
        let path = root.into().join(id.as_str());
        Self { id, path }
    }

    /// The home for `raw_id` under the resolved [`assistants_root`].
    ///
    /// Why: the one-call form for callers that have only a name.
    /// What: validates `raw_id` ([`AssistantInstanceId::new`]), resolves the
    /// root, and joins. Never touches the filesystem.
    /// Test: `super::tests::home_tests::for_instance_validates_the_id`.
    pub fn for_instance(raw_id: &str) -> Result<Self, AssistantError> {
        let id = AssistantInstanceId::new(raw_id)?;
        Ok(Self::under(assistants_root()?, id))
    }

    /// The instance this home belongs to. Test: `super::tests::home_tests::for_instance_validates_the_id`.
    pub fn id(&self) -> &AssistantInstanceId {
        &self.id
    }

    /// The home directory itself. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This instance's instructions file. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn instructions_path(&self) -> PathBuf {
        self.path.join(INSTRUCTIONS_FILE)
    }

    /// This instance's configuration file. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn config_path(&self) -> PathBuf {
        self.path.join(CONFIG_FILE)
    }

    /// Custom agents scoped to this instance. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn agents_dir(&self) -> PathBuf {
        self.path.join(AGENTS_DIR)
    }

    /// This instance's OKG store directory. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn okg_dir(&self) -> PathBuf {
        self.path.join(OKG_DIR)
    }

    /// This instance's chat attachments. Test: `super::tests::home_tests::layout_matches_the_ticket`.
    pub fn attachments_dir(&self) -> PathBuf {
        self.path.join(ATTACHMENTS_DIR)
    }

    /// Whether the home directory exists on disk.
    ///
    /// Test: `super::tests::home_tests::ensure_creates_the_whole_layout`.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// Resolve a store binding's OKG tree root against this home.
    ///
    /// Why: this is #4325 requirement 1 — the per-assistant root the binding
    /// never had. Without it, `okg://<agent>` resolved only into the SHARED
    /// `<knowledge_dir>/<slug>` pool, so "each instance carries its own OKG
    /// store" was a naming convention rather than a path. The result is
    /// CONFINED under the home: a binding that could name an absolute path or
    /// climb out with `..` would let one instance's extraction write into
    /// another's store — the same silent-wrong-target failure
    /// `crate::stores::binding` exists to prevent.
    /// What: `Ok(<home>/<root>)` for a declared relative, traversal-free
    /// `root`; `Ok(<home>/okg)` when `root` is absent (the default layout);
    /// [`AssistantError::UnconfinedStoreRoot`] for anything absolute, empty, or
    /// containing a `..`/prefix component.
    /// Test: `super::tests::store_root_tests` — the whole module.
    pub fn store_root(&self, binding: &AgentStoreBinding) -> Result<PathBuf, AssistantError> {
        let Some(declared) = binding
            .root
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        else {
            return Ok(self.okg_dir());
        };
        let unconfined = |reason: &str| AssistantError::UnconfinedStoreRoot {
            store: binding.name.clone(),
            root: declared.to_string(),
            reason: reason.to_string(),
        };
        let mut out = self.path.clone();
        for component in Path::new(declared).components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(unconfined(
                        "climbs out of the assistant's home with `..`; a store root must \
                         stay inside the home it belongs to",
                    ));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(unconfined(
                        "is an absolute path; a store root is relative to the assistant's home",
                    ));
                }
            }
        }
        if out == self.path {
            return Err(unconfined(
                "names the home directory itself, not a store inside it",
            ));
        }
        Ok(out)
    }

    /// Create any missing part of the home, seeding the two files (#4325).
    ///
    /// Why: "AUTO-CREATED, APP-GENERATED" is the ticket's own wording, and
    /// "app-generated" was clarified by the owner (2026-07-29) to mean
    /// ownership of ORIGIN — the app creates it — with NO write-enforcement and
    /// no narrowing semantics. So this call is additive only: every entry that
    /// already exists is left exactly as the user left it, including a file
    /// they emptied or rewrote. Overwriting would make the directory
    /// app-owned in the sense the owner explicitly ruled out.
    /// What: creates the home, [`AGENTS_DIR`], [`OKG_DIR`] and
    /// [`ATTACHMENTS_DIR`], and writes [`INSTRUCTIONS_FILE`] /
    /// [`CONFIG_FILE`] ONLY when absent. Returns the paths it actually
    /// created, so a caller can report what it generated rather than guess.
    /// Idempotent: a second call on an intact home creates nothing.
    /// Test: `super::tests::home_tests::ensure_creates_the_whole_layout`,
    /// `super::tests::home_tests::ensure_is_idempotent`,
    /// `super::tests::home_tests::ensure_never_overwrites_user_edits`.
    pub fn ensure(&self) -> Result<Created, AssistantError> {
        let mut created = Created::default();
        for dir in [
            self.path.clone(),
            self.agents_dir(),
            self.okg_dir(),
            self.attachments_dir(),
        ] {
            if !dir.is_dir() {
                std::fs::create_dir_all(&dir).map_err(|source| AssistantError::Io {
                    path: dir.clone(),
                    source,
                })?;
                created.paths.push(dir);
            }
        }
        self.seed(
            self.instructions_path(),
            seed_instructions(&self.id),
            &mut created,
        )?;
        self.seed(self.config_path(), seed_config(&self.id), &mut created)?;
        Ok(created)
    }

    /// Write `body` to `path` only when nothing is there. See [`Self::ensure`].
    fn seed(
        &self,
        path: PathBuf,
        body: String,
        created: &mut Created,
    ) -> Result<(), AssistantError> {
        if path.exists() {
            return Ok(());
        }
        std::fs::write(&path, body).map_err(|source| AssistantError::Io {
            path: path.clone(),
            source,
        })?;
        created.paths.push(path);
        Ok(())
    }
}

/// What one [`AssistantHome::ensure`] call actually generated.
///
/// Why: the caller reports "created your home directory" honestly, and a second
/// run reports nothing rather than repeating itself.
/// What: the paths created by THIS call, in creation order; empty when the home
/// was already intact.
/// Test: `super::tests::home_tests::ensure_is_idempotent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Created {
    pub paths: Vec<PathBuf>,
}

impl Created {
    /// Whether this call generated nothing. Test: `super::tests::home_tests::ensure_is_idempotent`.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// The `config.toml` shape at the root of an assistant's home (#4325).
///
/// Why: the ticket puts a `config.toml` in the home, and [`super::health`] has
/// to tell "malformed" from "fine" — which needs a shape to parse against.
/// Kept DELIBERATELY minimal and non-authoritative: the agent's behaviour still
/// comes from `agent.toml`, and this file exists so instance-scoped settings
/// (#4281's persisted selection, #4282's attached-index list) have a home to
/// land in without another format decision.
/// What: `id` plus an optional `display_name`. Unknown keys are IGNORED, not
/// rejected — a user's hand-added key must never make their home "malformed".
/// Test: `super::tests::health_tests::tolerates_unknown_config_keys`,
/// `super::tests::health_tests::reports_malformed_config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AssistantHomeConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// The seeded `instructions.md` body for a fresh home.
fn seed_instructions(id: &AssistantInstanceId) -> String {
    format!(
        "# {id}\n\n\
         Instructions for the `{id}` assistant instance.\n\n\
         This file is yours to edit. It was generated once, when the home\n\
         directory was created, and is never rewritten — if it goes missing or\n\
         cannot be read, the assistant tells you and helps you restore it\n\
         rather than silently replacing what you wrote.\n"
    )
}

/// The seeded `config.toml` body for a fresh home.
fn seed_config(id: &AssistantInstanceId) -> String {
    format!(
        "# Configuration for the `{id}` assistant instance (generated by\n\
         # trusty-agents, issue #4325). Safe to edit by hand.\n\
         id = \"{id}\"\n"
    )
}
