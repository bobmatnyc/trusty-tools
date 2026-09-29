//! The writes a `Bash` command makes, as the trust-anchor rule reads them
//! (#8878).
//!
//! Why: owner ruling Q2 (#8878, 2026-09-29) — the trust-anchor floor reads the
//! write-target classifier ([`super::write_targets`]) PLUS the destinations of
//! `cp`, `mv`, `ln`, `install` and `sed -i`. Those destinations are argv
//! positions, not redirects, so the classifier alone never names them. A link
//! or rename also acts on its SOURCE: `ln` gives the anchor a second name a
//! later write goes through, and `mv` moves the anchor or a directory above
//! it. The main-checkout write boundary keeps its own narrower reading; this
//! module does not widen it.
//! What: [`anchor_writes`] walks every segment and nested body through
//! [`shell_segments_map`] and names, per segment, the classifier's targets
//! ([`AnchorWrite::File`]), a copy verb's destination ([`AnchorWrite::Into`]),
//! the sources of `ln`, `mv`, `cp -l`/`-s` and `install -l`
//! ([`AnchorWrite::Source`]), each `sed -i` file operand, `install -d`
//! directories, and a `cd`/`pushd`/`popd` ([`AnchorWrite::DirChange`]), after
//! which the caller cannot place a relative path. A verb segment that will not
//! tokenize, and a Q2 write run by `xargs` (which appends operands the guard
//! never sees), is an [`UnplaceableWrite`]; the one exception is `xargs cp -t
//! DIR` / `install -t DIR`, which is [`AnchorWrite::IntoUnnamed`].
//! Option values: a short option whose value differs between GNU and BSD
//! (`install -S`, `sed -l`) is read as taking none. A misread value only adds
//! an operand; consuming a word that is not a value would drop the destination.
//! Residual: writers outside Q2 — `dd of=`, `rsync`, `curl -o`, `sed`'s `w`
//! command, interpreters, `find -exec cp` — are not read here; the
//! script-file case is #8879.
//! Test: `pm_guard_trust_anchor_tests.rs` (`bash_write_shapes_onto_the_anchor_are_denied`,
//! `a_link_or_rename_source_on_an_anchor_is_denied`,
//! `bsd_install_s_and_sed_l_keep_their_operands`,
//! `a_q2_verb_through_xargs_is_denied`, `reads_and_other_writes_stay_allowed`).

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
        /// The path each source takes inside a directory `dest`.
        names: Vec<String>,
        /// Whether `dest` itself is written even when it is a directory.
        dest_too: bool,
    },
    /// A shell word a link or rename acts on (`ln`, `mv`, `cp -l`/`-s`,
    /// `install -l`): judged as if it were written.
    Source(String),
    /// A directory receiving entries whose names the command does not show
    /// (`xargs cp -t DIR`).
    IntoUnnamed(String),
    /// A `cd`, `pushd` or `popd`: relative paths no longer name the hook cwd.
    DirChange,
}

/// A verb segment the guard could not lex (#8878).
const UNLEXABLE_VERB: UnplaceableWrite =
    "a `cp`/`mv`/`ln`/`install`/`sed`/`cd` whose arguments do not lex";

/// A Q2 write run by `xargs` (#8878 finding 4).
const XARGS_WRITE: UnplaceableWrite = "a `cp`/`mv`/`ln`/`install`/`sed -i` run by `xargs`, \
     which appends operands the guard never sees";

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
    let Some((argv, at)) = program_argv(segment)? else {
        return Ok(out);
    };
    if is_xargs(&argv[at]) {
        out.extend(xargs_writes(&argv[at + 1..])?);
        return Ok(out);
    }
    let Some(verb) = verb_of(&argv[at]) else {
        return Ok(out);
    };
    let args = operands(&argv[at + 1..]);
    match verb {
        Verb::Cd => out.push(AnchorWrite::DirChange),
        Verb::Sed => out.extend(
            sed_edits(&args)
                .unwrap_or_default()
                .into_iter()
                .map(AnchorWrite::File),
        ),
        copy => out.extend(copy_writes(copy, &args)),
    }
    Ok(out)
}

/// The segment's words and the index of the program they run.
///
/// What: tokenizes with here-document bodies blanked and finds the program
/// past env assignments and wrappers ([`strip_wrapper_prefix`]) — or, when a
/// wrapper takes a flag, the first verb or `xargs` word. A segment that does
/// not tokenize is `Err` when its plain first word is a verb or `xargs`, and
/// `None` otherwise.
fn program_argv(segment: &str) -> Result<Option<(Vec<String>, usize)>, UnplaceableWrite> {
    let argv = match tokenize(&split_heredoc_bodies(segment).0) {
        Ok(argv) => argv,
        Err(_)
            if first_command_token(segment)
                .as_deref()
                .is_some_and(|word| verb_of(word).is_some() || is_xargs(word)) =>
        {
            return Err(UNLEXABLE_VERB);
        }
        Err(_) => return Ok(None),
    };
    let at = strip_wrapper_prefix(&argv).or_else(|| {
        argv.iter()
            .position(|word| verb_of(word).is_some() || is_xargs(word))
    });
    Ok(at.filter(|at| *at < argv.len()).map(|at| (argv, at)))
}

/// A program word's basename, a leading `\` dropped.
fn program_name(word: &str) -> &str {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word)
}

/// The [`Verb`] a program word names, GNU `g`-prefixed spellings included.
fn verb_of(word: &str) -> Option<Verb> {
    match program_name(word) {
        "cp" | "gcp" => Some(Verb::Cp),
        "mv" | "gmv" => Some(Verb::Mv),
        "ln" | "gln" => Some(Verb::Ln),
        "install" | "ginstall" => Some(Verb::Install),
        "sed" | "gsed" => Some(Verb::Sed),
        "cd" | "pushd" | "popd" => Some(Verb::Cd),
        _ => None,
    }
}

/// Whether a program word is `xargs`.
fn is_xargs(word: &str) -> bool {
    matches!(program_name(word), "xargs" | "gxargs")
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

/// The writes an `xargs` segment's Q2 verb makes (#8878 finding 4).
///
/// What: the first Q2 verb word after `xargs` is its program — a misread only
/// over-denies. `sed` without `-i` writes nothing. `cp`/`install` with
/// `-t DIR`, no replace string (`-I`, `-i`, `-J`, `--replace`), and neither
/// link nor `-d` mode receive unnamed entries in `DIR`. Every other Q2 verb is
/// [`XARGS_WRITE`]: its destination, files or sources arrive on stdin.
/// Test: `a_q2_verb_through_xargs_is_denied`.
fn xargs_writes(tail: &[String]) -> Result<Vec<AnchorWrite>, UnplaceableWrite> {
    let Some((at, verb)) = tail
        .iter()
        .enumerate()
        .find_map(|(at, word)| verb_of(word).filter(|v| *v != Verb::Cd).map(|v| (at, v)))
    else {
        return Ok(Vec::new());
    };
    let replace = tail[..at].iter().any(|word| {
        word.starts_with("--replace")
            || (!word.starts_with("--")
                && word
                    .strip_prefix('-')
                    .is_some_and(|flags| flags.contains(['I', 'i', 'J'])))
    });
    let args = operands(&tail[at + 1..]);
    match verb {
        Verb::Sed if sed_edits(&args).is_none() => Ok(Vec::new()),
        Verb::Cp | Verb::Install => {
            let parsed = CopyArgs::parse(verb, &args);
            match parsed.target_dir {
                Some(dir) if !replace && !parsed.link && !parsed.dirs => {
                    Ok(vec![AnchorWrite::IntoUnnamed(dir)])
                }
                _ => Err(XARGS_WRITE),
            }
        }
        _ => Err(XARGS_WRITE),
    }
}

/// A `cp`/`mv`/`ln`/`install` argv with its options read.
#[derive(Debug, Default)]
struct CopyArgs {
    /// The operands, in order.
    positional: Vec<String>,
    /// `-t DIR` / `--target-directory`.
    target_dir: Option<String>,
    /// `-T` / `--no-target-directory`.
    no_target: bool,
    /// `install -d`: every operand is a directory to create.
    dirs: bool,
    /// `cp -l`/`-s`, `install -l`: the destination becomes a link to the source.
    link: bool,
    /// The link is symbolic (`ln -s`, `cp -s`, `install -l`): its text is read
    /// from the link's own directory.
    symbolic: bool,
    /// `cp --parents`: a source keeps its whole relative path.
    parents: bool,
}

impl CopyArgs {
    /// Read `args` for `verb`.
    ///
    /// What: `-t`, and `install`'s `-m -o -g -B -f`, consume a value; every
    /// other short option takes none — BSD `install -S` is a flag, and a GNU
    /// `-S SUFFIX` then only adds an operand (#8878 finding 2).
    fn parse(verb: Verb, args: &[String]) -> Self {
        let mut out = Self::default();
        let mut options = true;
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
                    "--target-directory" => {
                        out.target_dir = value.or_else(|| words.next().cloned())
                    }
                    "--no-target-directory" => out.no_target = true,
                    "--directory" => out.dirs |= verb == Verb::Install,
                    "--link" => out.link |= verb == Verb::Cp,
                    "--symbolic-link" | "--symbolic" => {
                        out.link |= verb == Verb::Cp;
                        out.symbolic = true;
                    }
                    "--parents" => out.parents |= verb == Verb::Cp,
                    "--suffix" | "--mode" | "--owner" | "--group" if value.is_none() => {
                        words.next();
                    }
                    _ => {}
                }
            } else if options && word.starts_with('-') && word.len() > 1 {
                out.short_cluster(verb, word, &mut words);
            } else {
                out.positional.push(word.clone());
            }
        }
        out
    }

    /// Read one short-option cluster, consuming a value from `words`.
    fn short_cluster<'a>(
        &mut self,
        verb: Verb,
        word: &str,
        words: &mut impl Iterator<Item = &'a String>,
    ) {
        for (at, c) in word.char_indices().skip(1) {
            let takes_value =
                c == 't' || (verb == Verb::Install && matches!(c, 'm' | 'o' | 'g' | 'B' | 'f'));
            if takes_value {
                let rest = &word[at + c.len_utf8()..];
                let value = if rest.is_empty() {
                    words.next().cloned()
                } else {
                    Some(rest.to_string())
                };
                if c == 't' {
                    self.target_dir = value;
                }
                return;
            }
            match (verb, c) {
                (_, 'T') => self.no_target = true,
                (Verb::Install, 'd') => self.dirs = true,
                (Verb::Cp, 'l') => self.link = true,
                (Verb::Cp, 's') | (Verb::Install, 'l') => {
                    self.link = true;
                    self.symbolic = true;
                }
                (Verb::Ln, 's') => self.symbolic = true,
                _ => {}
            }
        }
    }
}

/// The writes a `cp`/`mv`/`ln`/`install` segment makes.
///
/// What: `-t DIR`/`--target-directory` makes every operand a source into
/// `DIR`; `install -d` writes every operand as a directory; `ln` with one
/// operand links into `.`; otherwise the last operand is the destination and
/// the rest are sources. `-T`/`--no-target-directory`, and a `cp` source
/// ending in `/` or `/.` (BSD copies its CONTENTS), write the destination
/// itself too. `mv`, `ln` and link-mode `cp`/`install` also emit each source
/// ([`source_writes`]). A missing destination writes nothing.
fn copy_writes(verb: Verb, args: &[String]) -> Vec<AnchorWrite> {
    let parsed = CopyArgs::parse(verb, args);
    if parsed.dirs {
        return parsed
            .positional
            .into_iter()
            .map(AnchorWrite::File)
            .collect();
    }
    let (dest, sources) = match (&parsed.target_dir, parsed.positional.split_last()) {
        (Some(dir), _) => (dir.clone(), parsed.positional.as_slice()),
        (None, Some((only, []))) if verb == Verb::Ln => {
            (".".to_string(), std::slice::from_ref(only))
        }
        (None, Some((dest, sources))) if !sources.is_empty() => (dest.clone(), sources),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    if matches!(verb, Verb::Mv | Verb::Ln) || parsed.link {
        for source in sources {
            out.extend(source_writes(source, &dest, parsed.symbolic));
        }
    }
    out.push(AnchorWrite::Into {
        names: sources
            .iter()
            .map(|s| source_name(s, parsed.parents))
            .collect(),
        dest_too: parsed.no_target
            || (verb == Verb::Cp && sources.iter().any(|s| copies_contents(s))),
        dest,
    });
    out
}

/// The paths a link or rename of `source` into `dest` acts on (#8878 finding 1).
///
/// What: the source as written. A relative symbolic link's text is resolved
/// from the link's own directory, which is `dest` when it is a directory and
/// its parent otherwise; both are emitted, since the guard does not know which.
fn source_writes(source: &str, dest: &str, symbolic: bool) -> Vec<AnchorWrite> {
    let mut out = vec![AnchorWrite::Source(source.to_string())];
    if symbolic && !source.starts_with(['/', '~', '$']) {
        let dest = dest.trim_end_matches('/');
        let parent = match dest.rsplit_once('/') {
            Some(("", _)) => "",
            Some((parent, _)) => parent,
            None => ".",
        };
        for dir in [dest, parent] {
            out.push(AnchorWrite::Source(format!("{dir}/{source}")));
        }
    }
    out
}

/// The path a source takes inside a destination directory: its basename, or
/// with `cp --parents` its whole relative path (#8878 finding 6).
fn source_name(source: &str, parents: bool) -> String {
    let trimmed = source.trim_end_matches('/');
    if parents {
        return trimmed.trim_start_matches('/').to_string();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// Whether a `cp` of `source` merges its contents into the destination.
fn copies_contents(source: &str) -> bool {
    source.ends_with('/') || source.ends_with("/.") || source == "."
}

/// The files a `sed` segment edits in place; `None` when it has no `-i`.
///
/// What: `-i`, `-i<suffix>`, a cluster holding `i` anywhere before `-e`/`-f`
/// (`-li`, `-ni`), or `--in-place` (and its unique prefixes) turns in-place
/// editing on. `-e` and `-f` consume their value; `-l` takes none, as in BSD
/// (#8878 finding 2). Without `-e`/`-f` the first operand is the script. A
/// bare `-i` followed by an empty word or one starting with `.` consumes it as
/// the BSD suffix — and, since GNU reads that word as a file, a non-empty one
/// is returned too. Every remaining operand is an edited file.
fn sed_edits(args: &[String]) -> Option<Vec<String>> {
    let (mut in_place, mut script_given, mut options) = (false, false, true);
    let (mut files, mut suffixes) = (Vec::new(), Vec::new());
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
                        let suffix = args
                            .get(i)
                            .filter(|next| next.is_empty() || next.starts_with('.'));
                        if let Some(suffix) = suffix.filter(|_| rest_empty) {
                            suffixes.extend((!suffix.is_empty()).then(|| suffix.clone()));
                            i += 1;
                        }
                        break;
                    }
                    'e' | 'f' => {
                        script_given = true;
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
        return None;
    }
    if !script_given && !files.is_empty() {
        files.remove(0);
    }
    files.extend(suffixes);
    Some(files)
}
