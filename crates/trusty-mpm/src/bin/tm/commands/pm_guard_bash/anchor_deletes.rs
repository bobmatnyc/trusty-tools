//! The deletes a `Bash` command makes, as the trust-anchor rule reads them
//! (#8878 Q2 delete ruling).
//!
//! Why: Architect ruling on PR #8919 (#8878) — a PM that deletes
//! `architect-launch/<pid>.architect` empties the lineage, so the Architect
//! pane floor turns off. Deletes join the protected verb set for every trust
//! anchor; only the Architect's main thread may delete there.
//! What: [`delete_verb_of`] names `rm`, `rmdir`, `unlink`, `trash`, `shred`,
//! `truncate` and `find`; [`delete_writes`] turns one such segment's operands
//! into [`AnchorWrite::Delete`]s with their [`Reach`]. `rmdir -p` also
//! removes each parent the operand spells ([`Reach::Parent`]). `find` deletes
//! under its start points ([`Reach::Tree`]) when it carries `-delete` or runs
//! a delete verb, `mv` or a shell through `-exec`, `-execdir`, `-ok` or
//! `-okdir`; with no start point it searches `.`. #8878 H2: the command each
//! such action runs is read as a command of its own ([`anchor_writes`]).
//! Residual: see `pm_guard_trust_anchor.rs`.
//! Test: `pm_guard_trust_anchor_delete_tests.rs`
//! (`each_delete_verb_on_each_anchor_is_denied`,
//! `find_deletes_under_its_start_points`, `ordinary_deletes_stay_allowed`).

use super::anchor_verbs::{AnchorWrite, Reach, anchor_writes};
use super::write_targets::UnplaceableWrite;
use crate::commands::hook_rewrite::strip_wrapper_prefix;

/// A `find -exec`-family action whose command cannot be re-spelled (#8878 H2).
const UNLEXABLE_FIND_ACTION: UnplaceableWrite =
    "a `find -exec`/`-execdir`/`-ok`/`-okdir` command that does not lex";

/// A verb that removes or empties what it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeleteVerb {
    /// `rm`, `unlink`, `trash`: every operand is removed.
    Remove,
    /// `rmdir`: every operand, and with `-p` each parent it spells.
    Rmdir,
    /// `shred`: every operand is overwritten, and with `-u` removed.
    Shred,
    /// `truncate`: every operand is emptied or resized.
    Truncate,
    /// `find`: everything under its start points, when it deletes.
    Find,
}

/// The [`DeleteVerb`] a program word names, GNU `g`-prefixed spellings included.
pub(super) fn delete_verb_of(word: &str) -> Option<DeleteVerb> {
    // #8878 round 2: zsh runs `=rm` as `rm`.
    let word = word.strip_prefix(['\\', '=']).unwrap_or(word);
    match word.rsplit('/').next().unwrap_or(word) {
        "rm" | "grm" | "unlink" | "gunlink" | "trash" => Some(DeleteVerb::Remove),
        "rmdir" | "grmdir" => Some(DeleteVerb::Rmdir),
        "shred" | "gshred" => Some(DeleteVerb::Shred),
        "truncate" | "gtruncate" => Some(DeleteVerb::Truncate),
        "find" | "gfind" => Some(DeleteVerb::Find),
        _ => None,
    }
}

/// The anchor writes of one delete-verb segment; `args` has its redirects removed.
///
/// What: see the module doc. Option words are dropped (`--` ends them);
/// `shred -n/-s` and `truncate -s/-r` consume their value. `Err` when a
/// `find` action's command cannot be read.
/// Test: `each_delete_verb_on_each_anchor_is_denied`,
/// `a_find_exec_action_is_read_as_a_command`.
pub(super) fn delete_writes(
    verb: DeleteVerb,
    args: &[String],
) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    let entry = |word| AnchorWrite::Delete(word, Reach::Entry);
    Ok(match verb {
        DeleteVerb::Find => return find_writes(args),
        // #8878 split: `truncate` is a delete-set verb, judged as one.
        DeleteVerb::Truncate => positional(args, &['s', 'r'])
            .into_iter()
            .map(entry)
            .collect(),
        DeleteVerb::Shred => positional(args, &['n', 's'])
            .into_iter()
            .map(entry)
            .collect(),
        DeleteVerb::Remove => positional(args, &[]).into_iter().map(entry).collect(),
        DeleteVerb::Rmdir => {
            let parents = args
                .iter()
                .take_while(|w| *w != "--")
                .any(|w| w == "--parents" || (!w.starts_with("--") && short_has(w, 'p')));
            let mut out = Vec::new();
            for operand in positional(args, &[]) {
                // #8878: `rmdir -p a/b` removes `a/b`, then `a`.
                let mut path = operand.trim_end_matches('/').to_string();
                out.push(entry(operand));
                if !parents {
                    continue;
                }
                while let Some((parent, _)) = path.rsplit_once('/') {
                    if parent.is_empty() {
                        break;
                    }
                    path = parent.to_string();
                    out.push(AnchorWrite::Delete(path.clone(), Reach::Parent));
                }
            }
            out
        }
    })
}

/// Whether a short-option cluster (`-rf`) holds `flag`.
fn short_has(word: &str, flag: char) -> bool {
    word.len() > 1 && word.starts_with('-') && word[1..].contains(flag)
}

/// The operands of `args`: option words dropped until `--`, and a short
/// option in `valued` consuming its value (attached, or the next word).
fn positional(args: &[String], valued: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let (mut options, mut words) = (true, args.iter());
    while let Some(word) = words.next() {
        if options && word == "--" {
            options = false;
        } else if options && word.starts_with("--") {
            let valued_long = ["--size", "--reference", "--iterations", "--random-source"];
            if !word.contains('=') && valued_long.contains(&word.as_str()) {
                words.next();
            }
        } else if options && word.len() > 1 && word.starts_with('-') {
            let at = word[1..].find(|c| valued.contains(&c));
            if at.is_some_and(|at| at + 2 == word.len()) {
                words.next();
            }
        } else {
            out.push(word.clone());
        }
    }
    out
}

/// The anchor writes of a `find` segment: each action's command's writes,
/// and each start point, as a [`Reach::Tree`] delete, when the expression
/// deletes.
///
/// What: start points are the words before the first expression word (one
/// starting with `-`, or `(`, `)`, `!`, `,`), after the leading `-H -L -P
/// -E -X -d -s -x` flags; BSD `-f PATH` names one too. None means `.`.
/// Test: `find_deletes_under_its_start_points`,
/// `a_find_exec_action_is_read_as_a_command`.
fn find_writes(args: &[String]) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    let mut out = Vec::new();
    if find_deletes(args) {
        out.extend(find_starts(args));
    }
    out.extend(find_action_writes(args)?);
    Ok(out)
}

/// The writes of the command each `-exec`-family action runs (#8878 H2).
///
/// What: the action's words up to its `;` or `+`, each `{}` dropped, are
/// re-spelled as one command and read by [`anchor_writes`], so a verb, a
/// wrapper and a shell's `-c` string are read as at the top level. An
/// `-execdir`/`-okdir` runs in each match's directory: a [`AnchorWrite::DirChange`].
/// A command that cannot be re-spelled is `Err`.
fn find_action_writes(args: &[String]) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    let mut out = Vec::new();
    for (at, word) in args.iter().enumerate() {
        if !matches!(word.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            continue;
        }
        let tail = &args[at + 1..];
        let end = tail
            .iter()
            .position(|w| w == ";" || w == "+")
            .unwrap_or(tail.len());
        let argv: Vec<&str> = tail[..end]
            .iter()
            .map(String::as_str)
            .filter(|w| *w != "{}")
            .collect();
        if word.ends_with("dir") {
            out.push(AnchorWrite::DirChange);
        }
        if argv.is_empty() {
            continue;
        }
        // #8878 H2: an action that will not re-spell fails closed.
        let command = shlex::try_join(argv).map_err(|_| UNLEXABLE_FIND_ACTION)?;
        out.extend(anchor_writes(&command)?);
    }
    Ok(out)
}

/// A deleting `find`'s start points, as [`Reach::Tree`] deletes.
fn find_starts(args: &[String]) -> Vec<AnchorWrite> {
    let tree = |word| AnchorWrite::Delete(word, Reach::Tree);
    let mut starts = Vec::new();
    let mut words = args.iter().peekable();
    while let Some(word) = words.next_if(|w| w.starts_with('-') && w.len() > 1) {
        match word.as_str() {
            "-f" => starts.extend(words.next().cloned()),
            "--" => break,
            flags if flags[1..].chars().all(|c| "HLPEXdsx".contains(c)) => {}
            // #8878: the first expression word; no start point precedes it.
            _ => return vec![tree(".".to_string())],
        }
    }
    starts.extend(
        words
            .take_while(|w| !(w.starts_with('-') || ["(", ")", "!", ","].contains(&w.as_str())))
            .cloned(),
    );
    if starts.is_empty() {
        starts.push(".".to_string());
    }
    starts.into_iter().map(tree).collect()
}

/// Whether a `find` expression deletes: `-delete`, or an `-exec`-family
/// action whose program is a delete verb, `mv`, or a shell.
pub(super) fn find_deletes(args: &[String]) -> bool {
    args.iter()
        .enumerate()
        .any(|(at, word)| match word.as_str() {
            "-delete" => true,
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let tail = &args[at + 1..];
                let end = tail
                    .iter()
                    .position(|w| w == ";" || w == "+")
                    .unwrap_or(tail.len());
                let tail = &tail[..end];
                let program = strip_wrapper_prefix(tail).and_then(|i| tail.get(i));
                match program {
                    Some(program) => runs_a_delete(program),
                    // A wrapper the resolver cannot measure: any delete word counts.
                    None => tail.iter().any(|w| runs_a_delete(w)),
                }
            }
            _ => false,
        })
}

/// Whether a program word run by `find -exec` could delete what it is handed.
fn runs_a_delete(word: &str) -> bool {
    let name = word.strip_prefix('\\').unwrap_or(word);
    let name = name.rsplit('/').next().unwrap_or(name);
    delete_verb_of(name).is_some_and(|v| v != DeleteVerb::Find)
        || matches!(
            name,
            "mv" | "gmv" | "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish"
        )
}
