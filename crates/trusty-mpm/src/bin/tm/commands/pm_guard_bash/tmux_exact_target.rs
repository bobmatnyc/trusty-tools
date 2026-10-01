//! The exact-target floor of `tm hook --pm-guard` (#9001).
//!
//! Why: owner ruling 2026-10-01. tmux resolves a session name by exact match,
//! then by prefix, so `send-keys -t nosuch:0` reaches a live session whose
//! name starts with `nosuch`. #8443 made every target tm builds exact; this
//! floor holds a typed command to the same standard.
//! What: [`evaluate_tmux_exact_target`] takes the deny-set tmux commands
//! [`tmux_hits`] finds and checks each written `-t`/`-s` target against the
//! panes, windows and sessions its server lists. A target passes only when
//! every part names an existing object with no prefix or pattern step: an id,
//! an `=name`, a name equal to an existing one, a window or pane index. Every
//! caller is bound, the Architect included, under every bypass.
//! FAIL-CLOSED: a target the shell expands, a special token or glob, a server
//! tmux will not list (no binary, a query error, a refused spawn), and an
//! empty session part when the caller's pane is unknown all deny. No server
//! running lists nothing, so every target on it denies.
//! Residual: a hit this guard cannot read (`Hit::opaque`) and an omitted
//! target are left to the #8902 floor. The listing is read before the command
//! runs, so a session the same command creates denies.
//! Test: `tmux_exact_target_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_tmux_target_9001.rs`.

use super::architect_pane::PaneProbe;
use super::architect_pane_parse::{Hit, Target, tmux_hits};
use super::architect_pane_probe::Listed;

/// The rule name recorded with an exact-target deny.
pub(crate) const TMUX_TARGET_RULE: &str = "tmux-target-unresolved";

/// The `list-panes -F` format [`parse_objects`] reads, tab-separated.
pub(super) const OBJECT_FORMAT: &str = "#{session_id}\t#{window_id}\t#{window_index}\t\
     #{window_active}\t#{pane_id}\t#{pane_index}\t#{session_name}\t#{window_name}";

/// One pane of a server with the window and session it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TmuxObject {
    /// `#{session_id}`, `$N`.
    pub(crate) session: String,
    /// `#{window_id}`, `@N`.
    pub(crate) window: String,
    /// `#{window_index}` in this session.
    pub(crate) window_index: String,
    /// Whether this window is its session's current one.
    pub(crate) window_active: bool,
    /// `#{pane_id}`, `%N`.
    pub(crate) pane: String,
    /// `#{pane_index}` in its window.
    pub(crate) pane_index: String,
    /// `#{session_name}`.
    pub(crate) session_name: String,
    /// `#{window_name}`.
    pub(crate) window_name: String,
}

/// One server's objects, or why tmux would not list them.
type Listing = Result<Vec<TmuxObject>, String>;

/// Characters that make a target a special token, glob or format.
const SPECIAL: &[char] = &[
    '{', '}', '~', '!', '^', '*', '?', '[', ']', '#', ' ', '\t', '\n',
];

/// Classify a Bash command against the exact-target floor: `Some(reason)`
/// denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command.
/// What: each written target of each readable deny-set hit is checked against
/// the objects its server lists, each server listed once. The first target
/// the shell expands, whose server cannot be listed, or that does not resolve
/// exactly ([`resolve_exactly`]) denies, naming the target.
/// Test: `an_exact_target_passes_and_a_prefix_or_missing_one_denies`,
/// `an_unlistable_server_denies_every_target`,
/// `a_live_prefix_collision_denies_on_a_private_server`.
pub(crate) fn evaluate_tmux_exact_target(command: &str, probe: &dyn PaneProbe) -> Option<String> {
    let hits = tmux_hits(command);
    if hits.is_empty() {
        return None;
    }
    // A command that sets or reads a TMUX variable may move "current" (#8902).
    let current = (!command.contains("TMUX"))
        .then(|| probe.current_pane())
        .flatten();
    let mut listed: Vec<(&[String], Listing)> = Vec::new();
    for hit in hits.iter().filter(|h| h.opaque.is_none()) {
        for target in &hit.targets {
            let text = match target {
                Target::Literal(text) => text,
                Target::Dynamic(text) => {
                    return Some(refusal(hit, text, "the shell expands it"));
                }
                _ => continue,
            };
            if !listed.iter().any(|(s, _)| *s == hit.server.as_slice()) {
                listed.push((&hit.server, probe.objects(&hit.server)));
            }
            let Some((_, listing)) = listed.iter().find(|(s, _)| *s == hit.server.as_slice())
            else {
                return Some(unlisted(hit, text, "no listing"));
            };
            let objects = match listing {
                Ok(objects) => objects,
                Err(err) => return Some(unlisted(hit, text, err)),
            };
            if let Err(why) = resolve_exactly(text, objects, current.as_deref()) {
                return Some(refusal(hit, text, why));
            }
        }
    }
    None
}

/// `Ok` when `text` names existing objects with no prefix or pattern step.
///
/// What: `session:window.pane` resolves each part in turn; an empty session
/// part is the caller's session. A word with no colon is an id (`%N`, `@N`,
/// `$N`, a `.pane` allowed after `@N`), an `=name`, or a session name that
/// no window name starts with — tmux tries windows of a session it picks
/// before sessions.
/// Test: `an_exact_target_passes_and_a_prefix_or_missing_one_denies`.
pub(super) fn resolve_exactly(
    text: &str,
    rows: &[TmuxObject],
    current: Option<&str>,
) -> Result<(), &'static str> {
    if text.is_empty() || text == "=" || text.contains(SPECIAL) {
        return Err("it is empty, a special token, a glob or a format");
    }
    if text.starts_with(['+', '-']) {
        return Err("it is a relative token");
    }
    let Some((session, rest)) = text.split_once(':') else {
        return bare(text, rows);
    };
    let in_session = session_rows(session, rows, current)?;
    let (window, pane) = rest.split_once('.').unwrap_or((rest, ""));
    let in_window = window_rows(window, &in_session)?;
    pane_exists(pane, &in_window)
}

/// Whether `word` is a whole `sigil` id: the sigil, then digits only.
fn is_id(word: &str, sigil: char) -> bool {
    word.strip_prefix(sigil)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `word` is a non-empty run of digits.
fn is_index(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())
}

/// A target with no colon.
fn bare(text: &str, rows: &[TmuxObject]) -> Result<(), &'static str> {
    if let Some(exact) = text.strip_prefix('=') {
        let found = rows
            .iter()
            .any(|r| r.session_name == exact || r.window_name == exact);
        return found
            .then_some(())
            .ok_or("no session or window has that exact name");
    }
    if text.starts_with(['%', '@', '$']) {
        let (id, pane) = text.split_once('.').unwrap_or((text, ""));
        let keep: Vec<&TmuxObject> = match id.chars().next() {
            Some('%') if is_id(id, '%') && pane.is_empty() => {
                rows.iter().filter(|r| r.pane == id).collect()
            }
            Some('@') if is_id(id, '@') => rows.iter().filter(|r| r.window == id).collect(),
            Some('$') if is_id(id, '$') && pane.is_empty() => {
                rows.iter().filter(|r| r.session == id).collect()
            }
            _ => return Err("it is not a whole `%N`, `@N` or `$N` id"),
        };
        if keep.is_empty() {
            return Err("no pane, window or session has that id");
        }
        return pane_exists(pane, &keep);
    }
    if text.contains('.') {
        return Err("a `window.pane` with no session part reads a session tmux picks");
    }
    if is_index(text) {
        return Err("a bare number is an index in a session tmux picks");
    }
    if rows.iter().any(|r| r.window_name.starts_with(text)) {
        return Err("it also matches a window name, which tmux tries before a session");
    }
    if rows.iter().any(|r| r.session_name == text) {
        return Ok(());
    }
    Err(missing_session(text, rows))
}

/// Why a bare session name resolves to no session exactly.
fn missing_session(name: &str, rows: &[TmuxObject]) -> &'static str {
    if rows.iter().any(|r| r.session_name.starts_with(name)) {
        "it is only a prefix of a session name, and tmux would pick that session"
    } else {
        "no session has that name"
    }
}

/// The rows of the one session the session part of `session:…` names.
fn session_rows<'a>(
    part: &str,
    rows: &'a [TmuxObject],
    current: Option<&str>,
) -> Result<Vec<&'a TmuxObject>, &'static str> {
    let first = if part.is_empty() {
        let current = current.ok_or("its session is the caller's, which is unknown")?;
        rows.iter()
            .find(|r| r.pane == current)
            .ok_or("its session is the caller's, which is not on this server")?
    } else if part.starts_with(['%', '@', '$']) {
        if !(is_id(part, '%') || is_id(part, '@') || is_id(part, '$')) {
            return Err("it is not a whole `%N`, `@N` or `$N` id");
        }
        rows.iter()
            .find(|r| r.pane == part || r.window == part || r.session == part)
            .ok_or("no pane, window or session has that id")?
    } else if let Some(exact) = part.strip_prefix('=') {
        rows.iter()
            .find(|r| r.session_name == exact)
            .ok_or("no session has that exact name")?
    } else {
        rows.iter()
            .find(|r| r.session_name == part)
            .ok_or_else(|| missing_session(part, rows))?
    };
    Ok(rows.iter().filter(|r| r.session == first.session).collect())
}

/// The rows of the window the window part names inside one session.
fn window_rows<'a>(
    part: &str,
    rows: &[&'a TmuxObject],
) -> Result<Vec<&'a TmuxObject>, &'static str> {
    let pick = |keep: &dyn Fn(&TmuxObject) -> bool| -> Vec<&'a TmuxObject> {
        rows.iter().copied().filter(|r| keep(r)).collect()
    };
    let keep = if part.is_empty() {
        pick(&|r| r.window_active)
    } else if part.starts_with(['@', '%']) {
        if !(is_id(part, '@') || is_id(part, '%')) {
            return Err("it is not a whole `@N` or `%N` id");
        }
        let window = rows
            .iter()
            .find(|r| r.window == part || r.pane == part)
            .map(|r| r.window.clone());
        pick(&|r| Some(&r.window) == window.as_ref())
    } else if part.starts_with('$') {
        return Err("it is a special window token");
    } else if let Some(exact) = part.strip_prefix('=') {
        pick(&|r| r.window_name == exact)
    } else {
        let by_index = if is_index(part) {
            pick(&|r| r.window_index == part)
        } else {
            Vec::new()
        };
        if by_index.is_empty() {
            let by_name = pick(&|r| r.window_name == part);
            if by_name.is_empty() && rows.iter().any(|r| r.window_name.starts_with(part)) {
                return Err(
                    "it is only a prefix of a window name, and tmux would pick that window",
                );
            }
            by_name
        } else {
            by_index
        }
    };
    if keep.is_empty() {
        return Err("no window of that session matches it exactly");
    }
    Ok(keep)
}

/// Whether the pane part names a pane of `rows`; empty is the active pane.
fn pane_exists(part: &str, rows: &[&TmuxObject]) -> Result<(), &'static str> {
    let found = if part.is_empty() {
        true
    } else if is_id(part, '%') {
        rows.iter().any(|r| r.pane == part)
    } else if is_index(part) {
        rows.iter().any(|r| r.pane_index == part)
    } else {
        return Err("its pane part is not a pane index or `%N` id");
    };
    found
        .then_some(())
        .ok_or("no pane of that window matches it")
}

/// The pane list one `list-panes` run gives, as [`TmuxObject`]s.
///
/// What: no tmux binary, a spawn that failed or was refused, and a failure
/// other than "no server" are `Err`. No server running, or a socket nothing
/// listens on, is an empty list: no target exists there.
/// Test: `an_unlistable_server_denies_every_target`.
pub(super) fn classify_objects(listed: Listed<'_>) -> Result<Vec<TmuxObject>, String> {
    match listed {
        Listed::NotFound => Err("no tmux binary".into()),
        Listed::Failed(err) => Err(err),
        Listed::Ran {
            ok: false, stderr, ..
        } => {
            let err = stderr.trim();
            if ["no server running", "error connecting to"]
                .iter()
                .any(|m| err.contains(m))
            {
                return Ok(Vec::new());
            }
            Err(format!("tmux list-panes failed: {err}"))
        }
        Listed::Ran {
            ok: true, stdout, ..
        } => parse_objects(stdout),
    }
}

/// Parse [`OBJECT_FORMAT`] lines; a line with a tab in a name is `Err`.
fn parse_objects(text: &str) -> Result<Vec<TmuxObject>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [
                session,
                window,
                window_index,
                active,
                pane,
                pane_index,
                s_name,
                w_name,
            ] = f[..]
            else {
                return Err(format!("unreadable tmux pane line {line:?}"));
            };
            Ok(TmuxObject {
                session: session.to_owned(),
                window: window.to_owned(),
                window_index: window_index.to_owned(),
                window_active: active == "1",
                pane: pane.to_owned(),
                pane_index: pane_index.to_owned(),
                session_name: s_name.to_owned(),
                window_name: w_name.to_owned(),
            })
        })
        .collect()
}

/// The closing sentences every exact-target deny carries.
const REMEDY: &str = "Name an existing target exactly: `=session:window.pane`, a window or \
     pane index, or an id (`%N`, `@N`, `$N`). Create a session in an earlier call, not in the \
     same command. `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift this \
     floor.";

/// The deny for a target that does not resolve exactly.
fn refusal(hit: &Hit, target: &str, why: &str) -> String {
    format!(
        "Hard-floor deny (#9001): `tmux {}` targets `{target}`, which does not resolve exactly \
         to an existing tmux session, window or pane ({why}). tmux matches an unknown name by \
         prefix, so the command could reach another live session (owner ruling 2026-10-01). \
         {REMEDY}",
        hit.verb
    )
}

/// The deny for a target whose server the guard cannot list.
fn unlisted(hit: &Hit, target: &str, err: &str) -> String {
    format!(
        "Hard-floor deny (#9001): `tmux {}` targets `{target}`, and the guard cannot list that \
         tmux server to check the target exists ({err}). A target the guard cannot resolve is \
         refused (owner ruling 2026-10-01). {REMEDY}",
        hit.verb
    )
}

#[cfg(test)]
#[path = "tmux_exact_target_tests.rs"]
mod tests;
