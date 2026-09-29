//! The Architect pane floor of `tm hook --pm-guard` (#8902).
//!
//! Why: owner ruling 2026-09-29 12:20Z. A session that is not the Architect
//! may not type into, replace or kill the Architect's tmux pane, window or
//! session: a PM that drives that pane acts as the process-bound Architect
//! (#8878 ruling A) without forging its identity.
//! What: [`evaluate_architect_pane`] takes the deny-set tmux commands
//! [`tmux_hits`] finds and, only while an Architect launch record is live,
//! resolves each target to the tmux sessions it can reach. A session holding a
//! pane the [`PaneProbe`] marks as the Architect's denies. The granularity is
//! the session: every pane of the Architect's session, and of a session a
//! window of it is linked into, counts as the Architect's. The Architect
//! exemption is `pm_guard_floor`'s. Verb decisions: `DENY_VERBS` in
//! `architect_pane_verbs.rs`.
//! FAIL-CLOSED: while an Architect is live, a target the guard cannot resolve
//! denies — a word the shell expands (`$VAR`, `$(…)`), a glob, a relative or
//! special token (`{marked}`, `~`, `!`, `+1`), a `#{…}` format, the caller's
//! current pane when it is unknown or the command touches a `TMUX` variable —
//! and so does an unreadable hit (see `Hit::opaque`) or a pane list tmux will
//! not give. The pane list is the server the hook process sees, read before
//! the command runs, so a relative `-S`, a `TMUX`/`TMUX_TMPDIR` change,
//! `sudo`/`doas`/`env -i`, a session renamed or created in the same command,
//! and a nested command with no target in a pane tmux picks all deny (#8902
//! review). A server with no Architect pane on it is never protected.
//! Residual: a `command-alias` that shadows a built-in name and was defined
//! before this call, a tmux config file, a script the command runs, and a
//! runner or interpreter that is not a shared wrapper — `watch`, `script -q
//! /dev/null`, `su -c`, `ssh`, `python3 -c`.
//! Test: `architect_pane_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_architect_pane_8902.rs`.

use super::architect_pane_parse::{Hit, Target, tmux_hits};

/// One pane as `tmux list-panes -a` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pane {
    /// `#{pane_id}`, `%N`.
    pub(crate) pane: String,
    /// `#{window_id}`, `@N`.
    pub(crate) window: String,
    /// `#{session_id}`, `$N`.
    pub(crate) session: String,
    /// `#{session_name}`.
    pub(crate) name: String,
    /// Whether the pane runs the Architect, or sits in its launch session.
    pub(crate) architect: bool,
    /// `#{pane_marked}`.
    pub(crate) marked: bool,
}

/// What the floor asks about the live Architect and the tmux servers.
pub(crate) trait PaneProbe {
    /// Whether an Architect launch record names a live process. `Err` when
    /// the records cannot be read, which the floor treats as live.
    fn architect_live(&self) -> Result<bool, String>;
    /// Every pane of the server the `-L`/`-S` words select, the default one
    /// when empty. No server running is `Ok(vec![])`.
    fn panes(&self, server: &[String]) -> Result<Vec<Pane>, String>;
    /// The caller's own pane, `$TMUX_PANE`.
    fn current_pane(&self) -> Option<String>;
}

/// One server's pane list, or why tmux would not give it.
type Listing = Result<Vec<Pane>, String>;

/// The rule name recorded with an Architect pane deny.
pub(crate) const ARCHITECT_PANE_RULE: &str = "architect-pane";

/// Classify a Bash command against the Architect pane floor: `Some(reason)`
/// denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command.
/// What: no deny-set tmux command, or no live Architect, is `None`. Otherwise
/// each hit's server is listed once, and the first hit that is unreadable,
/// whose server cannot be listed, or whose target reaches or may reach an
/// Architect session denies.
/// Test: `every_deny_verb_is_denied_in_each_target_form`,
/// `an_unresolvable_target_denies_while_an_architect_is_live`,
/// `no_live_architect_means_the_rule_does_not_apply`,
/// `a_non_architect_target_and_a_read_verb_pass`.
pub(crate) fn evaluate_architect_pane(command: &str, probe: &dyn PaneProbe) -> Option<String> {
    let hits = tmux_hits(command);
    if hits.is_empty() || probe.architect_live() == Ok(false) {
        return None;
    }
    // #8902: a command that sets or reads a TMUX variable may move "current".
    let current = (!command.contains("TMUX"))
        .then(|| probe.current_pane())
        .flatten();
    let mut listed: Vec<(&[String], Listing)> = Vec::new();
    for hit in &hits {
        if let Some(why) = hit.opaque {
            return Some(unresolved(hit, "", why));
        }
        if !listed.iter().any(|(s, _)| *s == hit.server.as_slice()) {
            listed.push((&hit.server, probe.panes(&hit.server)));
        }
        let panes = match listed.iter().find(|(s, _)| *s == hit.server.as_slice()) {
            Some((_, Ok(panes))) => panes,
            Some((_, Err(err))) => return Some(unresolved(hit, "", &format!("tmux: {err}"))),
            None => return Some(unresolved(hit, "", "no pane list")),
        };
        if !panes.iter().any(|p| p.architect) {
            continue;
        }
        for target in &hit.targets {
            match reaches_architect(target, panes, current.as_deref()) {
                Ok(false) => {}
                Ok(true) => return Some(reached(hit, target)),
                Err(why) => return Some(unresolved(hit, &shown(target), why)),
            }
        }
    }
    None
}

/// Whether `target` reaches a session holding an Architect pane; `Err` with
/// the reason when it cannot be resolved.
///
/// Test: `every_deny_verb_is_denied_in_each_target_form`,
/// `an_unresolvable_target_denies_while_an_architect_is_live`.
fn reaches_architect(
    target: &Target,
    panes: &[Pane],
    current: Option<&str>,
) -> Result<bool, &'static str> {
    let sessions = match target {
        Target::Server => return Ok(true),
        Target::Dynamic(_) => return Err("the shell expands it"),
        Target::Current => current_sessions(panes, current)?,
        Target::Marked => sessions_where(panes, |p| p.marked),
        Target::Literal(text) => literal_sessions(text, panes, current)?,
    };
    Ok(panes
        .iter()
        .any(|p| p.architect && sessions.contains(&p.session.as_str())))
}

/// The sessions holding the pane `current` names.
fn current_sessions<'a>(
    panes: &'a [Pane],
    current: Option<&str>,
) -> Result<Vec<&'a str>, &'static str> {
    let current = current.ok_or("the current pane is unknown")?;
    let found = sessions_where(panes, |p| p.pane == current);
    if found.is_empty() {
        return Err("the current pane is not on this server");
    }
    Ok(found)
}

/// The session ids of the panes `keep` accepts.
fn sessions_where(panes: &[Pane], keep: impl Fn(&Pane) -> bool) -> Vec<&str> {
    panes
        .iter()
        .filter(|p| keep(p))
        .map(|p| p.session.as_str())
        .collect()
}

/// The sessions a literal tmux target can reach, over-approximated.
///
/// What: `%N` and `@N` ids name the sessions holding that pane or window;
/// `$N` names a session. `session:rest` resolves the session part — empty is
/// the current session, `=name` an exact name, a bare name every session it
/// prefixes (tmux's exact-then-prefix match) — plus any `%N`/`@N` id in
/// `rest`. A word with no colon is also read as a window or pane of the
/// current session, as tmux falls back. Globs, special tokens and formats are
/// `Err`.
fn literal_sessions<'a>(
    text: &str,
    panes: &'a [Pane],
    current: Option<&str>,
) -> Result<Vec<&'a str>, &'static str> {
    if text.contains(['{', '}', '~', '!', '^', '*', '?', '[', ']', '#']) || text == "=" {
        return Err("it is a special token, glob or format");
    }
    if text.starts_with(['+', '-']) {
        return Err("it is a relative token");
    }
    let (session, rest) = match text.split_once(':') {
        Some((session, rest)) => (Some(session), rest),
        None => (None, text),
    };
    let mut out = match session {
        Some(part) => session_part(part, panes, current)?,
        None if text.starts_with(['%', '@', '$']) => id_sessions(text, panes)?,
        None => {
            // A window or pane of the current session, else a session name.
            let name = text.split('.').next().unwrap_or_default();
            let mut found = current_sessions(panes, current)?;
            found.extend(session_part(name, panes, current)?);
            found
        }
    };
    for id in rest.split('.').filter(|w| w.starts_with(['%', '@'])) {
        out.extend(id_sessions(id, panes)?);
    }
    Ok(out)
}

/// The sessions the session part of a `session:…` target names.
fn session_part<'a>(
    part: &str,
    panes: &'a [Pane],
    current: Option<&str>,
) -> Result<Vec<&'a str>, &'static str> {
    if part.is_empty() {
        return current_sessions(panes, current);
    }
    if part.starts_with(['%', '@', '$']) {
        return id_sessions(part, panes);
    }
    if let Some(exact) = part.strip_prefix('=') {
        return Ok(sessions_where(panes, |p| p.name == exact));
    }
    Ok(sessions_where(panes, |p| p.name.starts_with(part)))
}

/// The sessions a `%N`, `@N` or `$N` id names, a trailing `.pane` ignored.
fn id_sessions<'a>(id: &str, panes: &'a [Pane]) -> Result<Vec<&'a str>, &'static str> {
    let id = id.split('.').next().unwrap_or_default();
    if id.len() < 2 || !id[1..].bytes().all(|b| b.is_ascii_digit()) {
        return Err("it is not a whole `%N`, `@N` or `$N` id");
    }
    Ok(sessions_where(panes, |p| {
        p.pane == id || p.window == id || p.session == id
    }))
}

/// How a target reads in a deny reason.
fn shown(target: &Target) -> String {
    match target {
        Target::Literal(text) | Target::Dynamic(text) => text.clone(),
        Target::Current => "(the current pane)".into(),
        Target::Marked => "(the marked or current pane)".into(),
        Target::Server => "(the whole server)".into(),
    }
}

/// The closing sentences every Architect pane deny carries.
const REMEDY: &str = "Only the Architect's main thread may type into, replace or kill its \
     pane; PM-to-Architect messages do not go through the pane (owner ruling 2026-09-29). \
     `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift this floor. Aim \
     other tmux commands at an exact target (`=name`, `%N`, `@N`).";

/// The deny for a target that reaches the Architect.
fn reached(hit: &Hit, target: &Target) -> String {
    format!(
        "Hard-floor deny (#8902): `tmux {}` targets `{}`, which reaches the Architect's tmux \
         pane, and this session is not the Architect. {REMEDY}",
        hit.verb,
        shown(target)
    )
}

/// The deny for a hit or target the guard cannot resolve.
fn unresolved(hit: &Hit, target: &str, why: &str) -> String {
    let target = if target.is_empty() {
        String::new()
    } else {
        format!(" targets `{target}`, which")
    };
    format!(
        "Hard-floor deny (#8902): `{}`{target} cannot be resolved ({why}). While an \
         Architect runs, a tmux command the guard cannot resolve counts as aimed at its pane. \
         {REMEDY}",
        format!("tmux {}", hit.verb).trim_end()
    )
}

#[cfg(test)]
#[path = "architect_pane_tests.rs"]
mod tests;
