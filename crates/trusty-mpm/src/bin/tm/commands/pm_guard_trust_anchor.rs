//! The trust-anchor write floor of `tm hook --pm-guard` (#8878, D4 and D8).
//!
//! Why: `~/.trusty-mpm/config.toml` holds the operator's grants — the
//! `[supervisor] projects` allowlist that makes a session the Architect — so a
//! session able to write it can grant itself anything the file grants. Owner
//! ruling 2026-09-29 (issue #8878): the trust anchors are that file plus
//! `~/.trusty-mpm/twin/armed/*.json` while such files still exist; the
//! Architect session may write them, no PM and no agent may; and the floor
//! holds under `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`.
//! What: [`deny_trust_anchor_write`] runs [`evaluate`] and prints the deny.
//! [`evaluate`] reads the call's writes — the edit tools' target, or for `Bash`
//! the classifier plus the `cp`/`mv`/`ln`/`install`/`sed -i` destinations
//! ([`anchor_writes`]) — and allows a call that writes nothing. It then allows
//! the Architect's main thread ([`is_architect_main_thread`]: the #8453
//! supervisor profile, which `tm fleet init` grants, AND a main-thread
//! payload). Every other writer is denied when a target resolves, symlinks and
//! hard links followed, to an anchor or to a directory above one.
//! FAIL-CLOSED (Q4/Q5): an unknown home, an anchor location that does not
//! resolve, a target that does not resolve, a target built from an expansion,
//! glob or `cd` whose file name could name an anchor, an unplaceable write,
//! and an identity that cannot be established are each a deny. The caller
//! reads stdin before the bypass variables, so an unreadable payload denies.
//! Residual: writers outside Q2 (`dd`, `rsync`, interpreters) and a write from
//! an executed script file (#8879) are not seen.
//! Test: `pm_guard_trust_anchor_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_trust_anchor_8878.rs`.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;
use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};

use crate::commands::misc::SUB_AGENT_ENV;
use crate::commands::pm_guard::{EDIT_TOOLS, edit_tool_target_path};
use crate::commands::pm_guard_bash::{AnchorWrite, UnplaceableWrite, anchor_writes};
use crate::commands::pm_guard_deny_log::{DenyContext, audit_denied_tool};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;

/// The rule name recorded with each deny.
pub(crate) const TRUST_ANCHOR_RULE: &str = "trust-anchor-write";

/// The `~/.trusty-mpm` root, relative to the home directory.
const ANCHOR_ROOT: &str = ".trusty-mpm";

/// The config anchor, under [`ANCHOR_ROOT`].
const CONFIG_ANCHOR: &str = "config.toml";

/// The arming-record directory, under [`ANCHOR_ROOT`]. Its `*.json` files are
/// anchors only while at least one exists (ruling Q1).
const ARMED_DIR: &str = "twin/armed";

/// Symlink hops followed through a dangling link before giving up.
const MAX_LINK_HOPS: usize = 40;

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
        }
    }
}

/// Deny a trust-anchor write: audit it, print the deny, and return `true`.
///
/// Why: `pm_guard` calls this from its bypass path and its guarded path.
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
    if is_architect_main_thread(payload, env, config) {
        return None;
    }
    let cwd = payload.get("cwd").and_then(Value::as_str).map(Path::new);
    decide(writes, cwd, env.home.as_deref())
}

/// Whether the call comes from the Architect session's main thread.
///
/// Why: ruling Q1 lets the Architect write the anchors and no agent. The
/// Architect is the #8453 supervisor profile (`tm fleet init` writes both
/// halves); a subagent of it is an agent.
/// What: [`thread_kind`] is [`ThreadKind::Main`] (a subagent, or a payload
/// that cannot establish the thread, is not) AND
/// [`session_profile::hook_profile`] resolves the supervisor: the launch
/// stamp, `CLAUDE_PROJECT_DIR`, the project's `.trusty-mpm.toml` and the
/// user-level allowlist all agree. Any missing or unreadable input is `false`.
/// Test: `the_architect_main_thread_may_write_the_anchor`,
/// `an_architect_subagent_is_denied`, `an_unestablished_identity_is_denied`.
pub(crate) fn is_architect_main_thread(
    payload: &Value,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> bool {
    thread_kind(payload, env.sub_agent) == ThreadKind::Main
        && session_profile::hook_profile(env.stamp.clone(), env.project_dir.clone(), config)
            .is_supervisor()
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
/// anchors that do not resolve deny outright; otherwise the first write
/// [`Anchors::judge`] denies. A `cd` anywhere in the command makes every
/// relative path unplaceable.
/// Test: `an_unknown_home_denies_every_write`, `an_unplaceable_write_is_denied`.
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

/// A file's `(device, inode)`, so a hard link to an anchor is recognised.
type FileId = (u64, u64);

#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// Where the anchors are, resolved once per evaluation.
struct Anchors {
    /// `~/.trusty-mpm/config.toml`, resolved (it need not exist).
    config: PathBuf,
    /// The config's identity, when it exists.
    config_id: Option<FileId>,
    /// The arming directory, when it holds a `*.json` file (or cannot be read).
    armed: Option<ArmedAnchors>,
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

/// A target's placement before the filesystem is consulted.
enum Placed {
    /// An absolute path.
    At(PathBuf),
    /// The path depends on an expansion, glob, `~user` or a `cd`.
    Unknown,
}

/// A placed path after symlinks are followed.
enum Resolved {
    /// The canonical path, or a missing leaf under a canonical parent.
    Path(PathBuf),
    /// A link loop, a permission error, or another error that is not
    /// "does not exist".
    Unresolvable,
}

impl Anchors {
    /// Resolve the anchors under `home`; `Err(path)` names one that does not.
    fn locate(home: &Path) -> Result<Self, PathBuf> {
        let root = home.join(ANCHOR_ROOT);
        let lexical = root.join(CONFIG_ANCHOR);
        let Resolved::Path(config) = resolve(&lexical) else {
            return Err(lexical);
        };
        let armed_lexical = root.join(ARMED_DIR);
        let Resolved::Path(armed_dir) = resolve(&armed_lexical) else {
            return Err(armed_lexical);
        };
        let armed = live_armed(armed_dir);
        let mut names = Vec::new();
        let mut paths = vec![lexical, config.clone()];
        if let Some(armed) = &armed {
            paths.extend([armed_lexical, armed.dir.clone()]);
        }
        for path in &paths {
            names.extend(path.components().filter_map(|c| match c {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            }));
        }
        Ok(Self {
            config_id: file_id(&config),
            config,
            armed,
            names,
        })
    }

    /// Whether a resolved path is an anchor, a directory above one, or a hard
    /// link to one.
    fn is_anchor(&self, resolved: &Path) -> bool {
        if self.config.starts_with(resolved) {
            return true;
        }
        let id = file_id(resolved);
        if id.is_some() && id == self.config_id {
            return true;
        }
        self.armed.as_ref().is_some_and(|armed| {
            armed.dir.starts_with(resolved)
                || id.is_some_and(|id| armed.ids.contains(&id))
                || (resolved.parent() == Some(armed.dir.as_path())
                    && resolved.extension().is_some_and(|ext| ext == "json"))
        })
    }

    /// Whether a target whose directory is unknown could still name an
    /// anchor: its file name is an expansion or glob, empty, `.`/`..`, an
    /// anchor path component, or a `*.json` while records are live.
    fn could_be(&self, spelling: &str) -> bool {
        let name = spelling.trim_end_matches('/');
        let name = name.rsplit('/').next().unwrap_or(name);
        name.is_empty()
            || name == "."
            || name == ".."
            || name.starts_with('~')
            || has_shell_pattern(name)
            || self.names.iter().any(|anchor| anchor == name)
            || (self.armed.is_some() && name.ends_with(".json"))
    }

    /// The deny for one write, or `None` when it misses every anchor.
    fn judge(&self, write: &AnchorWrite, base: Option<&Path>, home: &Path) -> Option<String> {
        match write {
            AnchorWrite::File(spelling) => {
                let word = dequote(spelling);
                self.judge_placed(&word, place(&word, base, home, true))
            }
            AnchorWrite::Literal(path) => self.judge_placed(path, place(path, base, home, false)),
            AnchorWrite::Into {
                dest,
                names,
                dest_too,
            } => self.judge_into(dest, names, *dest_too, base, home),
            AnchorWrite::DirChange => None,
        }
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
}

/// The live arming records in `dir`, or `None` when it holds no `*.json`.
///
/// What: a missing directory is not live. A directory that cannot be read, or
/// an entry that cannot be listed, is live — fail closed.
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
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            live = true;
            ids.extend(file_id(&path));
        }
    }
    live.then_some(ArmedAnchors { dir, ids })
}

/// Whether a shell word carries an expansion or a pattern the guard does not
/// evaluate: `$`, a backtick, a glob (`*`, `?`, `[`) or a brace.
fn has_shell_pattern(word: &str) -> bool {
    word.contains(['$', '`', '*', '?', '[', '{'])
}

/// A raw redirect word with its quotes removed, when it lexes to one word.
/// The redirect scan hands back the word as written (`"$HOME/x"`).
fn dequote(word: &str) -> String {
    if !word.contains(['\'', '"', '\\']) {
        return word.to_string();
    }
    match shlex::split(word) {
        Some(mut words) if words.len() == 1 => words.remove(0),
        _ => word.to_string(),
    }
}

/// Place `word` as an absolute path, or [`Placed::Unknown`].
///
/// What: `~` and `~/…` join `home`; `~user` is unknown. With `shell`, a
/// first segment `$HOME`/`${HOME}` joins `home` and `$PWD`/`${PWD}` joins
/// `base`, and any other expansion or glob is unknown. A relative path joins
/// `base`, and is unknown without one.
fn place(word: &str, base: Option<&Path>, home: &Path, shell: bool) -> Placed {
    let (root, rest) = if let Some(rest) = word.strip_prefix('~') {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Placed::Unknown;
        }
        (Some(home), rest)
    } else if shell {
        match word.split_once('/').unwrap_or((word, "")) {
            ("$HOME" | "${HOME}", rest) => (Some(home), rest),
            ("$PWD" | "${PWD}", rest) => match base {
                Some(base) => (Some(base), rest),
                None => return Placed::Unknown,
            },
            _ => (None, word),
        }
    } else {
        (None, word)
    };
    if shell && has_shell_pattern(rest) {
        return Placed::Unknown;
    }
    let rest = rest.trim_start_matches('/');
    match root {
        Some(root) => Placed::At(root.join(rest)),
        None if Path::new(word).is_absolute() => Placed::At(PathBuf::from(word)),
        None => base.map_or(Placed::Unknown, |base| Placed::At(base.join(word))),
    }
}

/// Follow `path` through the filesystem.
///
/// What: the canonical path when it exists; for a dangling symlink, the link's
/// destination, resolved in turn (at most [`MAX_LINK_HOPS`]); for a missing
/// leaf, its resolved parent joined with the leaf name. An entry that exists
/// but will not canonicalize, and every error other than "not found", is
/// [`Resolved::Unresolvable`].
/// Test: `an_unresolvable_target_is_denied`,
/// `a_dangling_symlink_onto_the_anchor_is_denied`.
fn resolve(path: &Path) -> Resolved {
    resolve_hops(path, MAX_LINK_HOPS)
}

fn resolve_hops(path: &Path, hops: usize) -> Resolved {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Resolved::Path(canonical);
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() && hops > 0 => {
            let (Ok(dest), Some(parent)) = (std::fs::read_link(path), path.parent()) else {
                return Resolved::Unresolvable;
            };
            resolve_hops(&parent.join(dest), hops - 1)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                return Resolved::Unresolvable;
            };
            match resolve_hops(parent, hops) {
                Resolved::Path(parent) => Resolved::Path(parent.join(name)),
                Resolved::Unresolvable => Resolved::Unresolvable,
            }
        }
        _ => Resolved::Unresolvable,
    }
}

/// The closing sentence every deny carries.
const REMEDY: &str = "Trust anchors are `~/.trusty-mpm/config.toml` and, while any exist, \
     `~/.trusty-mpm/twin/armed/*.json`. Only the Architect session's main thread may write them \
     — no PM, agent or subagent — and `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` \
     do not lift this rule. Ask the operator, or make the change from the Architect session.";

/// The deny for a write that resolves to an anchor.
fn anchor_reason(target: &str) -> String {
    format!("Trust-anchor write denied (#8878): `{target}` resolves to a trust anchor. {REMEDY}")
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
mod tests;
