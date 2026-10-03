//! `cp` and `rm` confined to the session scratchpad, for a read-only dispatch
//! (#8571).
//!
//! Why: a red-on-base check unpacks the base tree under `<scratchpad>/base-<sha>/`
//! and a `code-critic` then has to copy files around in it and delete it. The
//! #8439 allowlist refused `cp` and `rm` outright, so the run stalled. Owner
//! ruling 2026-09-28: allow scratchpad-only `ls`/`cp`/`rm` for read-only agents
//! (`ls` was already a plain reader).
//! What: [`scratchpad_write`] admits `cp` and `rm` only when every operand is a
//! literal absolute path strictly below a session scratchpad root, both as
//! spelled and after canonicalizing its nearest existing ancestor — so neither
//! a symlink nor a `..` can carry the write out. Flags are a short allowlist;
//! every long option is refused, including `cp -t`/`--target-directory`, which
//! moves the destination off the operand list.
//! Test: `read_only_allow_tests::scratchpad_cp_and_rm_are_allowed`,
//! `read_only_allow_tests::cp_and_rm_outside_the_scratchpad_are_refused`.

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
/// What: `Ok` when every flag is a short cluster from [`allowed_flags`] and
/// every operand (at least two for `cp`, one for `rm`) passes
/// [`in_scratchpad`]; `Err` naming the first refusal otherwise.
/// Test: `read_only_allow_tests::scratchpad_cp_and_rm_are_allowed`,
/// `read_only_allow_tests::cp_and_rm_outside_the_scratchpad_are_refused`.
pub(super) fn scratchpad_write(program: &str, rest: &[Arg]) -> Result<(), String> {
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
        if !in_scratchpad(Path::new(text)) {
            return Err(format!(
                "`{program}` naming {text}, which is not a literal absolute path inside the \
                 session scratchpad"
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

/// Whether `path` lies strictly below a session scratchpad root, lexically and
/// canonically (#8571).
///
/// What: `false` for a relative path, any `.`/`..` component, the scratchpad
/// root itself, or a path whose nearest existing ancestor canonicalizes
/// outside the canonical form of that same root. Every unreadable step is
/// `false`, so the rule fails closed.
fn in_scratchpad(path: &Path) -> bool {
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
    let (Ok(real_root), Some(real)) = (root.canonicalize(), canonical_existing_ancestor(path))
    else {
        return false;
    };
    real.starts_with(&real_root)
}
