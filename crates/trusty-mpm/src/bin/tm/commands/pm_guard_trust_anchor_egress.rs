//! The #8454 G3 additions to the #8878 trust-anchor floor (slice S2).
//!
//! Why: the `tm hook --egress-guard` entry is only as strong as the files that
//! register it and the config the hook reads. Bob ruling 5 extends the #8878
//! anchor set with `~/.trusty-tools/trusty-mpm/config.yaml` and the egress
//! entry. Architect D2 protects the project tier by content rather than by a
//! blanket anchor, because engineers edit committed settings; D6 extends the
//! Architect main-thread exemption to the new anchors only.
//! What: three pieces, each consumed by the parent module.
//! [`EgressAnchors::locate`] adds `config.yaml` and the managed
//! `claude-config/settings.json` and `settings.local.json` to the anchor set,
//! under the parent's resolve, symlink, hard-link, case and fail-closed rules —
//! so the directories above them are protected too, and `rm -rf` or `mv` of
//! `~/.trusty-tools/trusty-mpm` is denied. [`Anchors::judge_delete`] judges an
//! `rm`/`rmdir`/`unlink` operand, a glob or expansion by its literal parent.
//! [`project_settings_verdict`] is the D2 content rule for a project's
//! `.claude/settings.json` and `settings.local.json`, and binds every session,
//! the Architect included: an Edit or Write whose result drops or weakens the
//! egress entry, or sets `disableAllHooks`, is denied; a Bash write is denied
//! while either file of that `.claude/` directory carries the entry; an
//! unreadable or malformed file denies every write to it. Deleting, moving or
//! copying over the `.claude/` directory itself is a write to both files.
//! Residual: a Bash write that creates `disableAllHooks` in a project whose
//! settings never carried the entry; a hard link to a project settings file
//! made before this rule (a new `ln` of one is judged as its source);
//! `xargs rm`, `find -delete`, a glob after a `cd`, an operand whose
//! parent is an expansion (`rm $F`), and a `cp -r` of a `.claude` tree that
//! sets `disableAllHooks` into a project with no entry (its source is unseen).
//! Test: `pm_guard_trust_anchor_egress_tests.rs`; end to end in
//! `tests/tm_hook_egress_anchor_8454.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::standalone::hooks::egress::{
    EgressGuardState, disables_all_hooks, egress_guard_state,
};
use trusty_mpm::core::trusty_tools_config::{CRATE_NAME, managed_claude_config_dir_at};

use super::{
    AnchorWrite, Anchors, EDIT_TOOLS, FileId, Placed, Resolved, UnplaceableWrite, dequote, file_id,
    place, resolve, starts_with_ci,
};

/// The characters that start a glob or an expansion in a shell word.
const PATTERN_CHARS: [char; 6] = ['$', '`', '*', '?', '[', '{'];

/// The project settings file names the content rule guards.
const SETTINGS_NAMES: [&str; 2] = ["settings.json", "settings.local.json"];

/// The directory a project settings file lives in.
const CLAUDE_DIR: &str = ".claude";

/// The #8454 anchors under one home, resolved.
pub(super) struct EgressAnchors {
    /// Resolved paths protected together with every directory above them.
    pub(super) guarded: Vec<PathBuf>,
    /// Lexical and resolved spellings, whose components feed the parent's
    /// name list for unplaceable targets.
    pub(super) paths: Vec<PathBuf>,
    /// The identities of the anchors that exist, for hard links.
    pub(super) ids: Vec<FileId>,
}

impl EgressAnchors {
    /// Resolve the anchors under `home`; `Err(path)` when one's location does
    /// not resolve, which the parent turns into a deny of every write.
    ///
    /// Test: `egress_anchor_config_yaml_writes_are_denied`,
    /// `egress_anchor_unresolvable_location_denies_every_write`.
    pub(super) fn locate(home: &Path) -> Result<Self, PathBuf> {
        let managed = managed_claude_config_dir_at(home);
        let lexical = [
            trusty_common::crate_config::crate_config_path_at(home, CRATE_NAME),
            managed.join(SETTINGS_NAMES[0]),
            managed.join(SETTINGS_NAMES[1]),
        ];
        let mut out = Self {
            guarded: Vec::new(),
            paths: Vec::new(),
            ids: Vec::new(),
        };
        for path in lexical {
            let Resolved::Path(resolved) = resolve(&path) else {
                return Err(path);
            };
            out.ids.extend(file_id(&resolved));
            out.guarded.push(resolved.clone());
            out.paths.extend([path, resolved]);
        }
        Ok(out)
    }
}

impl Anchors {
    /// The deny for one delete-verb operand, or `None`.
    ///
    /// What: an operand with no glob or expansion is judged as a write to it.
    /// One with a glob or an expansion can delete any entry of its literal
    /// parent ([`literal_parent`]), so it is denied when that parent is an
    /// anchor, a directory above one, inside a fenced tree, or does not
    /// resolve. A glob in the first segment judges the working directory. An
    /// expansion in the first segment has no parent to place; a literal last
    /// component is then judged by the parent's name rule (`rm "$D/config.yaml"`).
    /// Only an operand whose parent cannot be placed at all — `rm $F`, a
    /// relative path after a `cd` — is allowed, the documented residual.
    /// Test: `egress_anchor_rm_and_mv_of_the_config_dir_are_denied`,
    /// `egress_anchor_delete_with_an_expansion_is_judged_by_its_parent`,
    /// `egress_anchor_delete_through_a_looped_glob_parent_is_denied`.
    pub(super) fn judge_delete(
        &self,
        word: &str,
        base: Option<&Path>,
        home: &Path,
    ) -> Option<String> {
        let word = dequote(word);
        let Some(at) = pattern_at(&word) else {
            return self.judge_placed(&word, place(&word, base, home, true));
        };
        let parent = match literal_parent(&word, at) {
            Some(parent) => parent,
            // A glob in the first segment deletes entries of the cwd.
            None if !word[at..].starts_with(['$', '`']) => ".",
            // #8454 fix round: an expansion in the first segment — only a
            // literal last component names anything the guard can judge.
            None => {
                let last = word.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                let judged = word.contains('/') && !last.contains(PATTERN_CHARS);
                return (judged && self.could_be(last)).then(|| super::unknown_reason(&word));
            }
        };
        let Placed::At(path) = place(parent, base, home, true) else {
            return None;
        };
        match resolve(&path) {
            Resolved::Path(dir) => {
                let above = self.guarded.iter().any(|g| starts_with_ci(g, &dir));
                let fenced = self.fence.iter().any(|f| starts_with_ci(&dir, f));
                (above || fenced).then(|| super::anchor_reason(&word))
            }
            Resolved::Unresolvable => Some(super::unresolvable_reason(&word)),
        }
    }
}

/// Where a shell word's first glob or expansion starts, a leading `$HOME`
/// (which `place` resolves) skipped.
fn pattern_at(word: &str) -> Option<usize> {
    let skip = ["${HOME}", "$HOME"]
        .iter()
        .find(|p| {
            word.strip_prefix(**p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .map_or(0, |p| p.len());
    word[skip..].find(PATTERN_CHARS).map(|at| at + skip)
}

/// The literal directory before the segment holding byte `at`; `None` when
/// the pattern is in the first segment.
fn literal_parent(word: &str, at: usize) -> Option<&str> {
    match word[..at].rfind('/') {
        Some(0) => Some("/"),
        Some(slash) => Some(&word[..slash]),
        None => None,
    }
}

/// The D2 project-settings verdict for one call: `Some(reason)` denies.
///
/// What: places every write the call makes (an edit tool's target literally,
/// a Bash word as the shell would) and judges each one that names a project
/// settings file — by its spelling or after symlinks — with
/// [`judge_settings_write`]. A Bash target the guard cannot place whose file
/// name is a settings name denies. #8454 fix round: a delete, rename source or
/// copy destination that is a `.claude` directory — or a glob whose literal
/// parent is one — is judged by [`judge_claude_dir`] as a write to both
/// settings files. An unknown home, and a Bash command the parent cannot place
/// at all, are left to the parent's own rules.
/// Test: the `egress_project_settings_*` tests.
pub(super) fn project_settings_verdict(
    tool_name: &str,
    input: Option<&Value>,
    writes: &Result<Vec<AnchorWrite>, UnplaceableWrite>,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
    let (home, Ok(writes)) = (home?, writes) else {
        return None;
    };
    let edit = EDIT_TOOLS.contains(&tool_name).then_some(input).flatten();
    let base = if writes.contains(&AnchorWrite::DirChange) {
        None
    } else {
        cwd
    };
    let mut targets: Vec<(String, bool)> = Vec::new();
    // #8454 fix round: `(spelling, incoming)` operands that may be a `.claude`
    // directory; `incoming` marks a tree moved into place.
    let mut dirs: Vec<(String, bool)> = Vec::new();
    for write in writes {
        match write {
            AnchorWrite::Literal(path) => targets.push((path.clone(), false)),
            AnchorWrite::File(word) => targets.push((dequote(word), true)),
            AnchorWrite::Source(word) => {
                targets.push((dequote(word), true));
                dirs.push((dequote(word), true));
            }
            AnchorWrite::Delete(word) => {
                let word = dequote(word);
                match pattern_at(&word) {
                    None => dirs.push((word.clone(), false)),
                    Some(at) => match literal_parent(&word, at) {
                        Some(parent) => dirs.push((parent.to_owned(), false)),
                        None if !word[at..].starts_with(['$', '`']) => {
                            dirs.push((".".to_owned(), false));
                        }
                        None => {}
                    },
                }
                targets.push((word, true));
            }
            AnchorWrite::Into {
                dest,
                names,
                dest_too,
            } => {
                targets.push((dest.clone(), true));
                let dir = dest.trim_end_matches('/');
                for name in names {
                    targets.push((format!("{dir}/{name}"), true));
                    dirs.push((format!("{dir}/{name}"), false));
                }
                if *dest_too {
                    dirs.push((dest.clone(), false));
                }
            }
            AnchorWrite::IntoUnnamed(dir) => {
                let dir = dir.trim_end_matches('/');
                targets.extend(SETTINGS_NAMES.iter().map(|n| (format!("{dir}/{n}"), true)));
            }
            AnchorWrite::DirChange => {}
        }
    }
    let dir_reason = dirs.iter().find_map(|(spelling, incoming)| {
        let Placed::At(lexical) = place(spelling, base, home, true) else {
            let name = spelling.trim_end_matches('/').rsplit('/').next()?;
            return name
                .eq_ignore_ascii_case(CLAUDE_DIR)
                .then(|| unplaceable_reason(spelling));
        };
        let dir = claude_dir(&lexical)?;
        judge_claude_dir(&dir, *incoming).map(|why| project_reason(spelling, &why))
    });
    if dir_reason.is_some() {
        return dir_reason;
    }
    targets.iter().find_map(|(spelling, shell)| {
        let Placed::At(lexical) = place(spelling, base, home, *shell) else {
            let name = spelling.trim_end_matches('/').rsplit('/').next()?;
            return is_settings_name(name).then(|| unplaceable_reason(spelling));
        };
        let dir = settings_dir(&lexical)?;
        judge_settings_write(edit, &lexical, &dir).map(|why| project_reason(spelling, &why))
    })
}

/// `path` when it, as spelled or after symlinks, is a `.claude` directory.
fn claude_dir(lexical: &Path) -> Option<PathBuf> {
    let named = |path: &Path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(CLAUDE_DIR))
    };
    match resolve(lexical) {
        Resolved::Path(resolved) if named(&resolved) => Some(resolved),
        _ if named(lexical) => Some(lexical.to_path_buf()),
        _ => None,
    }
}

/// Judge a delete, rename or copy that replaces the `.claude` directory `dir`
/// as a write to both its settings files (#8454 fix round).
///
/// What: either file unreadable or malformed denies; either carrying an
/// egress entry denies; for an `incoming` tree, either setting
/// `disableAllHooks` denies. A directory holding neither file is allowed.
/// Test: `egress_project_settings_claude_dir_operations_are_denied`,
/// `egress_project_settings_incoming_claude_tree_disabling_hooks_is_denied`.
fn judge_claude_dir(dir: &Path, incoming: bool) -> Option<String> {
    for name in SETTINGS_NAMES {
        let path = dir.join(name);
        let current = match read_settings(&path) {
            Ok(current) => current,
            Err(why) => return Some(format!("`{}` {why}", path.display())),
        };
        if state_of(&current) != EgressGuardState::Missing {
            return Some("the `.claude` directory's settings carry the egress entry".to_owned());
        }
        if incoming && matches!(&current, Current::Object(value, _) if disables_all_hooks(value)) {
            return Some(format!("`{}` sets `disableAllHooks`", path.display()));
        }
    }
    None
}

/// Whether a file name is a guarded settings name, in any case.
fn is_settings_name(name: &str) -> bool {
    SETTINGS_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// The `.claude/` directory when `path`, as spelled or after symlinks, is a
/// project settings file.
fn settings_dir(lexical: &Path) -> Option<PathBuf> {
    let shaped = |path: &Path| {
        let name = path.file_name()?.to_str()?;
        let parent = path.parent()?;
        let dir = parent.file_name()?.to_str()?;
        (is_settings_name(name) && dir.eq_ignore_ascii_case(CLAUDE_DIR))
            .then(|| parent.to_path_buf())
    };
    let resolved = match resolve(lexical) {
        Resolved::Path(resolved) => shaped(&resolved),
        Resolved::Unresolvable => None,
    };
    resolved.or_else(|| shaped(lexical))
}

/// One project settings file as read.
enum Current {
    /// No file.
    Absent,
    /// The parsed object (an empty file reads as `{}`) and its text.
    Object(Value, String),
}

/// Read `path`: `Err(why)` when it is unreadable or not a JSON object.
fn read_settings(path: &Path) -> Result<Current, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Current::Absent),
        Err(e) => return Err(format!("cannot be read ({e})")),
    };
    match parse_object(&text) {
        Some(value) => Ok(Current::Object(value, text)),
        None => Err("is not a JSON object".to_owned()),
    }
}

/// `text` as a JSON object, an empty text as `{}`; `None` otherwise.
fn parse_object(text: &str) -> Option<Value> {
    if text.trim().is_empty() {
        return Some(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
}

/// The egress state of a file as read; an absent file carries nothing.
fn state_of(current: &Current) -> EgressGuardState {
    match current {
        Current::Absent => EgressGuardState::Missing,
        Current::Object(value, _) => egress_guard_state(value),
    }
}

/// Judge one write to the settings file `file` in the `.claude/` directory
/// `dir`; `edit` is the edit tool's input, `None` for a Bash write.
///
/// What: either file of `dir` unreadable or malformed denies. A Bash write
/// denies while either file carries an egress entry. An edit denies when its
/// result sets `disableAllHooks`, or is weaker than `file` as read
/// (Present > Weakened > Missing); a result the guard cannot reproduce is
/// judged by whether `file` carries the entry or the input names
/// `disableAllHooks`.
fn judge_settings_write(edit: Option<&Value>, file: &Path, dir: &Path) -> Option<String> {
    let mut carried = false;
    for name in SETTINGS_NAMES {
        let path = dir.join(name);
        match read_settings(&path) {
            Ok(current) => carried |= state_of(&current) != EgressGuardState::Missing,
            Err(why) => return Some(format!("`{}` {why}", path.display())),
        }
    }
    let Some(input) = edit else {
        return carried.then(|| "the project's settings carry the egress entry".to_owned());
    };
    let current = match read_settings(file) {
        Ok(current) => current,
        Err(why) => return Some(format!("`{}` {why}", file.display())),
    };
    let before = state_of(&current);
    let text = match &current {
        Current::Absent => "",
        Current::Object(_, text) => text.as_str(),
    };
    let named_disable = input.to_string().contains("disableAllHooks");
    let Some(result) = edited(input, text).and_then(|r| parse_object(&r)) else {
        return (before != EgressGuardState::Missing || named_disable)
            .then(|| "the guard cannot reproduce this edit's result".to_owned());
    };
    if disables_all_hooks(&result) {
        return Some("the result sets `disableAllHooks`".to_owned());
    }
    (egress_guard_state(&result) < before)
        .then(|| "the result drops or weakens the egress entry".to_owned())
}

/// The text an edit tool would leave, or `None` when it cannot be reproduced.
fn edited(input: &Value, current: &str) -> Option<String> {
    if let Some(content) = input.get("content") {
        return content.as_str().map(str::to_owned);
    }
    let apply = |text: String, edit: &Value| {
        let old = edit.get("old_string")?.as_str()?;
        let new = edit.get("new_string")?.as_str()?;
        if old.is_empty() || !text.contains(old) {
            return None;
        }
        let all = edit.get("replace_all").and_then(Value::as_bool) == Some(true);
        Some(if all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        })
    };
    match input.get("edits").and_then(Value::as_array) {
        Some(edits) => edits.iter().try_fold(current.to_owned(), &apply),
        None => apply(current.to_owned(), input),
    }
}

/// The closing sentence of every project-settings deny.
const PROJECT_REMEDY: &str = "The `tm hook --egress-guard` PreToolUse entry and \
     `disableAllHooks` are protected in a project's `.claude/settings.json` and \
     `settings.local.json` for every session, the Architect included; edits that keep the entry \
     are allowed, and neither bypass variable lifts this rule. Ask the operator.";

/// The deny for a write to a guarded project settings file.
fn project_reason(target: &str, why: &str) -> String {
    format!("Project settings write denied (#8454): `{target}` — {why}. {PROJECT_REMEDY}")
}

/// The deny for a settings-named target the guard cannot place.
fn unplaceable_reason(target: &str) -> String {
    project_reason(
        target,
        "the name matches a protected name in a directory the guard cannot place (after a `cd`, \
         or behind an expansion or glob). Retry with the absolute path",
    )
}
