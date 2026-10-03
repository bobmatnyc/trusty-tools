//! Every file a `Bash` command would write, for the main-checkout write
//! boundary (#7399, #8468 option B, #8730).
//!
//! Why (#8730): the boundary read a segment's write only from a `>` redirect
//! or a git write option, off segment text cut on composition operators. Three
//! shapes wrote past it: `tee <path>` names its file as an ordinary argument;
//! a write inside a subshell `( … )` or a command substitution `$(…)` reached
//! the redirect scan with the closing `)` glued to its target (`src/lib.rs)`),
//! which no extension check reads as source; and a `$(…)` inside double quotes
//! was never scanned, because the quote map calls its bytes quoted.
//! What: [`shell_write_targets`] lifts every live substitution and subshell
//! body out of the command ([`lift_bodies`]), judges each body as a command of
//! its own, and reads the remaining outer text segment by segment
//! ([`segment_write_targets`]) for redirects, a git write option and `tee`
//! operands. A body it cannot delimit, a `tee` it cannot tokenize, nesting past
//! [`MAX_SUBSTITUTION_DEPTH`] and an unlexable `sh -c` wrapper are each an
//! [`UnplaceableWrite`]: the boundary cannot place that write, so it refuses
//! it (fail closed). Bash rejects every one of those shapes as a syntax error
//! except the depth cap, so the refusal costs no command that would have run.
//! Residual: a `$(…)` in an UNQUOTED-delimiter here-document body runs in bash
//! but is skipped here with the rest of the body (#5356's framing); a
//! `case … in x)` arm inside a substitution closes it early. The write
//! boundary's module doc lists the writers no rule here detects.
//! Test: `write_targets_*` below; end to end in `pm_guard_write_boundary`'s
//! `denies_a_tee_write_in_a_main_checkout` and its siblings.

use super::bash_tokens::{RedirectRole, redirect_role, tokenize};
use super::heredoc::{HeredocBodies, split_heredoc_bodies};
use super::{
    MAX_SUBSTITUTION_DEPTH, scan_file_write_redirects, shell_lex, split_shell_segments_raw,
};
use crate::commands::hook_rewrite::{first_command_token, strip_wrapper_prefix};

/// What about a write kept the guard from placing its target (#8730).
///
/// The write boundary quotes it in the deny, so it names the parse problem.
pub(crate) type UnplaceableWrite = &'static str;

const UNBALANCED_BODY: UnplaceableWrite =
    "a subshell `(`, command substitution `$(`/backtick or process substitution that never closes";
const TOO_DEEP: UnplaceableWrite = "substitutions or wrappers nested deeper than the guard follows";
const UNLEXABLE_TEE: UnplaceableWrite = "a `tee` whose arguments do not lex";
const UNLEXABLE_WRAPPER: UnplaceableWrite =
    "an `sh -c`/`bash -c`/`env -S`/`xargs` wrapper whose inner command does not lex";

/// Every file `command` would write, one entry per positively named write.
///
/// Why (#7399, #8468 option B, #8730): the boundary asks WHERE a write lands,
/// so it needs each file a command names as a write, from every segment and
/// every nested body — not only the first. A redirect, a git write option and
/// a `tee` operand each NAME the file created; the sed/awk trailing token stays
/// out because a read occupies that same position. No target is ever resolved
/// against a directory a `cd` moved to (#8468; following `cd` is #8704).
/// What: [`targets_at`] from depth 0. `Ok(empty)` when nothing names a write.
/// `Err` when a write-bearing construct cannot be parsed; the caller denies.
/// Test: `shell_write_targets_reads_redirects_and_git_output`,
/// `shell_write_targets_collects_every_segments_write`,
/// `write_targets_read_tee_substitution_and_subshell_writes`,
/// `write_targets_refuse_what_they_cannot_place`.
pub(crate) fn shell_write_targets(command: &str) -> Result<Vec<String>, UnplaceableWrite> {
    shell_segments_map(command, &segment_write_targets)
}

/// `per_segment` over every segment of `command`, at every nesting depth.
///
/// Why (#8878): the trust-anchor rule reads more write shapes per segment than
/// the write boundary does, and must reach the same nested bodies.
/// What: [`targets_at`] from depth 0 with `per_segment`; its results in order.
/// Test: `anchor_verbs_reach_a_nested_body` (`pm_guard_trust_anchor_tests.rs`).
pub(crate) fn shell_segments_map<T>(
    command: &str,
    per_segment: &dyn Fn(&str) -> Result<Vec<T>, UnplaceableWrite>,
) -> Result<Vec<T>, UnplaceableWrite> {
    targets_at(command, 0, per_segment)
}

/// [`shell_segments_map`] at one nesting depth: the outer text's segments,
/// each wrapper's inner command, then each lifted body.
fn targets_at<T>(
    command: &str,
    depth: usize,
    per_segment: &dyn Fn(&str) -> Result<Vec<T>, UnplaceableWrite>,
) -> Result<Vec<T>, UnplaceableWrite> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        return Err(TOO_DEEP);
    }
    let (outer, bodies) = lift_bodies(command)?;
    let mut out = Vec::new();
    for raw in split_shell_segments_raw(&outer) {
        let segment = raw.trim();
        out.extend(per_segment(segment)?);
        match shell_lex::wrapped_command(segment) {
            shell_lex::WrappedCommand::Inner(inner) => {
                out.extend(targets_at(&inner, depth + 1, per_segment)?);
            }
            shell_lex::WrappedCommand::Unlexable => return Err(UNLEXABLE_WRAPPER),
            shell_lex::WrappedCommand::None => {}
        }
    }
    for body in bodies {
        out.extend(targets_at(&body, depth + 1, per_segment)?);
    }
    Ok(out)
}

/// The files ONE command segment names as writes.
///
/// Why: [`super::extract_shell_edit_target`] and [`shell_write_targets`] ask
/// the same question of a segment and must not drift apart.
/// What: every redirect target ([`scan_file_write_redirects`]) that names a
/// token and is not a `>(…)` process substitution, the file a git write option
/// names ([`shell_lex::git_file_write_target`]), and every `tee` operand
/// ([`tee_targets`]). A git write with no readable path yields nothing here;
/// `classify_bash_segment` still denies it for the PM.
/// Test: `shell_write_targets_reads_redirects_and_git_output`,
/// `write_targets_reads_every_redirect_of_a_segment`.
pub(super) fn segment_write_targets(segment: &str) -> Result<Vec<String>, UnplaceableWrite> {
    if segment.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = scan_file_write_redirects(segment)
        .into_iter()
        // #8730: `>(…)` is a process substitution, whose body is lifted.
        .filter(|target| !target.is_empty() && !target.starts_with('('))
        .collect();
    if first_command_token(segment).as_deref() == Some("git")
        && let Some(target) = shell_lex::git_file_write_target(segment)
        && !target.is_empty()
    {
        out.push(target);
    }
    out.extend(tee_targets(segment)?);
    Ok(out)
}

/// The files a `tee` segment writes (#8730).
///
/// Why: `tee` writes every operand, and none of them is a redirect.
/// What: tokenizes the segment with its here-document bodies blanked
/// ([`split_heredoc_bodies`]), so body prose never reads as an operand; finds
/// the program past env assignments and
/// wrappers ([`strip_wrapper_prefix`]), or — when a wrapper takes a flag, as
/// in `sudo -u x tee f` — the first `tee` word. Every later word is a target
/// except options before `--` and redirect words, which the redirect scan
/// reads. A segment that will not tokenize is an `Err` when its plain
/// whitespace split still names `tee` as the program, and empty otherwise, so
/// here-document prose with an apostrophe is not refused.
/// Test: `write_targets_read_tee_substitution_and_subshell_writes`,
/// `write_targets_refuse_what_they_cannot_place`.
fn tee_targets(segment: &str) -> Result<Vec<String>, UnplaceableWrite> {
    let argv = match tokenize(&split_heredoc_bodies(segment).0) {
        Ok(argv) => argv,
        Err(_) if first_command_token(segment).as_deref() == Some("tee") => {
            return Err(UNLEXABLE_TEE);
        }
        Err(_) => return Ok(Vec::new()),
    };
    let is_tee = |word: &String| basename(word) == "tee";
    let program = strip_wrapper_prefix(&argv).or_else(|| argv.iter().position(is_tee));
    let Some(program) = program.filter(|&at| argv.get(at).is_some_and(is_tee)) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut options = true;
    let mut skip_next = false;
    for word in &argv[program + 1..] {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if options && word == "--" {
            options = false;
            continue;
        }
        if options && word.starts_with('-') && word.len() > 1 {
            continue;
        }
        match redirect_role(word) {
            RedirectRole::TargetFollows => skip_next = true,
            RedirectRole::Target(_) | RedirectRole::FileDescriptor => {}
            RedirectRole::None => match input_redirect(word) {
                Some(target_follows) => skip_next = target_follows,
                // #8730: never keep the `)` that closes an enclosing body.
                None => out.push(before_unmatched_close(word).to_string()),
            },
        }
    }
    out.retain(|target| !target.is_empty());
    Ok(out)
}

/// A word's program name: past any `/`, without a leading `\` or the `$(`,
/// `(` or backtick of an unlifted body it opens (#8730: `$(tee`).
fn basename(word: &str) -> &str {
    let word = word.trim_start_matches(['$', '(', '`']);
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word)
}

/// `word` up to its first `)` that closes no `(` opened inside it — the same
/// paren-aware read `scan_file_write_redirects` gives a redirect target.
fn before_unmatched_close(word: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in word.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return &word[..at],
            ')' => depth -= 1,
            _ => {}
        }
    }
    word
}

/// `Some(true)` for a bare input redirect (`<`, `<<`, `<<<`, `<<-`) whose
/// source is the next word, `Some(false)` for one carrying it (`<in`,
/// `<<EOF`, `<()`), `None` for any other word.
// #8878: shared with `anchor_verbs`, which drops redirect words from operands.
pub(super) fn input_redirect(word: &str) -> Option<bool> {
    let after = word
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .strip_prefix('<')?;
    Some(after.trim_start_matches(['<', '-']).is_empty())
}

/// Split `command` into its outer text and the bodies of its live subshells
/// and substitutions (#8730).
///
/// What: [`lift`] with quote tracking. When the command ends inside an open
/// quote — a syntax error, or here-document prose with an apostrophe the
/// here-document scan could not frame — the quote map is untrustworthy, so it
/// lifts again with every opener live and an unclosed one left in place:
/// that pass detects conservatively and never refuses on prose.
fn lift_bodies(command: &str) -> Result<(String, Vec<String>), UnplaceableWrite> {
    match lift(command, true) {
        Lifted::Done(outer, bodies) => Ok((outer, bodies)),
        Lifted::Unbalanced => Err(UNBALANCED_BODY),
        Lifted::OpenQuote => match lift(command, false) {
            Lifted::Done(outer, bodies) => Ok((outer, bodies)),
            Lifted::Unbalanced | Lifted::OpenQuote => Err(UNBALANCED_BODY),
        },
    }
}

/// [`lift`]'s three outcomes.
enum Lifted {
    Done(String, Vec<String>),
    Unbalanced,
    OpenQuote,
}

/// Quote state while scanning.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// One left-to-right pass: copy `command` to the outer text, replacing each
/// live body with nothing so its delimiters (`$()`, `()`, ``` `` ```) remain.
///
/// What: lifts a here-document body its operator line hands to a shell as one
/// body, and skips a data body, `\`-escaped bytes and a `#` comment to the
/// end of its line. With `quotes`,
/// tracks `'`/`"`: `$(` and a backtick open a body outside single quotes,
/// `(`, `<(` and `>(` only outside every quote, and an unclosed opener is
/// [`Lifted::Unbalanced`]. Without `quotes`, every opener is live and an
/// unclosed one is left in the outer text.
fn lift(command: &str, quotes: bool) -> Lifted {
    let heredocs = HeredocBodies::scan(command);
    let bytes = command.as_bytes();
    let (mut outer, mut bodies) = (String::with_capacity(command.len()), Vec::new());
    let (mut quote, mut copied, mut i) = (Quote::None, 0, 0);
    while i < bytes.len() {
        if heredocs.contains(i) {
            // #8730: a body handed to a shell (`bash <<'EOF'`) is shell source,
            // so it is lifted whole and judged as a command; a data body is
            // skipped.
            if let Some(end) = heredocs.shell_body_starting_at(i) {
                outer.push_str(&command[copied..i]);
                bodies.push(command[i..end].to_string());
                copied = end;
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        let b = bytes[i];
        if quote == Quote::Single {
            quote = if b == b'\'' { Quote::None } else { quote };
            i += 1;
            continue;
        }
        if b == b'\\' {
            i += 2;
            continue;
        }
        // A comment's `(` or `'` is prose: `ls # see (1` runs `ls`.
        if b == b'#' && quote == Quote::None && starts_a_word(bytes, i) {
            i = command[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if quotes && b == b'\'' && quote == Quote::None {
            quote = Quote::Single;
        } else if quotes && b == b'"' {
            quote = if quote == Quote::Double {
                Quote::None
            } else {
                Quote::Double
            };
        }
        let Some(len) = opener_at(bytes, i, quote) else {
            i += 1;
            continue;
        };
        match close_of(bytes, i + len, bytes[i + len - 1] == b'`', quotes) {
            Some(close) => {
                outer.push_str(&command[copied..i + len]);
                bodies.push(command[i + len..close].to_string());
                copied = close;
                i = close + 1;
            }
            None if quotes => return Lifted::Unbalanced,
            None => i += len,
        }
    }
    if quote != Quote::None {
        return Lifted::OpenQuote;
    }
    outer.push_str(&command[copied..]);
    Lifted::Done(outer, bodies)
}

/// Whether byte `i` begins a shell word, which is where `#` opens a comment.
fn starts_a_word(bytes: &[u8], i: usize) -> bool {
    i == 0
        || matches!(
            bytes[i - 1],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'('
        )
}

/// The length of a body opener at `i`, when one is live in `quote`.
fn opener_at(bytes: &[u8], i: usize, quote: Quote) -> Option<usize> {
    let next_is_paren = bytes.get(i + 1) == Some(&b'(');
    match bytes[i] {
        b'$' if next_is_paren => Some(2),
        b'`' => Some(1),
        b'<' | b'>' if next_is_paren && quote == Quote::None => Some(2),
        b'(' if quote == Quote::None => Some(1),
        _ => None,
    }
}

/// The index of the byte closing a body that starts at `start`: the matching
/// backtick, or the `)` that returns the paren depth to zero outside quotes.
fn close_of(bytes: &[u8], start: usize, backtick: bool, quotes: bool) -> Option<usize> {
    let (mut depth, mut quote, mut j) = (1usize, Quote::None, start);
    while j < bytes.len() {
        let b = bytes[j];
        match (quote, b) {
            (Quote::Single, b'\'') => quote = Quote::None,
            (Quote::Single, _) => {}
            (_, b'\\') => j += 1,
            (_, b'`') if backtick => return Some(j),
            (Quote::None, b'\'') if quotes => quote = Quote::Single,
            (Quote::None, b'"') if quotes => quote = Quote::Double,
            (Quote::Double, b'"') => quote = Quote::None,
            (Quote::None, b'(') if !backtick => depth += 1,
            (Quote::None, b')') if !backtick => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(command: &str) -> Vec<String> {
        shell_write_targets(command).unwrap_or_else(|e| panic!("`{command}`: {e}"))
    }

    // #8730: each shape below wrote `src/lib.rs` with no target the boundary
    // could read — `tee` names it as an argument, and the subshell and
    // substitution forms glued `)` to it or hid it inside double quotes.
    #[test]
    fn write_targets_read_tee_substitution_and_subshell_writes() {
        for command in [
            "tee src/lib.rs",
            "echo x | tee src/lib.rs",
            "echo x | tee -a src/lib.rs",
            "echo x | tee -- src/lib.rs",
            "echo x | sudo tee src/lib.rs",
            "echo x | sudo -u root tee src/lib.rs",
            "echo x | tee src/lib.rs > /dev/null",
            "echo x | t'e'e src/lib.rs",
            "echo $(echo x > src/lib.rs)",
            "echo \"$(echo x > src/lib.rs)\"",
            "echo `echo x > src/lib.rs`",
            "echo $(echo x | tee src/lib.rs)",
            "echo $(cd /tmp; echo x > src/lib.rs)",
            "(echo x > src/lib.rs)",
            "( cd /tmp && echo x > src/lib.rs )",
            "(echo x | tee src/lib.rs)",
            "echo x > >(tee src/lib.rs)",
            "sh -c 'echo $(echo x > src/lib.rs)'",
            "echo $(echo $(echo x > src/lib.rs))",
        ] {
            assert!(
                targets(command).contains(&"src/lib.rs".to_string()),
                "`{command}` -> {:?}",
                targets(command)
            );
        }
        assert_eq!(targets("tee a.md b.rs"), vec!["a.md", "b.rs"]);
        assert_eq!(targets("tee -a a.md < in.txt"), vec!["a.md"]);
    }

    // #8730: fail closed. Each shape hides a write the guard cannot place.
    #[test]
    fn write_targets_refuse_what_they_cannot_place() {
        for command in [
            "echo $(echo x > src/lib.rs",
            "(echo x > src/lib.rs",
            "echo `echo x > src/lib.rs",
            "echo x | tee 'src/lib.rs",
        ] {
            assert!(shell_write_targets(command).is_err(), "{command}");
        }
        let deep = format!("{}echo x > src/lib.rs{}", "$(".repeat(40), ")".repeat(40));
        assert_eq!(shell_write_targets(&deep), Err(TOO_DEEP));
    }

    // #8730: what is not a live body stays out, and nothing here is refused —
    // single quotes, here-document prose, `$((…))` arithmetic.
    #[test]
    fn write_targets_leave_literal_text_alone() {
        for command in [
            "echo '$(echo x > src/lib.rs)'",
            "echo '(tee src/lib.rs)'",
            "cat > notes.md <<'EOF'\nrun $(echo x > src/lib.rs) or (tee src/lib.rs)\nEOF",
            "cat > notes.md <<'EOF'\nit's (open and $(unclosed\nEOF",
            "echo $((1 + 2)) > n.txt",
            "grep -rn tee src/",
            "git log --format='(%h)'",
            "ls # see (1, it's",
            "tee notes.md <<'EOF'\nsrc/lib.rs\nEOF",
        ] {
            let got = targets(command);
            assert!(
                !got.iter().any(|t| t.contains("lib.rs")),
                "`{command}` -> {got:?}"
            );
        }
    }

    // #8730 critic, CRITICAL 1: the `|` of `>|` was cut as a pipe and the
    // target read as empty, so a clobber redirect named no file. zsh (the
    // host shell) also writes through `>>|`, `>!` and `>>!`.
    #[test]
    fn write_targets_read_a_clobber_redirect() {
        for command in [
            "echo x >|src/lib.rs",
            "echo x >| src/lib.rs",
            "echo x >>| src/lib.rs",
            "echo x >! src/lib.rs",
            "echo x >>!src/lib.rs",
            "echo $(echo x >| src/lib.rs)",
        ] {
            assert_eq!(targets(command), vec!["src/lib.rs"], "{command}");
        }
    }

    // #8730 round 3: the byte scanner read every `>&` as a descriptor copy, but
    // `>&word` opens `word` for stdout and stderr (bash), and zsh's `>&|`/`>&!`
    // always open a file. A real descriptor copy still names nothing.
    #[test]
    fn write_targets_read_a_descriptor_redirect_that_names_a_file() {
        for command in [
            "echo x >&src/lib.rs",
            "echo x >& src/lib.rs",
            "echo x >&!src/lib.rs",
            "echo x >&| src/lib.rs",
            "echo x 2>&1 >&src/lib.rs",
            "(echo x >&src/lib.rs)",
        ] {
            assert_eq!(targets(command), vec!["src/lib.rs"], "{command}");
        }
        for command in [
            "cargo test 2>&1",
            "echo x >&2",
            "echo x 2>&-",
            "echo x >& 2",
            "x 1>&2-",
        ] {
            assert_eq!(targets(command), Vec::<String>::new(), "{command}");
        }
    }

    // #8730 critic, CRITICAL 2: a body handed to a shell is shell source, and
    // its substitutions were skipped with the rest of the here-document. A
    // data body stays data.
    #[test]
    fn write_targets_read_a_shell_heredoc_body() {
        for command in [
            "bash <<'EOF'\n$(tee src/lib.rs)\nEOF",
            "bash <<'EOF'\necho $(echo x | tee src/lib.rs)\nEOF",
            "sh <<'EOF'\nsh -c \"$(echo x > src/lib.rs)\"\nEOF",
            "bash <<'EOF'\necho x > src/lib.rs\nEOF",
            "bash <<'EOF'\necho x | tee src/lib.rs\nEOF",
            "zsh <<EOF\necho x >| src/lib.rs\nEOF",
        ] {
            assert!(
                targets(command).contains(&"src/lib.rs".to_string()),
                "`{command}` -> {:?}",
                targets(command)
            );
        }
        let data = "cat <<'EOF' > notes.md\n$(tee src/lib.rs)\nEOF";
        assert_eq!(targets(data), vec!["notes.md"]);
    }

    // #8730 critic, MEDIUM: an operand read without its lifted body keeps no
    // glued `)`. The `a(1).md` row tests `tee_targets` in isolation: through
    // the full pipeline `opener_at` lifts `(1)` as a subshell body first, so
    // the operand reads `a().md` (bash rejects the unquoted shape anyway).
    #[test]
    fn write_targets_strip_a_glued_close_paren_from_a_tee_operand() {
        assert_eq!(
            tee_targets("tee src/lib.rs)"),
            Ok(vec!["src/lib.rs".to_string()])
        );
        assert_eq!(
            tee_targets("$(tee src/lib.rs)"),
            Ok(vec!["src/lib.rs".to_string()])
        );
        assert_eq!(tee_targets("tee a(1).md"), Ok(vec!["a(1).md".to_string()]));
    }

    // #8730: bash opens every redirect of a segment; the first no longer hides
    // the rest.
    #[test]
    fn write_targets_reads_every_redirect_of_a_segment() {
        assert_eq!(
            targets("echo x > notes.md > src/lib.rs"),
            vec!["notes.md", "src/lib.rs"]
        );
        assert_eq!(
            targets("echo x > crates/x/src/$(true)lib.rs"),
            vec!["crates/x/src/$()lib.rs"]
        );
    }
}
