//! The writes a `Bash` command makes, as the trust-anchor rule reads them
//! (#8878).
//!
//! Why: owner ruling Q2 (#8878, 2026-09-29) — the trust-anchor floor reads the
//! write-target classifier ([`super::write_targets`]) PLUS the destinations of
//! `cp`, `mv`, `ln`, `install` and `sed -i`. Those destinations are argv
//! positions, not redirects, so the classifier alone never names them. The
//! main-checkout write boundary keeps its own narrower reading; this module
//! does not widen it.
//! What: [`anchor_writes`] walks every segment and nested body through
//! [`shell_segments_map`] and names, per segment, the classifier's targets
//! ([`AnchorWrite::File`]), a copy verb's destination ([`AnchorWrite::Into`]),
//! each `sed -i` file operand, `install -d` directories, and a
//! `cd`/`pushd`/`popd` ([`AnchorWrite::DirChange`]), after which the caller
//! cannot place a relative path. A verb segment that will not tokenize is an
//! [`UnplaceableWrite`].
//! Residual: writers outside Q2 — `dd of=`, `rsync`, `curl -o`, `sed`'s `w`
//! command, interpreters, `find -exec cp` — are not read here; the
//! script-file case is #8879.
//! Test: `pm_guard_trust_anchor_tests.rs` (`bash_write_shapes_onto_the_anchor_are_denied`,
//! `reads_and_other_writes_stay_allowed`).

use super::bash_tokens::{RedirectRole, redirect_role, tokenize};
use super::heredoc::split_heredoc_bodies;
use super::write_targets::{
    UnplaceableWrite, input_redirect, segment_write_targets, shell_segments_map,
};
use crate::commands::hook_rewrite::{first_command_token, strip_wrapper_prefix};

/// One write a command makes, for the trust-anchor rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnchorWrite {
    /// A path written as a shell word: `~`, `$HOME`, globs and quotes apply.
    File(String),
    /// A path named outside a shell (an edit tool's `file_path`): literal.
    Literal(String),
    /// A copy-shaped write. `dest` is written as a file, or — when it is a
    /// directory — receives one entry per `names` element; `dest_too` says
    /// the directory itself is written as well (`-T`, a `cp` of `src/`).
    Into {
        /// The destination operand.
        dest: String,
        /// The basename each source takes inside a directory `dest`.
        names: Vec<String>,
        /// Whether `dest` itself is written even when it is a directory.
        dest_too: bool,
    },
    /// A `cd`, `pushd` or `popd`: relative paths no longer name the hook cwd.
    DirChange,
}

/// A verb segment the guard could not lex (#8878).
const UNLEXABLE_VERB: UnplaceableWrite =
    "a `cp`/`mv`/`ln`/`install`/`sed`/`cd` whose arguments do not lex";

/// The verbs this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Cp,
    Mv,
    Ln,
    Install,
    Sed,
    Cd,
}

/// Every write `command` makes that the trust-anchor rule judges.
///
/// Why/What: see the module doc. `Ok(empty)` when nothing is written; `Err`
/// when a write-bearing construct cannot be parsed, which the caller denies.
/// Test: `bash_write_shapes_onto_the_anchor_are_denied`,
/// `an_unplaceable_write_is_denied`.
pub(crate) fn anchor_writes(command: &str) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    shell_segments_map(command, &segment_anchor_writes)
}

/// [`anchor_writes`] for ONE segment.
fn segment_anchor_writes(segment: &str) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    let mut out: Vec<AnchorWrite> = segment_write_targets(segment)?
        .into_iter()
        .map(AnchorWrite::File)
        .collect();
    let Some((verb, args)) = verb_argv(segment)? else {
        return Ok(out);
    };
    match verb {
        Verb::Cd => out.push(AnchorWrite::DirChange),
        Verb::Sed => out.extend(sed_in_place_files(&args).into_iter().map(AnchorWrite::File)),
        copy => out.extend(copy_writes(copy, &args)),
    }
    Ok(out)
}

/// The verb this segment runs and its operands, redirect words removed.
///
/// What: tokenizes with here-document bodies blanked, finds the program past
/// env assignments and wrappers ([`strip_wrapper_prefix`]) — or, when a
/// wrapper takes a flag, the first verb word — and returns `None` when it is
/// not a [`Verb`]. A segment that does not tokenize is `Err` when its plain
/// first word is a verb, and `None` otherwise.
fn verb_argv(segment: &str) -> Result<Option<(Verb, Vec<String>)>, UnplaceableWrite> {
    let argv = match tokenize(&split_heredoc_bodies(segment).0) {
        Ok(argv) => argv,
        Err(_)
            if first_command_token(segment)
                .as_deref()
                .and_then(verb_of)
                .is_some() =>
        {
            return Err(UNLEXABLE_VERB);
        }
        Err(_) => return Ok(None),
    };
    let program = strip_wrapper_prefix(&argv)
        .or_else(|| argv.iter().position(|word| verb_of(word).is_some()));
    let Some((at, verb)) = program.and_then(|at| Some((at, verb_of(argv.get(at)?)?))) else {
        return Ok(None);
    };
    Ok(Some((verb, operands(&argv[at + 1..]))))
}

/// The [`Verb`] a program word names: its basename, a leading `\` dropped,
/// GNU `g`-prefixed spellings included.
fn verb_of(word: &str) -> Option<Verb> {
    let word = word.strip_prefix('\\').unwrap_or(word);
    match word.rsplit('/').next().unwrap_or(word) {
        "cp" | "gcp" => Some(Verb::Cp),
        "mv" | "gmv" => Some(Verb::Mv),
        "ln" | "gln" => Some(Verb::Ln),
        "install" | "ginstall" => Some(Verb::Install),
        "sed" | "gsed" => Some(Verb::Sed),
        "cd" | "pushd" | "popd" => Some(Verb::Cd),
        _ => None,
    }
}

/// `words` without redirections: the redirect scan reads output targets, and
/// an input redirect's source is never a destination.
fn operands(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for word in words {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        match redirect_role(word) {
            RedirectRole::TargetFollows => skip_next = true,
            RedirectRole::Target(_) | RedirectRole::FileDescriptor => {}
            RedirectRole::None => match input_redirect(word) {
                Some(target_follows) => skip_next = target_follows,
                None => out.push(word.clone()),
            },
        }
    }
    out
}

/// Whether short option `c` of `verb` takes a value (GNU and BSD spellings).
fn takes_value(verb: Verb, c: char) -> bool {
    matches!(c, 't' | 'S') || (verb == Verb::Install && matches!(c, 'm' | 'o' | 'g' | 'B' | 'f'))
}

/// The write a `cp`/`mv`/`ln`/`install` segment makes.
///
/// What: `-t DIR`/`--target-directory` makes every operand a source into
/// `DIR`; `install -d` writes every operand as a directory; `ln` with one
/// operand links into `.`; otherwise the last operand is the destination and
/// the rest are sources. `-T`/`--no-target-directory`, and a `cp` source
/// ending in `/` or `/.` (BSD copies its CONTENTS), write the destination
/// itself too. A missing destination writes nothing.
fn copy_writes(verb: Verb, args: &[String]) -> Vec<AnchorWrite> {
    let mut positional = Vec::new();
    let mut target_dir: Option<String> = None;
    let (mut no_target, mut dirs, mut options) = (false, false, true);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if options && word == "--" {
            options = false;
        } else if options && word.starts_with("--") {
            let (name, value) = match word.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (word.as_str(), None),
            };
            match name {
                "--target-directory" => target_dir = value.or_else(|| words.next().cloned()),
                "--no-target-directory" => no_target = true,
                "--directory" => dirs = verb == Verb::Install,
                "--suffix" | "--mode" | "--owner" | "--group" if value.is_none() => {
                    words.next();
                }
                _ => {}
            }
        } else if options && word.starts_with('-') && word.len() > 1 {
            for (at, c) in word.char_indices().skip(1) {
                if takes_value(verb, c) {
                    let rest = &word[at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        words.next().cloned()
                    } else {
                        Some(rest.to_string())
                    };
                    if c == 't' {
                        target_dir = value;
                    }
                    break;
                }
                no_target |= c == 'T';
                dirs |= c == 'd' && verb == Verb::Install;
            }
        } else {
            positional.push(word.clone());
        }
    }
    if dirs {
        return positional.into_iter().map(AnchorWrite::File).collect();
    }
    let into = |dest: String, sources: &[String]| AnchorWrite::Into {
        dest,
        names: sources.iter().map(|s| source_name(s)).collect(),
        dest_too: no_target || (verb == Verb::Cp && sources.iter().any(|s| copies_contents(s))),
    };
    if let Some(dir) = target_dir {
        return vec![into(dir, &positional)];
    }
    match positional.split_last() {
        Some((only, [])) if verb == Verb::Ln => {
            vec![into(".".to_string(), std::slice::from_ref(only))]
        }
        Some((dest, sources)) if !sources.is_empty() => vec![into(dest.clone(), sources)],
        _ => Vec::new(),
    }
}

/// The name a source takes inside a destination directory.
fn source_name(source: &str) -> String {
    let trimmed = source.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// Whether a `cp` of `source` merges its contents into the destination.
fn copies_contents(source: &str) -> bool {
    source.ends_with('/') || source.ends_with("/.") || source == "."
}

/// The files a `sed` segment edits in place; empty when it has no `-i`.
///
/// What: `-i`, `-i<suffix>`, a cluster holding `i`, or `--in-place` (and its
/// unique prefixes) turns in-place editing on. A bare `-i` followed by an
/// empty word or one starting with `.` consumes it as the BSD suffix. `-e`,
/// `-f` and `-l` consume their value; without `-e`/`-f` the first operand is
/// the script. Every remaining operand is an edited file.
fn sed_in_place_files(args: &[String]) -> Vec<String> {
    let (mut in_place, mut script_given, mut options) = (false, false, true);
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let word = &args[i];
        i += 1;
        if options && word == "--" {
            options = false;
        } else if options && word.starts_with("--") {
            let name = word.split('=').next().unwrap_or(word);
            let prefix_of = |long: &str, min: usize| name.len() >= min && long.starts_with(name);
            let has_value = word.contains('=');
            if prefix_of("--in-place", 3) {
                in_place = true;
            } else if prefix_of("--expression", 3) || prefix_of("--file", 4) {
                script_given = true;
                i += usize::from(!has_value);
            } else if prefix_of("--line-length", 3) {
                i += usize::from(!has_value);
            }
        } else if options && word.starts_with('-') && word.len() > 1 {
            for (at, c) in word.char_indices().skip(1) {
                let rest_empty = word.len() == at + c.len_utf8();
                match c {
                    'i' => {
                        in_place = true;
                        let bsd_suffix = args
                            .get(i)
                            .is_some_and(|next| next.is_empty() || next.starts_with('.'));
                        i += usize::from(rest_empty && bsd_suffix);
                        break;
                    }
                    'e' | 'f' | 'l' => {
                        script_given |= c != 'l';
                        i += usize::from(rest_empty);
                        break;
                    }
                    _ => {}
                }
            }
        } else {
            files.push(word.clone());
        }
    }
    if !in_place {
        return Vec::new();
    }
    if !script_given && !files.is_empty() {
        files.remove(0);
    }
    files
}
