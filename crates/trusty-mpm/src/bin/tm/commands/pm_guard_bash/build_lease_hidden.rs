//! Heavy builds the lease rewrite cannot reach, refused instead (#11886).
//!
//! Why: a `cargo test` that runs outside `tm build-lease` in the shared
//! `CARGO_TARGET_DIR` can execute a test binary built from another worktree's
//! sources and report green (#11886). The rewrite inserts the lease before a
//! program word it can see. These shapes run a build through text the shell
//! reads later, or through a word only the shell can expand: a here-document,
//! `<<<` string or pipe fed to a shell, `eval`, `script -c`, and a program or
//! subcommand named by an expansion. `tm build-lease` cannot wrap them in
//! place, so the hook refuses them and names the leased form.
//! What: [`hidden_build_refusal`] returns the refusal for the first such
//! shape, or `None`. [`unparseable_build_refusal`] is the fail-closed arm: a
//! command whose quoting or here-document the scan cannot read, and whose raw
//! words still name a heavy build, is refused, never passed through.
//! Test: `wrapper_operands_runners_and_coproc_are_leased_in_place` and its
//! siblings in `build_lease_hidden_tests.rs`; the `*_11886` cases in
//! `tests/tm_hook_pm_guard_build_lease.rs`.

use super::super::build_lease_program::{
    LEADING_KEYWORDS, Word, heavy_program_offset, is_wrapper, subcommand, words,
};
use super::super::heredoc::HeredocBodies;
use super::super::shell_lex::DASH_C_SHELLS;
use super::super::split_shell_segments_raw;
use super::{LeaseRewrite, rewrite_for_lease};
use crate::commands::hook_rewrite::is_env_assignment;
use std::borrow::Cow;

/// The `(program, optional subcommand)` heavy-build table.
type Heavy = [(String, Option<String>)];

/// The refusal for a heavy build `command` runs where no lease can go.
///
/// What: checks every here-document a shell reads, then every simple command
/// outside a here-document body: `eval`, a shell reading stdin from a `<<<`
/// string or a pipe, `script -c`, and an expansion in the program or
/// subcommand position. A text a shell runs counts when
/// [`rewrite_for_lease`] would lease or refuse it on its own.
/// Test: `a_shell_reading_stdin_is_refused_only_when_it_builds`,
/// `eval_and_script_strings_are_refused_only_when_they_build`,
/// `expansions_are_refused_only_in_a_heavy_position`.
pub(super) fn hidden_build_refusal(
    command: &str,
    heavy: &Heavy,
    bodies: &HeredocBodies,
) -> Option<String> {
    let shell_body = bodies.bodies().iter().any(|body| {
        let (op_start, op_end) = body.operator_line;
        operator_line_feeds_a_shell(&command[op_start..op_end])
            && runs_heavy(&command[body.span.0..body.span.1], heavy)
    });
    if shell_body {
        return Some(refusal("a here-document a shell runs"));
    }
    let origin = command.as_ptr() as usize;
    let mut upstream: Option<(usize, &str)> = None;
    for seg in split_shell_segments_raw(command) {
        let at = seg.as_ptr() as usize - origin;
        if bodies.contains(at) {
            upstream = None;
            continue;
        }
        let piped = upstream
            .filter(|(end, _)| is_pipe(&command[*end..at]))
            .map(|(_, text)| text);
        if let Some(kind) = segment_refusal(seg, piped, heavy) {
            return Some(refusal(kind));
        }
        upstream = Some((at + seg.len(), seg));
    }
    None
}

/// The fail-closed refusal for a command the scan cannot read (#11886).
///
/// Why: the Fail-Open Check — a parse failure must never become a silent,
/// unleased build. The shell may still run what the scan could not split.
/// What: `Some` when the here-document scan is unscannable or the quotes do
/// not balance outside here-document bodies, AND a raw word sequence names a
/// heavy build.
/// Test: `an_unparseable_build_is_refused_and_unparseable_data_is_not`.
pub(super) fn unparseable_build_refusal(
    command: &str,
    heavy: &Heavy,
    bodies: &HeredocBodies,
) -> Option<String> {
    let unreadable = bodies.is_unscannable() || !balanced_outside_bodies(command, bodies);
    (unreadable && mentions_heavy(command, heavy))
        .then(|| refusal("quoting or a here-document the hook cannot parse"))
}

/// The shape name for one simple command that hides a heavy build.
fn segment_refusal(seg: &str, piped: Option<&str>, heavy: &Heavy) -> Option<&'static str> {
    if heavy_program_offset(seg, heavy).is_some() {
        return None;
    }
    let seg_words = words(seg);
    let idx = program_index(&seg_words)?;
    let word = &seg_words[idx];
    let rest = &seg_words[idx + 1..];
    let base = basename(&word.text);
    let joined = |ws: &[Word]| {
        ws.iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    if base == "eval" && runs_heavy(&joined(rest), heavy) {
        return Some("`eval`");
    }
    // `script` is a runner, so the program walk steps past it to its `-c` value.
    let script = seg_words[..=idx]
        .iter()
        .position(|w| basename(&w.text) == "script");
    if script.is_some_and(|at| {
        option_value(&seg_words[at + 1..], &["-c", "--command"])
            .is_some_and(|v| runs_heavy(v, heavy))
    }) {
        return Some("`script -c`");
    }
    if DASH_C_SHELLS.contains(&base) && reads_stdin(rest) {
        let mut fed: Vec<String> = herestrings(rest);
        if let Some(text) = piped {
            fed.extend(words(text).iter().map(|w| w.text.replace("\\n", "\n")));
        }
        if fed.iter().any(|text| runs_heavy(text, heavy)) {
            return Some("a shell reading its commands from a pipe or a `<<<` string");
        }
    }
    let raw = &seg[word.start..word.end];
    if is_expansion(raw) {
        let text = expansion_text(seg, word);
        // #11886 r1: the words after the whole expansion, not inside a backtick.
        let end = word.start + text.len();
        let after = seg_words
            .iter()
            .position(|w| w.start >= end)
            .unwrap_or(seg_words.len());
        if expanded_heavy_program(text, heavy).is_some_and(|program| {
            runs_a_heavy_verb(program, subcommand(&seg_words[after..]), heavy)
        }) {
            return Some("a program named by an expansion");
        }
    }
    let sub = subcommand(rest);
    let has_sub = heavy.iter().any(|(p, s)| p == base && s.is_some());
    if has_sub && sub.is_some_and(|s| is_expansion(&s)) {
        return Some("a subcommand named by an expansion");
    }
    None
}

/// Whether a here-document opened on `line` is read by a shell: the simple
/// command holding its `<<` is a shell, or the line pipes on into one.
///
/// Why: `heredoc_line::line_runs_a_shell` reads any `$(…)` word as a program,
/// so `git commit -m "$(cat <<'EOF'` made every commit message that names a
/// build a refused command. This reads the command the body goes to.
fn operator_line_feeds_a_shell(line: &str) -> bool {
    let shell_reads_stdin = |text: &str| {
        let words = words(text);
        program_index(&words).is_some_and(|i| {
            DASH_C_SHELLS.contains(&basename(&words[i].text)) && reads_stdin(&words[i + 1..])
        })
    };
    line.match_indices("<<").any(|(pos, _)| {
        let bytes = line.as_bytes();
        if bytes.get(pos + 2) == Some(&b'<') || (pos > 0 && bytes[pos - 1] == b'<') {
            return false;
        }
        let prefix = &line[..pos];
        let start = ["$(", "`", "|", ";", "&", "(", "{"]
            .iter()
            .filter_map(|sep| prefix.rfind(sep).map(|at| at + sep.len()))
            .max()
            .unwrap_or(0);
        shell_reads_stdin(&format!("{} <<X", &prefix[start..]))
            || line[pos..].split('|').skip(1).any(shell_reads_stdin)
    })
}

/// Whether `text`, run as shell source, runs a heavy build.
fn runs_heavy(text: &str, heavy: &Heavy) -> bool {
    !matches!(
        rewrite_for_lease(text, heavy, "tm build-lease --"),
        LeaseRewrite::None
    )
}

/// The index of the program word: past keywords, `(`, assignments, wrappers
/// and their dash options.
fn program_index(words: &[Word]) -> Option<usize> {
    let mut after_wrapper = false;
    words.iter().position(|w| {
        let bare = w.text.trim_start_matches('(');
        let skip = LEADING_KEYWORDS.contains(&w.text.as_str())
            || bare.is_empty()
            || is_env_assignment(bare)
            || is_wrapper(basename(bare))
            || (after_wrapper && (bare.starts_with('-') || bare.parse::<f64>().is_ok()));
        after_wrapper |= is_wrapper(basename(bare));
        !skip
    })
}

/// A word's program basename: `(`, a leading `\` and the directory dropped.
fn basename(word: &str) -> &str {
    let bare = word.trim_start_matches('(');
    let bare = bare.strip_prefix('\\').unwrap_or(bare);
    bare.rsplit('/').next().unwrap_or(bare)
}

/// Whether a shell with these arguments reads its commands from stdin: no
/// `-c` and no script operand, or `-s`. A redirect and its target are skipped.
fn reads_stdin(rest: &[Word]) -> bool {
    let mut saw_s = false;
    let mut iter = rest.iter().map(|w| w.text.as_str());
    while let Some(arg) = iter.next() {
        if is_redirect(arg) {
            if arg
                .trim_start_matches(|c: char| c.is_ascii_digit() || "<>&-".contains(c))
                .is_empty()
            {
                iter.next();
            }
            continue;
        }
        if arg == "-" {
            return true;
        }
        if let Some(cluster) = arg.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            if cluster.contains('c') {
                return false;
            }
            saw_s |= cluster.contains('s');
            continue;
        }
        if arg.starts_with("--") || arg.starts_with('+') {
            continue;
        }
        return saw_s;
    }
    true
}

/// Whether a word is a redirect operator, with or without its target.
fn is_redirect(arg: &str) -> bool {
    let tail = arg.trim_start_matches(|c: char| c.is_ascii_digit());
    tail.starts_with(['<', '>']) || tail.starts_with("&>")
}

/// The `<<<` strings among a command's arguments.
fn herestrings(rest: &[Word]) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = rest.iter().map(|w| w.text.as_str());
    while let Some(arg) = iter.next() {
        match arg.strip_prefix("<<<") {
            Some("") => out.extend(iter.next().map(str::to_string)),
            Some(text) => out.push(text.to_string()),
            None => {}
        }
    }
    out
}

/// The value of the first of `names` among `rest`, as `-c V` or `--command=V`.
fn option_value<'a>(rest: &'a [Word], names: &[&str]) -> Option<&'a str> {
    rest.iter().enumerate().find_map(|(i, w)| {
        let text = w.text.as_str();
        if names.contains(&text) {
            return rest.get(i + 1).map(|v| v.text.as_str());
        }
        names
            .iter()
            .find_map(|n| text.strip_prefix(n).and_then(|t| t.strip_prefix('=')))
    })
}

/// Whether the shell expands this word before it runs: `$…`, `"$…"`, `` `…` ``.
fn is_expansion(word: &str) -> bool {
    word.trim_start_matches(['(', '"']).starts_with(['$', '`'])
}

/// The text of the expansion that starts at `word`: through the closing
/// backtick when it opens one, since [`words`] splits a backtick body.
fn expansion_text<'a>(seg: &'a str, word: &Word) -> &'a str {
    let from = &seg[word.start..];
    let lead = from.len() - from.trim_start_matches(['(', '"']).len();
    if from[lead..].starts_with('`') {
        let close = from[lead + 1..]
            .find('`')
            .map_or(from.len(), |k| lead + 2 + k);
        return &from[..close];
    }
    &seg[word.start..word.end]
}

/// The heavy program an expanded program word names: an identifier equal to
/// one, case aside (`$CARGO`, `${CARGO:-cargo}`, `$(which cargo)`).
///
/// #11886 r1: a path whose last component is literal (`$CARGO_TARGET_DIR/debug/tm`,
/// `"$CARGO_HOME/bin/cargo"`) is the classifier's to read by that basename.
fn expanded_heavy_program<'h>(text: &str, heavy: &'h Heavy) -> Option<&'h str> {
    let bare = text.trim_matches('"');
    if let Some((_, tail)) = bare.rsplit_once('/')
        && !tail.contains(['$', '`', ')', '}'])
    {
        return None;
    }
    // #11886 r2: `_` splits, so `$CARGO_BIN` names cargo; the path rule
    // above still passes `$CARGO_TARGET_DIR/debug/tm`.
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .find_map(|tok| {
            heavy
                .iter()
                .find(|(p, _)| p.eq_ignore_ascii_case(tok))
                .map(|(p, _)| p.as_str())
        })
}

/// Whether `program` with subcommand `sub` is a heavy build, or may be one
/// because `sub` is itself an expansion (#11886 r1: `$CARGO fmt` is not).
fn runs_a_heavy_verb(program: &str, sub: Option<String>, heavy: &Heavy) -> bool {
    heavy
        .iter()
        .filter(|(p, _)| p == program)
        .any(|(_, s)| match s {
            None => true,
            Some(verb) => sub
                .as_deref()
                .is_some_and(|sub| sub == verb || is_expansion(sub)),
        })
}

/// Whether a `watch`, `parallel` or `flock` segment runs a heavy build as one
/// quoted operand (#11886 r1): `watch -n 5 'cargo check'`, `flock F -c '…'`.
/// The rewrite then leases the whole runner.
/// Test: `a_runners_quoted_command_is_leased_whole`.
pub(super) fn runner_operand_hides(seg: &str, heavy: &Heavy) -> bool {
    let seg_words = words(seg);
    let upto = program_index(&seg_words).map_or(seg_words.len(), |i| i + 1);
    let Some(at) = seg_words[..upto]
        .iter()
        .position(|w| matches!(basename(&w.text), "watch" | "parallel" | "flock"))
    else {
        return false;
    };
    seg_words[at + 1..]
        .iter()
        .any(|w| w.text.contains(char::is_whitespace) && runs_heavy(&w.text, heavy))
}

/// Whether the text between two segments joins them with a pipe.
fn is_pipe(between: &str) -> bool {
    let between = between.trim();
    between.starts_with('|') && !between.starts_with("||")
}

/// Whether the raw words of `command`, quoting ignored, name a heavy build.
fn mentions_heavy(command: &str, heavy: &Heavy) -> bool {
    let tokens: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || "'\"`;|&(){}<>\\".contains(c))
        .filter(|t| !t.is_empty())
        .collect();
    (0..tokens.len()).any(|i| {
        heavy.iter().any(|(p, _)| p == basename(tokens[i]))
            && heavy_program_offset(&tokens[i..tokens.len().min(i + 8)].join(" "), heavy).is_some()
    })
}

/// Whether `command`'s quotes close, here-document bodies and `#` comments
/// skipped (#11886 r1: a comment's apostrophe is no open quote).
fn balanced_outside_bodies(command: &str, bodies: &HeredocBodies) -> bool {
    comment_walk(command, bodies).1
}

/// `command` with every `#` comment outside quotes and here-document bodies
/// blanked to spaces, so every byte offset survives.
///
/// Why (#11886 r2): `QuoteScan`, and the segment splitter on it, read a
/// comment's apostrophe as an open quote and cut a later quoted string apart,
/// so `# it's` + `bash -c 'cargo fmt; cargo test'` leased nothing.
/// Test: `comments_and_underscored_names_stay_fail_closed`.
pub(super) fn blank_comments<'a>(command: &'a str, bodies: &HeredocBodies) -> Cow<'a, str> {
    let (comments, _) = comment_walk(command, bodies);
    if comments.is_empty() {
        return Cow::Borrowed(command);
    }
    let mut bytes = command.as_bytes().to_vec();
    for (start, end) in comments {
        bytes[start..end].fill(b' ');
    }
    Cow::Owned(String::from_utf8_lossy(&bytes).into_owned())
}

/// The `#` comment spans of `command`, and whether its quotes close, both
/// read outside here-document bodies.
fn comment_walk(command: &str, bodies: &HeredocBodies) -> (Vec<(usize, usize)>, bool) {
    let bytes = command.as_bytes();
    let mut comments = Vec::new();
    let mut quote: Option<u8> = None;
    let mut prev = b'\n';
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if bodies.contains(i) {
            i += 1;
            prev = b;
            continue;
        }
        match quote {
            Some(q) if b == q => quote = None,
            Some(b'"') if b == b'\\' => i += 1,
            Some(_) => {}
            None if b == b'\\' => i += 1,
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'#' && (prev.is_ascii_whitespace() || b";|&(".contains(&prev)) => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                comments.push((start, i));
                continue;
            }
            None => {}
        }
        prev = b;
        i += 1;
    }
    (comments, quote.is_none())
}

/// The #11886 refusal reason for a heavy build run through `shape`.
fn refusal(shape: &str) -> String {
    format!(
        "Build lease (#11886): this command runs a heavy build (cargo build/test/clippy/check/\
         doc/install/run/bench, or `builders.heavy_build_commands`) through {shape}, where the \
         hook cannot insert `tm build-lease`. An unleased build in the shared target directory \
         can run a test binary built from another worktree's sources. Run the build as its own \
         command with the program and subcommand spelled out, or wrap it yourself: \
         `tm build-lease -- cargo test …`."
    )
}

#[cfg(test)]
#[path = "build_lease_hidden_tests.rs"]
mod tests;
