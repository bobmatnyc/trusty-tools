//! The trust-anchor write floor of `tm hook --pm-guard` (#8878, D4 and D8).
//!
//! Why: `~/.trusty-mpm/config.toml` holds the operator's grants — the
//! `[supervisor] projects` allowlist that makes a session the Architect — so a
//! session able to write it can grant itself anything the file grants. Owner
//! ruling 2026-09-29 (issue #8878): the trust anchors are that file plus
//! `~/.trusty-mpm/twin/armed/*.json` while such files still exist; the
//! Architect session may write them, no PM and no agent may; and the floor
//! holds under `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`.
//! Ruling A (#8878, 2026-09-29) makes the Architect identity process-bound,
//! and adds its launch records, `~/.trusty-mpm/architect-launch/`, as an anchor.
//! What: [`deny_trust_anchor_write`] runs [`evaluate`] and prints the deny.
//! [`evaluate`] reads the call's writes — the edit tools' target, or for `Bash`
//! the classifier plus the `cp`/`mv`/`ln`/`install`/`sed -i` destinations and
//! the sources of `ln`, `mv`, `cp -l` and `cp -s` ([`anchor_writes`]) — and
//! allows a call that writes nothing. It then allows the Architect's main
//! thread ([`architect_main_thread`], the one identity predicate): the
//! `claude` `tm fleet init` launched, matched on PID and start time, not an
//! environment claim. Every other writer is denied when a target or source
//! resolves, symlinks and hard links followed, to an anchor or to a directory
//! above one — which includes `twin/` and `twin/armed/` whether or not a record
//! is live — or to anything under `architect-launch/`. Anchor names and the
//! `.json` and `.architect` extensions compare ASCII case-insensitively, as
//! APFS does.
//! FAIL-CLOSED (Q4/Q5): an unknown home, a config location that does not
//! resolve, a target that does not resolve, a target built from an expansion,
//! glob or `cd` whose file name could name an anchor, an unplaceable write
//! (including a Q2 verb run by `xargs`), and an identity that cannot be
//! established are each a deny. An arming directory that does not resolve
//! fences the `twin/` tree only, not every write. The caller reads stdin before
//! the bypass variables, so an unreadable payload denies.
//! Deletes (#8878 Q2 delete ruling, Architect on PR #8919): `rm`, `unlink`,
//! `trash`, `rmdir` (with `-p`, each parent too), `shred`, `truncate` and a
//! `find` that deletes (`-delete`, or `-exec`/`-execdir`/`-ok`/`-okdir` running
//! a delete verb, `mv` or a shell) are anchor writes, as is `mv` OUT of an
//! anchor (its source). A delete is judged where the entry sits and where it
//! leads, so `rm -r` of a directory above an anchor denies; a glob in the last
//! component judges its directory. Wrappers resolve through the #8735
//! program-word resolver (`command`, `env`, `sudo`, `nice`, …), and a delete
//! run by `xargs` is unplaceable, so it denies.
//! Limit: after a `cd`, a relative path cannot be placed, so `cd X && cp Y .`
//! is denied for every session but the Architect's main thread: `.` could be
//! `~/.trusty-mpm`, and the guard does not evaluate the `cd`. Spell the
//! destination as an absolute path instead.
//! Residual: writers and deleters outside the verb set — `dd`, `rsync`
//! (including `--remove-source-files`), `git rm`/`git clean`, `srm`,
//! interpreters (`python -c`, `perl -e`, `node -e`), a `find -exec` whose
//! program is none of the above (an interpreter or `env -S` string), and a
//! write or delete from an executed script file (#8879) — are not seen.
//! Test: `pm_guard_trust_anchor_tests.rs` and, for deletes,
//! `pm_guard_trust_anchor_delete_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_trust_anchor_8878.rs`.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use trusty_mpm::core::architect_launch::{self, ARCHITECT_DIR, ARCHITECT_EXT};
use trusty_mpm::core::architect_session::SESSION_EXT;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;
use trusty_mpm::core::twin_arming::{self, ARMED_DIR};
use trusty_mpm::core::twin_identity::ClaudeProcess;

use crate::commands::misc::SUB_AGENT_ENV;
use crate::commands::pm_guard::{EDIT_TOOLS, edit_tool_target_path};
use crate::commands::pm_guard_architect_reason::{architect_main_thread, with_identity};
use crate::commands::pm_guard_bash::{AnchorWrite, UnplaceableWrite, anchor_writes};
use crate::commands::pm_guard_deny_log::{DenyContext, audit_denied_tool};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;
use crate::commands::pm_guard_trust_anchor_paths::{
    FileId, Placed, Resolved, dequote, file_id, has_shell_pattern, is_json, place, resolve,
    same_ci, starts_with_ci,
};

/// The rule name recorded with each deny.
pub(crate) const TRUST_ANCHOR_RULE: &str = "trust-anchor-write";

/// The `~/.trusty-mpm` root, relative to the home directory.
pub(crate) const ANCHOR_ROOT: &str = ".trusty-mpm";

/// The config anchor, under [`ANCHOR_ROOT`].
const CONFIG_ANCHOR: &str = "config.toml";

/// The hook's ambient inputs, injectable so every fail-closed arm is testable.
#[derive(Debug, Clone, Default)]
pub(crate) struct HookEnv {
    /// The home directory; `None` when unknown or not absolute.
    pub(crate) home: Option<PathBuf>,
    /// Whether `CLAUDE_MPM_SUB_AGENT` is set.
    pub(crate) sub_agent: bool,
    /// `TRUSTY_MPM_SESSION_PROFILE`, the #8453 launch stamp.
    pub(crate) stamp: Option<OsString>,
    /// `CLAUDE_PROJECT_DIR`, the session's launch directory.
    pub(crate) project_dir: Option<OsString>,
    /// The hook's nearest `claude` ancestor, read only when needed.
    pub(crate) claude: ClaudeLookup,
}

impl HookEnv {
    /// This process's inputs. The home is `dirs::home_dir`, the directory
    /// `MpmConfig::load_default` reads, so the anchor is the file it loads.
    pub(crate) fn ambient() -> Self {
        Self {
            home: dirs::home_dir().filter(|home| home.is_absolute()),
            sub_agent: std::env::var_os(SUB_AGENT_ENV).is_some(),
            stamp: std::env::var_os(session_profile::SESSION_PROFILE_ENV),
            project_dir: std::env::var_os(session_profile::PROJECT_DIR_ENV),
            claude: ClaudeLookup::live(),
        }
    }
}

/// The lookup of the hook's nearest `claude` ancestor (#8878, ruling A).
///
/// Why: the walk spawns `ps`, so it runs only for a write that could be the
/// Architect's; a test injects a process table instead.
/// What: a shared closure. [`Default`] is fail-closed: it reports that no
/// process table was supplied.
#[derive(Clone)]
pub(crate) struct ClaudeLookup(Arc<ClaudeLookupFn>);

/// The closure a [`ClaudeLookup`] runs.
type ClaudeLookupFn = dyn Fn() -> Result<Option<ClaudeProcess>, String> + Send + Sync;

impl ClaudeLookup {
    /// A lookup running `f`.
    pub(crate) fn new(
        f: impl Fn() -> Result<Option<ClaudeProcess>, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f))
    }

    /// The walk from this process over the live process table.
    fn live() -> Self {
        Self::new(|| twin_arming::nearest_claude_ancestor(std::process::id()))
    }

    /// Run the lookup.
    pub(crate) fn get(&self) -> Result<Option<ClaudeProcess>, String> {
        (self.0)()
    }
}

impl Default for ClaudeLookup {
    fn default() -> Self {
        Self::new(|| Err("no process table was supplied".to_owned()))
    }
}

impl std::fmt::Debug for ClaudeLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClaudeLookup")
    }
}

/// Deny a trust-anchor write: audit it, print the deny, and return `true`.
///
/// Why: `pm_guard` calls this on its guarded path; the bypass path reaches
/// [`evaluate`] through `pm_guard_floor`.
/// What: [`evaluate`] over the ambient inputs and the user config; `false`
/// (nothing printed) when it allows.
/// Test: `the_floor_denies_an_anchor_write_under_each_bypass`.
pub(crate) async fn deny_trust_anchor_write(url: &str, payload: &Value) -> bool {
    let Some(reason) = evaluate(payload, &HookEnv::ambient(), MpmConfig::load_default) else {
        return false;
    };
    audit_denied_tool(
        &DenyContext::from_payload(url, payload),
        TRUST_ANCHOR_RULE,
        &reason,
    )
    .await;
    println!("{}", build_pm_guard_deny_response(&reason));
    true
}

/// The trust-anchor verdict for one `PreToolUse` payload: `Some(reason)` denies.
///
/// Why/What: see the module doc. `config` is read only when the launch stamp
/// says supervisor. The payload `cwd` places a relative path; with no `cwd`
/// a relative path cannot be placed.
/// Test: `an_edit_tool_write_to_the_anchor_is_denied`,
/// `the_architect_main_thread_may_write_the_anchor`,
/// `an_unknown_home_denies_every_write`.
pub(crate) fn evaluate(
    payload: &Value,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Option<String> {
    let tool_name = payload.get("tool_name").and_then(Value::as_str)?;
    let writes = call_writes(tool_name, payload.get("tool_input"))?;
    // #8878: the ONE identity decision; an identity ruling swaps this call only.
    // #8878 PR-I: the deny names the identity check that failed.
    let Err(why) = architect_main_thread(payload, env, config) else {
        return None;
    };
    let cwd = payload.get("cwd").and_then(Value::as_str).map(Path::new);
    decide(writes, cwd, env.home.as_deref()).map(|reason| with_identity(reason, why))
}

/// Whether the call comes from the Architect session's main thread.
///
/// Why: ruling Q1 lets the Architect write the anchors and no agent. The
/// environment alone is spoofable, so ruling A (#8878, 2026-09-29) binds the
/// Architect to the `claude` process `tm fleet init` launched; a subagent of
/// it is an agent.
/// What: `true` only when ALL hold, cheapest first: the thread is the main
/// thread; [`session_profile::hook_profile`] resolves the supervisor (stamp,
/// `CLAUDE_PROJECT_DIR`, the project's `.trusty-mpm.toml` and the user-level
/// allowlist agree); and [`architect_launch::is_launched_architect`] matches
/// the hook's nearest `claude` ancestor to a launch record on PID, start time
/// and project. Any missing or unreadable input is `false`. #8878 PR-I: a
/// wrapper over [`architect_main_thread`], which names the check that failed.
/// Test: `the_architect_main_thread_may_write_the_anchor`,
/// `a_supervisor_stamp_without_a_launch_record_is_denied`,
/// `a_launch_record_binds_one_process`, `an_architect_subagent_is_denied`,
/// `an_unestablished_identity_is_denied`,
/// `the_reason_verdict_equals_the_bool_verdict`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "#8878 PR-I: the bool API; the hook's callers need the reason"
    )
)]
pub(crate) fn is_architect_main_thread(
    payload: &Value,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> bool {
    architect_main_thread(payload, env, config).is_ok()
}

/// The writes a tool call makes; `None` when it makes none.
fn call_writes(
    tool_name: &str,
    tool_input: Option<&Value>,
) -> Option<Result<Vec<AnchorWrite>, UnplaceableWrite>> {
    if EDIT_TOOLS.contains(&tool_name) {
        let path = edit_tool_target_path(tool_input)?;
        return Some(Ok(vec![AnchorWrite::Literal(path.to_string())]));
    }
    if tool_name != "Bash" {
        return None;
    }
    let command = tool_input?.get("command")?.as_str()?;
    match anchor_writes(command) {
        Ok(writes) if writes.iter().all(|w| *w == AnchorWrite::DirChange) => None,
        other => Some(other),
    }
}

/// The verdict once the call is known to write and not to be the Architect.
///
/// What: an unknown home ([`unknown_home_reason`]), an unplaceable write, or
/// a config location that does not resolve deny outright; otherwise the first
/// write [`Anchors::judge`] denies. A `cd` anywhere in the command makes every
/// relative path unplaceable.
/// Test: `an_unknown_home_denies_every_write`, `an_unplaceable_write_is_denied`,
/// `an_unlocatable_anchor_denies_every_write`.
fn decide(
    writes: Result<Vec<AnchorWrite>, UnplaceableWrite>,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
    // #8878 Q4 (i): no home, no anchor location — every write is refused.
    let Some(home) = home else {
        return Some(unknown_home_reason());
    };
    let writes = match writes {
        Ok(writes) => writes,
        Err(what) => return Some(unplaceable_reason(what)),
    };
    let anchors = match Anchors::locate(home) {
        Ok(anchors) => anchors,
        Err(path) => return Some(unlocatable_anchor_reason(&path)),
    };
    let base = if writes.contains(&AnchorWrite::DirChange) {
        None
    } else {
        cwd
    };
    writes
        .iter()
        .find_map(|write| anchors.judge(write, base, home))
}

/// Where the anchors are, resolved once per evaluation.
struct Anchors {
    /// Resolved paths that are protected together with every directory above
    /// them: the config, and the arming directory whenever it resolves — so
    /// `twin/` and `twin/armed/` are protected even with no record live.
    guarded: Vec<PathBuf>,
    /// The config's identity, when it exists.
    config_id: Option<FileId>,
    /// The live arming records, when the directory holds a `*.json` (or
    /// cannot be read).
    armed: Option<ArmedAnchors>,
    /// Trees every path under which is refused: the Architect launch
    /// directory, and the `twin/` tree when the arming directory does not
    /// resolve.
    fence: Vec<PathBuf>,
    /// Whether a `*.json` name could be an arming record: records are live,
    /// or the arming directory cannot be seen.
    records_live: bool,
    /// Every path component of every anchor, lexical and resolved: the names
    /// an unplaceable target's file name is compared against.
    names: Vec<String>,
}

/// The live arming records.
struct ArmedAnchors {
    /// The resolved `~/.trusty-mpm/twin/armed`.
    dir: PathBuf,
    /// Each record's identity.
    ids: Vec<FileId>,
}

impl Anchors {
    /// Resolve the anchors under `home`; `Err(path)` when the config's own
    /// location does not resolve.
    ///
    /// What: an arming directory that does not resolve (`twin` written as a
    /// file, a link loop) no longer refuses every write; it fences the `twin/`
    /// tree, lexical and resolved, and counts records as live. The Architect
    /// launch directory is always fenced.
    /// Test: `an_unlocatable_anchor_denies_every_write`,
    /// `a_blocked_arming_dir_fences_only_the_twin_tree`,
    /// `the_launch_record_dir_is_an_anchor`.
    fn locate(home: &Path) -> Result<Self, PathBuf> {
        let root = home.join(ANCHOR_ROOT);
        let lexical = root.join(CONFIG_ANCHOR);
        let Resolved::Path(config) = resolve(&lexical) else {
            return Err(lexical);
        };
        let armed_lexical = root.join(ARMED_DIR);
        let mut guarded = vec![config.clone()];
        let mut paths = vec![lexical, config.clone(), armed_lexical.clone()];
        let (armed, fence) = match resolve(&armed_lexical) {
            Resolved::Path(dir) => {
                guarded.push(dir.clone());
                paths.push(dir.clone());
                (live_armed(dir), Vec::new())
            }
            // #8878 finding 5: fail closed on the twin tree only.
            Resolved::Unresolvable => {
                let twin = Path::new(ARMED_DIR).iter().next().unwrap_or_default();
                let resolved_root = config.parent().map(|p| p.join(twin));
                (
                    None,
                    [Some(root.join(twin)), resolved_root]
                        .into_iter()
                        .flatten()
                        .collect(),
                )
            }
        };
        let records_live = armed.is_some() || !fence.is_empty();
        let mut fence = fence;
        // #8878 ruling A: the Architect launch directory is sealed — itself,
        // every directory above it, and everything under it.
        let launch = root.join(architect_launch::ARCHITECT_DIR);
        paths.push(launch.clone());
        match resolve(&launch) {
            Resolved::Path(dir) => {
                guarded.push(dir.clone());
                fence.push(dir.clone());
                paths.push(dir);
            }
            Resolved::Unresolvable => {
                fence.extend(config.parent().map(|p| p.join(ARCHITECT_DIR)));
                fence.push(launch);
            }
        }
        let mut names = Vec::new();
        for path in &paths {
            names.extend(path.components().filter_map(|c| match c {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            }));
        }
        Ok(Self {
            guarded,
            config_id: file_id(&config),
            records_live,
            armed,
            fence,
            names,
        })
    }

    /// Whether a resolved path is an anchor, a directory above one, a hard
    /// link to one, or inside the fenced `twin/` tree.
    fn is_anchor(&self, resolved: &Path) -> bool {
        if self.guarded.iter().any(|p| starts_with_ci(p, resolved)) {
            return true;
        }
        if self.fence.iter().any(|f| starts_with_ci(resolved, f)) {
            return true;
        }
        let id = file_id(resolved);
        if id.is_some() && id == self.config_id {
            return true;
        }
        self.armed.as_ref().is_some_and(|armed| {
            id.is_some_and(|id| armed.ids.contains(&id))
                || (resolved.parent().is_some_and(|p| same_ci(p, &armed.dir))
                    && resolved.file_name().is_some_and(is_json))
        })
    }

    /// Whether a target whose directory is unknown could still name an
    /// anchor: its file name is an expansion or glob, empty, `.`/`..`, an
    /// anchor path component, a `*.architect` launch record or its
    /// `*.architect-session` name sidecar (any case), or a `*.json` while
    /// records could be live.
    /// Test: `the_launch_record_dir_is_an_anchor`.
    fn could_be(&self, spelling: &str) -> bool {
        let name = spelling.trim_end_matches('/');
        let name = name.rsplit('/').next().unwrap_or(name);
        name.is_empty()
            || name == "."
            || name == ".."
            || name.starts_with('~')
            || has_shell_pattern(name)
            || self
                .names
                .iter()
                .any(|anchor| anchor.eq_ignore_ascii_case(name))
            // #8878 ruling A: a planted launch record would be an identity,
            // and (R1 critic HIGH) a planted name sidecar would rename it.
            || Path::new(name).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case(ARCHITECT_EXT) || ext.eq_ignore_ascii_case(SESSION_EXT)
            })
            || (self.records_live && is_json(name.as_ref()))
    }

    /// The deny for one write, or `None` when it misses every anchor.
    fn judge(&self, write: &AnchorWrite, base: Option<&Path>, home: &Path) -> Option<String> {
        match write {
            AnchorWrite::File(spelling) => {
                let word = dequote(spelling);
                self.judge_placed(&word, place(&word, base, home, true))
            }
            AnchorWrite::Literal(path) => self.judge_placed(path, place(path, base, home, false)),
            AnchorWrite::Source(word) => self.judge_placed(word, place(word, base, home, true)),
            AnchorWrite::Into {
                dest,
                names,
                dest_too,
            } => self.judge_into(dest, names, *dest_too, base, home),
            AnchorWrite::IntoUnnamed(dir) => self.judge_unnamed(dir, base, home),
            AnchorWrite::Delete(word) => self.judge_delete(word, base, home),
            AnchorWrite::DirChange => None,
        }
    }

    /// [`Self::judge`] for a delete (#8878 Q2 delete ruling).
    ///
    /// What: the entry where it sits (its parent resolved, the leaf not
    /// followed: `rm` removes a link, not its target) and where it leads are
    /// each judged; a parent that does not resolve denies. A glob confined to
    /// the last component ([`glob_dir`]) could remove any entry of its
    /// directory, so the directory is judged: an anchor, or above one, denies.
    /// Any other unplaced word denies when [`Self::could_be`] says so.
    /// Test: `each_delete_verb_on_each_anchor_is_denied`,
    /// `an_unplaceable_delete_is_denied`, `ordinary_deletes_stay_allowed`.
    fn judge_delete(&self, word: &str, base: Option<&Path>, home: &Path) -> Option<String> {
        let path = match place(word, base, home, true) {
            Placed::At(path) => path,
            Placed::Unknown => {
                let Some(dir) = glob_dir(word) else {
                    return self.could_be(word).then(|| unknown_reason(word));
                };
                return match place(dir, base, home, true) {
                    Placed::At(dir) => match resolve(&dir) {
                        Resolved::Path(dir) => self.is_anchor(&dir).then(|| anchor_reason(word)),
                        Resolved::Unresolvable => Some(unresolvable_reason(word)),
                    },
                    Placed::Unknown => self.could_be(dir).then(|| unknown_reason(word)),
                };
            }
        };
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            match resolve(parent) {
                Resolved::Path(parent) if self.is_anchor(&parent.join(name)) => {
                    return Some(anchor_reason(word));
                }
                Resolved::Path(_) => {}
                Resolved::Unresolvable => return Some(unresolvable_reason(word)),
            }
        }
        self.judge_placed(word, Placed::At(path))
    }

    /// [`Self::judge`] for a placed single path.
    fn judge_placed(&self, spelling: &str, placed: Placed) -> Option<String> {
        match placed {
            Placed::At(path) => match resolve(&path) {
                Resolved::Path(resolved) => {
                    self.is_anchor(&resolved).then(|| anchor_reason(spelling))
                }
                // #8878 Q4 (ii): an unresolvable target could be an anchor.
                Resolved::Unresolvable => Some(unresolvable_reason(spelling)),
            },
            Placed::Unknown => self.could_be(spelling).then(|| unknown_reason(spelling)),
        }
    }

    /// [`Self::judge`] for a copy verb: the destination, or each entry it
    /// receives when it is a directory.
    fn judge_into(
        &self,
        dest: &str,
        names: &[String],
        dest_too: bool,
        base: Option<&Path>,
        home: &Path,
    ) -> Option<String> {
        let entry = |name: &str| format!("{}/{name}", dest.trim_end_matches('/'));
        let path = match place(dest, base, home, true) {
            Placed::At(path) => path,
            Placed::Unknown => {
                if self.could_be(dest) {
                    return Some(unknown_reason(dest));
                }
                let name = names.iter().find(|name| self.could_be(name))?;
                return Some(unknown_reason(&entry(name)));
            }
        };
        let resolved = match resolve(&path) {
            Resolved::Path(resolved) => resolved,
            Resolved::Unresolvable => return Some(unresolvable_reason(dest)),
        };
        if !resolved.is_dir() || dest_too {
            if self.is_anchor(&resolved) {
                return Some(anchor_reason(dest));
            }
            if !resolved.is_dir() {
                return None;
            }
        }
        names.iter().find_map(|name| {
            if has_shell_pattern(name) || name.starts_with('~') {
                return self.could_be(name).then(|| unknown_reason(&entry(name)));
            }
            self.judge_placed(&entry(name), Placed::At(resolved.join(name)))
        })
    }

    /// [`Self::judge`] for a directory receiving entries the command does not
    /// name (`xargs cp -t DIR`, #8878 finding 4).
    ///
    /// What: denies a directory that cannot be placed or resolved, that is an
    /// anchor or above one, or that already holds an entry leading to an
    /// anchor (the copy would write through it). A missing directory receives
    /// nothing; one that cannot be listed denies.
    /// Test: `a_q2_verb_through_xargs_is_denied`.
    fn judge_unnamed(&self, dir: &str, base: Option<&Path>, home: &Path) -> Option<String> {
        let Placed::At(path) = place(dir, base, home, true) else {
            return Some(unknown_reason(dir));
        };
        let Resolved::Path(resolved) = resolve(&path) else {
            return Some(unresolvable_reason(dir));
        };
        if self.is_anchor(&resolved) {
            return Some(anchor_reason(dir));
        }
        let entries = match std::fs::read_dir(&resolved) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => return Some(unresolvable_reason(dir)),
            Ok(entries) => entries,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return Some(unresolvable_reason(dir));
            };
            let spelling = entry.path().display().to_string();
            let reason = self.judge_placed(&spelling, Placed::At(entry.path()));
            if reason.is_some() {
                return reason;
            }
        }
        None
    }
}

/// The live arming records in `dir`, or `None` when it holds no `*.json`.
///
/// What: a missing directory is not live. A directory that cannot be read, or
/// an entry that cannot be listed, is live — fail closed.
/// Test: `a_live_armed_record_is_an_anchor`, `an_unreadable_armed_dir_is_live`.
fn live_armed(dir: PathBuf) -> Option<ArmedAnchors> {
    let entries = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            return Some(ArmedAnchors {
                dir,
                ids: Vec::new(),
            });
        }
        Ok(entries) => entries,
    };
    let (mut live, mut ids) = (false, Vec::new());
    for entry in entries {
        let Ok(entry) = entry else {
            live = true;
            continue;
        };
        if is_json(&entry.file_name()) {
            live = true;
            ids.extend(file_id(&entry.path()));
        }
    }
    live.then_some(ArmedAnchors { dir, ids })
}

/// The directory a glob in `word`'s last component lists, or `None` when the
/// last component has no glob, or carries a `$`, backtick or brace, whose
/// expansion could hold a `/` (#8878 Q2 delete ruling).
fn glob_dir(word: &str) -> Option<&str> {
    let word = word.trim_end_matches('/');
    let (dir, name) = match word.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some(split) => split,
        None => (".", word),
    };
    (name.contains(['*', '?', '[']) && !name.contains(['$', '`', '{'])).then_some(dir)
}

/// The closing sentence every deny carries.
const REMEDY: &str = "Trust anchors are `~/.trusty-mpm/config.toml`, \
     `~/.trusty-mpm/architect-launch/` and, while any exist, \
     `~/.trusty-mpm/twin/armed/*.json`. Only the Architect session's main thread may write or \
     delete them — no PM, agent or subagent — and `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` \
     do not lift this rule. Ask the operator, or make the change from the Architect session.";

/// The deny for a write, link or rename that resolves to an anchor.
fn anchor_reason(target: &str) -> String {
    format!(
        "Trust-anchor write denied (#8878): `{target}` resolves to a trust anchor, a directory \
         holding one, or a link to one. {REMEDY}"
    )
}

/// The deny for a write whose target does not resolve (Q4 ii).
fn unresolvable_reason(target: &str) -> String {
    format!(
        "Trust-anchor write denied (#8878): `{target}` does not resolve (a symlink loop, an \
         unreadable directory, or a path through a file), so the guard cannot rule out that it \
         lands on a trust anchor. {REMEDY}"
    )
}

/// The deny for a target whose directory the guard cannot place.
fn unknown_reason(target: &str) -> String {
    format!(
        "Trust-anchor write denied (#8878): `{target}` depends on a shell expansion, glob, `~user` \
         or `cd` the guard does not evaluate, and its file name could name a trust anchor. Spell \
         the path out literally. {REMEDY}"
    )
}

/// The deny when the home directory is unknown (Q4 i).
fn unknown_home_reason() -> String {
    format!(
        "Trust-anchor write denied (#8878): the home directory is unknown, so the guard cannot \
         locate the trust anchors and refuses every write. {REMEDY}"
    )
}

/// The deny when an anchor's own location does not resolve.
fn unlocatable_anchor_reason(path: &Path) -> String {
    format!(
        "Trust-anchor write denied (#8878): `{}` does not resolve, so the guard cannot locate the \
         trust anchors and refuses every write. {REMEDY}",
        path.display()
    )
}

/// The deny for a write the guard cannot place at all.
fn unplaceable_reason(what: UnplaceableWrite) -> String {
    format!(
        "Trust-anchor write denied (#8878): this command writes through {what}, so the guard \
         cannot tell whether the write lands on a trust anchor. {REMEDY}"
    )
}

#[cfg(test)]
#[path = "pm_guard_trust_anchor_tests.rs"]
pub(crate) mod tests;
