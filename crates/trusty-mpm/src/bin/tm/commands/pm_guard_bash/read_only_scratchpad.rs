//! `cp` and `rm` confined to the session scratchpad, for a read-only dispatch
//! (#8571).
//!
//! Why: a red-on-base check unpacks the base tree under `<scratchpad>/base-<sha>/`
//! and a `code-critic` then has to copy files around in it and delete it. The
//! #8439 allowlist refused `cp` and `rm` outright, so the run stalled. Owner
//! ruling 2026-09-28: allow scratchpad-only `ls`/`cp`/`rm` for read-only agents
//! (`ls` was already a plain reader).
//! What: [`scratchpad_write`] admits `cp` and `rm` only when every operand is a
//! literal absolute path strictly below THIS session's scratchpad root
//! (`…/<session_id>/scratchpad`, the id read off the hook payload), both as
//! spelled and after canonicalizing — so neither a symlink, a `..`, a
//! symlinked `scratchpad` component, nor another session's scratchpad can
//! carry the write out. With no session id the rule fails closed. An operand
//! with a `.git` component, in any ASCII case and as spelled or once resolved,
//! is refused, because a write into a clone's `.git/hooks` or `.git/config`
//! runs code at the next git command. Flags are a short allowlist; every long
//! option is refused, including `cp -t`/`--target-directory`, which moves the
//! destination off the operand list.
//!
//! Residuals, accepted: any `cp` into an existing directory writes through an
//! existing symlinked entry below it — only the operand itself is resolved,
//! not the tree `cp` walks — and a recursive copy carries any `.git` entry
//! already inside its source tree. And each path is canonicalized at hook
//! time, so a symlink swapped in between the hook and the command's run
//! (TOCTOU) is not seen.
//! Test: `read_only_allow_tests::scratchpad_cp_and_rm_are_allowed`,
//! `read_only_allow_tests::cp_and_rm_outside_the_scratchpad_are_refused`,
//! `read_only_allow_tests::a_symlinked_scratchpad_root_is_refused`,
//! `read_only_allow_tests::another_sessions_scratchpad_is_refused`,
//! `read_only_allow_tests::a_git_component_operand_is_refused`.

use std::ffi::OsStr;
use std::path::{Component, Path};

use super::read_only_programs::Arg;
use crate::commands::pm_guard_write_boundary::{canonical_existing_ancestor, scratchpad_root};

/// Short-flag characters each program may carry, alone or clustered.
fn allowed_flags(program: &str) -> &'static str {
    match program {
        "cp" => "rRpfvna",
        _ => "rRfvd",
    }
}

/// Judge a `cp` or `rm` argv tail for a read-only dispatch (#8571).
///
/// Why: see the module doc.
/// What: `Ok` when `session` is known, every flag is a short cluster from
/// [`allowed_flags`], and every operand (at least two for `cp`, one for `rm`)
/// carries no `.git` component ([`has_git_component`]) and passes
/// [`in_scratchpad`]; `Err` naming the first refusal otherwise.
/// Test: `read_only_allow_tests::scratchpad_cp_and_rm_are_allowed`,
/// `read_only_allow_tests::cp_and_rm_outside_the_scratchpad_are_refused`,
/// `read_only_allow_tests::a_git_component_operand_is_refused`.
pub(super) fn scratchpad_write(
    program: &str,
    rest: &[Arg],
    session: Option<&str>,
) -> Result<(), String> {
    // #8571 review: the ruling names the SESSION scratchpad; without the id
    // there is no way to tell this session's from another's, so refuse.
    let Some(session) = session else {
        return Err(format!(
            "`{program}` with no session id in the hook payload to bind the scratchpad to"
        ));
    };
    let mut operands = 0;
    let mut after_dashdash = false;
    for arg in rest {
        let Some(text) = arg.text() else {
            return Err(format!("a `{program}` operand held in a variable"));
        };
        if !after_dashdash && text == "--" {
            after_dashdash = true;
            continue;
        }
        if !after_dashdash && text.starts_with('-') {
            let cluster = &text[1..];
            let ok =
                !cluster.is_empty() && cluster.chars().all(|c| allowed_flags(program).contains(c));
            if !ok {
                return Err(format!("`{program} {text}`, a flag outside its short list"));
            }
            continue;
        }
        let path = Path::new(text);
        // #8571 review: `.git/hooks/*` and `.git/config` run code later.
        if has_git_component(path) {
            return Err(format!(
                "`{program}` naming {text}, which has a `.git` component"
            ));
        }
        if !in_scratchpad(path, session) {
            return Err(format!(
                "`{program}` naming {text}, which is not a literal absolute path inside the \
                 session scratchpad, or resolves through a `.git` component"
            ));
        }
        operands += 1;
    }
    let needed = if program == "cp" { 2 } else { 1 };
    if operands < needed {
        return Err(format!("`{program}` without its operands"));
    }
    Ok(())
}

/// Whether `path` lies strictly below `session`'s scratchpad root, lexically
/// and canonically (#8571).
///
/// What: `false` for a relative path, any `.`/`..` component, or the root
/// itself. The root, as spelled AND canonicalized, must be a scratchpad root
/// whose parent directory is named `session`. An existing operand (a symlink
/// included) must canonicalize strictly below the canonical root; a new one
/// is judged by its nearest existing ancestor, which may be the root. That
/// canonical path must carry no `.git` component either. Every unreadable
/// step is `false`, so the rule fails closed.
fn in_scratchpad(path: &Path, session: &str) -> bool {
    let plain = path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !plain {
        return false;
    }
    let Some(root) = scratchpad_root(path).filter(|root| root.as_path() != path) else {
        return false;
    };
    let Ok(real_root) = root.canonicalize() else {
        return false;
    };
    // #8571 review, HIGH: a symlinked `scratchpad` component (`/tmp/scratchpad
    // -> ~`) canonicalizes elsewhere; the resolved root must still be one.
    // #8571 review, MEDIUM: and it must be this session's.
    if !is_session_root(&root, session)
        || !is_session_root(&real_root, session)
        || scratchpad_root(&real_root).as_deref() != Some(real_root.as_path())
    {
        return false;
    }
    let real = match path.symlink_metadata() {
        Ok(_) => path
            .canonicalize()
            .ok()
            .filter(|real| *real != real_root && real.starts_with(&real_root)),
        Err(_) => canonical_existing_ancestor(path).filter(|real| real.starts_with(&real_root)),
    };
    // #8571 review round 2: an in-pad symlink (`cfg -> .git/config`, a link to
    // a `.git` directory) or a case variant reaches `.git` by another spelling.
    real.is_some_and(|real| !has_git_component(&real))
}

/// Whether any component of `path` is `.git`, compared ASCII-case-insensitively
/// (#8571): on case-insensitive APFS, `.GIT/config` is `.git/config`.
fn has_git_component(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
}

/// Whether `root`'s parent directory is named `session`.
fn is_session_root(root: &Path, session: &str) -> bool {
    root.parent().and_then(Path::file_name) == Some(OsStr::new(session))
}
