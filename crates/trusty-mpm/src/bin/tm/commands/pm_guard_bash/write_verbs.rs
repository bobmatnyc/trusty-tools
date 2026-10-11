//! The files a copy verb or an in-place edit writes, for the main-checkout
//! write boundary (#7228 Tier A).
//!
//! Why: `cp`, `mv`, `install`, `rsync`, `dd of=` and `sed -i` name their
//! destination as an ordinary argument, so [`super::write_targets`] read no
//! target from them and an agent standing in its worktree copied source back
//! into the shared main checkout by absolute path. Their argv grammar is
//! already parsed for the trust-anchor rule ([`super::anchor_verbs`]); this
//! module reuses that parse and keeps only the boundary's question — WHICH
//! FILE lands where. A source is a read and is never returned.
//! What: [`verb_write_targets`] finds the segment's program past env
//! assignments and wrappers and returns, per verb: the destination file, or —
//! when the destination is a directory (`-t`, a trailing `/`, `.`/`..`, or a
//! directory on disk per `is_dir`) — `<dest>/<name>` for each source; each
//! `install -d` directory; each `dd of=` file; each `sed -i` file operand. A
//! remote `rsync` destination writes no local file. A source copied by its
//! CONTENTS (`src/`, `src/.`) adds no name, since the files it holds are not
//! in the argv.
//!
//! Fail closed, matching an unlexable `tee`: a segment whose program is one of
//! these verbs and that does not tokenize is [`UnplaceableWrite`], and the
//! boundary denies it. Bash rejects the same unbalanced quoting, so the
//! refusal costs no command that would have run. A verb that names NO
//! destination (`cp a`, `dd if=x`) writes nothing and yields nothing; a
//! destination behind an unexpanded variable goes to the boundary's own #7838
//! rule, exactly as a redirect target does.
//!
//! Residual: operands an `xargs` appends or a `find -exec` substitutes are not
//! in the argv; a recursive copy of a directory names only the directory, not
//! the files under it; `ln`, `perl -i`, `curl -o`, `rsync --write-batch` and
//! `sed`'s `w` command are not read.
//! Test: `write_targets_read_*`, `write_targets_never_name_a_copy_source`,
//! `write_targets_refuse_a_write_verb_that_does_not_lex`,
//! `write_targets_expand_an_existing_directory_destination`.

use super::anchor_verbs::{
    CopyArgs, Verb, copies_contents, operands, program_name, sed_edits, source_name,
};
use super::bash_tokens::tokenize;
use super::heredoc::split_heredoc_bodies;
use super::write_targets::UnplaceableWrite;
use crate::commands::hook_rewrite::{first_command_token, strip_wrapper_prefix};

/// A write verb whose arguments do not lex (#7228).
const UNLEXABLE_WRITE_VERB: UnplaceableWrite =
    "a `cp`/`mv`/`install`/`rsync`/`dd`/`sed` whose arguments do not lex";

/// The verbs whose destination this module reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteVerb {
    Copy(Verb),
    Rsync,
    Dd,
    Sed,
}

/// The [`WriteVerb`] a program word names, GNU `g`-prefixed spellings included.
fn write_verb(word: &str) -> Option<WriteVerb> {
    match program_name(word) {
        "cp" | "gcp" => Some(WriteVerb::Copy(Verb::Cp)),
        "mv" | "gmv" => Some(WriteVerb::Copy(Verb::Mv)),
        "install" | "ginstall" => Some(WriteVerb::Copy(Verb::Install)),
        "rsync" => Some(WriteVerb::Rsync),
        "dd" | "gdd" => Some(WriteVerb::Dd),
        "sed" | "gsed" => Some(WriteVerb::Sed),
        _ => None,
    }
}

/// The files ONE segment writes through a copy verb, `dd of=` or `sed -i`.
///
/// Why/What: see the module doc. `is_dir` answers whether a bare destination
/// is an existing directory; the boundary resolves it against the hook cwd.
/// Test: `write_targets_read_cp_and_mv_destination`,
/// `write_targets_read_sed_i_operands`,
/// `write_targets_refuse_a_write_verb_that_does_not_lex`.
pub(super) fn verb_write_targets(
    segment: &str,
    is_dir: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, UnplaceableWrite> {
    let argv = match tokenize(&split_heredoc_bodies(segment).0) {
        Ok(argv) => argv,
        // #7228: fail closed, as an unlexable `tee` does.
        Err(_) if first_command_token(segment).is_some_and(|w| write_verb(&w).is_some()) => {
            return Err(UNLEXABLE_WRITE_VERB);
        }
        Err(_) => return Ok(Vec::new()),
    };
    let at = strip_wrapper_prefix(&argv)
        .or_else(|| argv.iter().position(|word| write_verb(word).is_some()));
    let Some((at, verb)) = at.and_then(|at| Some((at, write_verb(argv.get(at)?)?))) else {
        return Ok(Vec::new());
    };
    let args = operands(&argv[at + 1..]);
    Ok(match verb {
        WriteVerb::Copy(verb) => copy_targets(verb, &args, is_dir),
        WriteVerb::Rsync => rsync_targets(&args, is_dir),
        WriteVerb::Dd => args
            .iter()
            .filter_map(|word| word.strip_prefix("of="))
            .map(str::to_string)
            .collect(),
        WriteVerb::Sed => sed_edits(&args).unwrap_or_default(),
    })
}

/// The files a `cp`/`mv`/`install` writes: see [`landing_paths`].
fn copy_targets(verb: Verb, args: &[String], is_dir: &dyn Fn(&str) -> bool) -> Vec<String> {
    let parsed = CopyArgs::parse(verb, args);
    if parsed.dirs {
        return parsed.positional;
    }
    let (dest, sources) = match (&parsed.target_dir, parsed.positional.split_last()) {
        (Some(dir), _) => (dir.as_str(), parsed.positional.as_slice()),
        (None, Some((dest, sources))) if !sources.is_empty() => (dest.as_str(), sources),
        _ => return Vec::new(),
    };
    let into_dir = parsed.target_dir.is_some();
    let names = names_of(sources, parsed.parents);
    landing_paths(dest, &names, into_dir, parsed.no_target, is_dir)
}

/// The value-taking `rsync` options, which must not be read as operands.
const RSYNC_VALUE_OPTIONS: &[&str] = &[
    "--exclude",
    "--include",
    "--filter",
    "--exclude-from",
    "--include-from",
    "--files-from",
    "--rsh",
    "--rsync-path",
    "--log-file",
    "--log-file-format",
    "--out-format",
    "--backup-dir",
    "--suffix",
    "--temp-dir",
    "--compare-dest",
    "--copy-dest",
    "--link-dest",
    "--partial-dir",
    "--chmod",
    "--chown",
    "--usermap",
    "--groupmap",
    "--max-size",
    "--min-size",
    "--max-delete",
    "--timeout",
    "--contimeout",
    "--port",
    "--password-file",
    "--bwlimit",
    "--block-size",
    "--modify-window",
    "--compress-level",
    "--skip-compress",
    "--address",
    "--sockopts",
    "--info",
    "--debug",
    "--iconv",
    "--write-batch",
    "--only-write-batch",
    "--read-batch",
    "--protocol",
    "--remote-option",
];

/// The files an `rsync` writes locally.
///
/// What: options before `--` are skipped, with the value of a short `-e -f
/// -B -T -M` or a [`RSYNC_VALUE_OPTIONS`] long option consumed; the last
/// operand is the destination when there are two or more. A remote
/// destination (`host:path`, `rsync://`) writes nothing here.
fn rsync_targets(args: &[String], is_dir: &dyn Fn(&str) -> bool) -> Vec<String> {
    let (mut positional, mut options, mut words) = (Vec::new(), true, args.iter());
    while let Some(word) = words.next() {
        if options && word == "--" {
            options = false;
        } else if options && word.starts_with("--") {
            if !word.contains('=') && RSYNC_VALUE_OPTIONS.contains(&word.as_str()) {
                words.next();
            }
        } else if options && word.starts_with('-') && word.len() > 1 {
            let flags = &word[1..];
            if let Some(at) = flags.find(['e', 'f', 'B', 'T', 'M'])
                && at + 1 == flags.len()
            {
                words.next();
            }
        } else {
            positional.push(word.clone());
        }
    }
    let Some((dest, sources)) = positional.split_last() else {
        return Vec::new();
    };
    if sources.is_empty() || is_remote(dest) {
        return Vec::new();
    }
    landing_paths(dest, &names_of(sources, false), false, false, is_dir)
}

/// Whether an rsync operand names another host: `rsync://`, or a `:` before
/// any `/`.
fn is_remote(operand: &str) -> bool {
    operand.starts_with("rsync://") || operand.split('/').next().is_some_and(|s| s.contains(':'))
}

/// The name each source takes inside a destination directory; a source
/// copied by its contents names nothing.
fn names_of(sources: &[String], parents: bool) -> Vec<String> {
    sources
        .iter()
        .filter(|source| !copies_contents(source))
        .map(|source| source_name(source, parents))
        .collect()
}

/// Where a copy into `dest` lands.
///
/// What: `<dest>/<name>` for each name when `dest` is a directory — `into_dir`
/// (`-t`), a trailing `/`, `.`/`..`, or `is_dir(dest)` — unless `no_target`
/// (`-T`) says `dest` is the file itself; otherwise `dest`.
fn landing_paths(
    dest: &str,
    names: &[String],
    into_dir: bool,
    no_target: bool,
    is_dir: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let spelled_dir = dest.ends_with('/')
        || matches!(dest, "." | "..")
        || dest.ends_with("/.")
        || dest.ends_with("/..");
    if no_target || !(into_dir || spelled_dir || is_dir(dest)) {
        return vec![dest.to_string()];
    }
    let base = dest.trim_end_matches('/');
    names.iter().map(|name| format!("{base}/{name}")).collect()
}
