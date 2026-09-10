//! `tm hook --pm-guard` — a Bash command, `Read` or `Grep` that NAMES a
//! secret-bearing file (issue #7266).
//!
//! Why: a `local-ops` agent told "never print tfvars values" printed an ngrok
//! authtoken by running `sed -n '38,46p' terraform.tfvars`. Rounds 1 through 4
//! answered that by enumerating the verbs that print file bytes — `cat`, `sed`,
//! `head`, then `base64`, `diff`, then a process-substitution wrapper — and
//! each round's critic bypassed the list with a verb it did not name:
//! `dd if=.env`, `tar cf - .env`, `php -r 'readfile(".env")'`, `deno eval`,
//! `perl -ne`, and `echo "$(cat .env)"`, which no lexer-based operand rule sees
//! at all. Every round found a sibling class the previous one missed, so round
//! 5 inverts the rule: the FILE decides, not the verb.
//!
//! What: [`evaluate_secret_file_read_command`] denies a Bash segment as soon as
//! any word in it names a secret-bearing file, whatever the segment does with
//! it, and [`evaluate_secret_file_read_tool`] denies a `Read` or `Grep` call on
//! the same class of path. [`SAFE_HANDLING_VERBS`] and
//! [`SAFE_GIT_SUBCOMMANDS`] are the only escape: a segment whose program is one
//! of them, which runs no nested command, and whose secret-shaped words are all
//! direct arguments of that program, is allowed. Those verbs move, delete,
//! stage or describe a file; none of them prints its bytes.
//!
//! The verb no longer needs parsing, so the reading it does no longer needs
//! recognising. `$(cat .env)`, `dd if=.env`, `xxd .env`,
//! `python -c 'open(".env")'` and an unlexable segment carrying `.env` all
//! reach the same deny through the same test — [`secret_files_named_in`] cuts
//! the raw segment at every byte a path cannot contain, so a filename hidden
//! inside a quoted program string, a substitution or a broken command still
//! surfaces as a word. A command this guard cannot lex therefore fails CLOSED,
//! because lexing is only ever consulted to GRANT the allowlist, never to deny.
//!
//! The file classifier is the one `pm_guard_bash::secret_file_copy` owns, read
//! here through [`is_secret_read_target`]; round 5 adds no pattern. What it
//! does add is a shape test, [`names_a_secret_file`]. Three denylist entries
//! (`*credentials*`, `*secrets*`, `token*`) have an English word for a literal
//! core, and a rule that fires under every verb would refuse
//! `echo "no secrets here"`, `git commit -m "fix token handling"` and
//! `grep -rn credentials src/`. A word matched only by those three
//! (`pm_guard_bash::matches_only_name_substring_family`) counts as a file
//! reference only when it is written as a path — `cat ./secrets` denies,
//! `find secrets/ -name '*.md'` does not. Every other family is a filename and
//! nothing else, so `cat id_rsa` denies on the bare word.
//!
//! What this costs, deliberately: naming a secret-shaped file in ANY command
//! now denies, including one that reads nothing — `git log --grep .env`,
//! `git commit -m "add .env.example"`, `cp .env .env.bak`. Rounds 1 to 4
//! allowed those and were bypassed four times; a rule an agent can route around
//! by renaming the verb protects nothing. Rephrasing the message, or handing
//! the file to a tool by absolute path, is the way through.
//!
//! Residual, named rather than silently allowed: a file called exactly
//! `secrets` or `token` with no extension and no directory in front of it is
//! not screened (see above); a path that reaches the command only through a
//! variable (`sed -n 1,5p "$F"`) is not resolved, because this rule reads words
//! and not the filesystem; `git show HEAD:terraform.tfvars` prints a COMMITTED
//! copy, which no filename rule sees; and a GLOB whose only literal is the TAIL
//! of an `.env.<name>` file (`Grep(glob = "*.production")`) names no family's
//! core, a trade #7266 round 4 made to keep every ordinary extension search
//! working.
//!
//! Test: `denies_the_reported_sed_line_range`,
//! `denies_every_bypass_the_earlier_rounds_missed`,
//! `allows_the_ordinary_command_corpus`,
//! `allows_only_the_safe_handling_verbs`,
//! `denies_an_unlexable_segment_that_names_a_secret`, and the rest of this
//! module's `tests` submodule. The rule is proved WIRED end to end through the
//! real binary by `pm_guard_denies_a_line_range_read_of_a_secret_bearing_file`,
//! `pm_guard_denies_a_read_tool_call_on_a_secret_bearing_file`,
//! `pm_guard_denies_a_grep_tool_call_on_a_secret_bearing_file`,
//! `pm_guard_denies_a_grep_glob_that_can_match_a_secret`,
//! `pm_guard_denies_a_read_through_process_substitution`,
//! `pm_guard_denies_every_verb_bypass_of_the_secret_file_rule`,
//! `pm_guard_allows_the_safe_handling_verbs_on_a_secret_file` and
//! `pm_guard_still_allows_ordinary_reads_and_non_operand_mentions` in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::Path;

use crate::commands::hook_rewrite::{first_command_token, strip_wrapper_prefix};
use crate::commands::pm_guard_bash::{
    expand_brace_alternatives, git_subcommand, matches_only_name_substring_family,
    secret_pattern_overlaps, split_shell_segments, strip_process_substitution,
};

/// Programs that may name a secret-bearing file without printing its bytes.
///
/// Why: #7266 rounds 1 to 4 listed the verbs that PRINT a file and were
/// bypassed four times by a verb the list did not carry. Round 5 lists the
/// verbs that do not print instead, which is a question with a short and
/// stable answer: `ls` and `stat` report metadata, `file` reports a type,
/// `test`/`[` answer a predicate, and `rm` deletes. None of them can put a
/// credential in the transcript, and an agent needs all five to manage a
/// secret file it must never read.
/// What: matched against the BASENAME of the segment's resolved program, after
/// `strip_wrapper_prefix` removes leading env assignments and `sudo`/`nice`
/// noise. Anything not on this list, and not a [`SAFE_GIT_SUBCOMMANDS`] git
/// call, denies.
/// Test: `allows_only_the_safe_handling_verbs`,
/// `denies_every_bypass_the_earlier_rounds_missed`.
const SAFE_HANDLING_VERBS: &[&str] = &["ls", "stat", "rm", "test", "[", "file"];

/// `git` subcommands that may name a secret-bearing file.
///
/// Why: staging, removing, renaming and status-checking a file are the git
/// operations that touch a path without emitting its contents. `git show`,
/// `git diff`, `git log -p` and `git cat-file` all print bytes, so git is not
/// safe as a program — only these four subcommands are.
/// What: compared against `pm_guard_bash::git_subcommand`'s answer, which
/// already resolves the subcommand behind `git -C <path>` and the other global
/// options.
/// Test: `allows_only_the_safe_handling_verbs`,
/// `denies_a_git_subcommand_that_prints_file_bytes`.
const SAFE_GIT_SUBCOMMANDS: &[&str] = &["add", "rm", "mv", "status"];

/// Shell text that runs a SECOND command inside the segment.
///
/// Why: `ls $(cat .env)` has `ls` for a program and prints the file anyway.
/// The allowlist above is a claim about what the segment's program does with
/// its arguments, and that claim is void as soon as another command runs
/// inside them.
/// What: substrings checked against the raw segment; any hit withdraws the
/// allowlist, so the segment falls through to the deny.
/// Test: `denies_a_safe_verb_wrapping_a_substitution`.
const NESTED_COMMAND_MARKERS: &[&str] = &["$(", "`", "<(", ">(", "${"];

/// Bytes a filename can carry, for the purpose of cutting a raw segment into
/// candidate path words.
///
/// Why: the deny must not depend on lexing, because `echo "$(cat .env)"`,
/// `php -r 'readfile(".env")'` and an unbalanced quote all defeat a lexer while
/// still naming the file in plain text. Cutting at every byte a path cannot
/// contain surfaces the name in all three.
/// What: ASCII alphanumerics plus the punctuation a real path uses. A quote,
/// `$`, `(`, `=`, `*`, `<`, `:` and whitespace are all cuts, so
/// `if=.env`, `"$(cat .env)"` and `*.env` each yield the bare name.
/// Test: `secret_files_named_in_finds_a_name_inside_a_program_string`.
fn is_path_byte(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@' | '{' | '}' | ',')
}

/// File extensions whose content is source or markup, never a credential value.
///
/// Why: the shared classifier's `*credentials*`, `*secrets*` and `token*`
/// patterns match 24 tracked files in this repository alone. Every
/// extension-typed secret family is unaffected, because none of `*.tfvars`,
/// `.env*`, `*.pem`, `*.key`, `id_rsa*`, `*.p12`, `*.pfx`, `*.jks`, `*.kdbx`,
/// `*.ovpn` or `.netrc` ends in one of these.
/// `.json` and `.toml` are deliberately ABSENT: `credentials.json` and
/// `secrets.toml` are real credential-store spellings, and `*.tfvars.json` is
/// itself a JSON secret.
/// Test: `allows_reading_ordinary_source_files_that_match_a_substring_pattern`.
const TRANSPARENT_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "css", "scss", "sass", "svelte", "vue", "py",
    "rb", "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "md", "mdx", "html", "htm",
    "sh", "bash", "zsh", "yml", "yaml", "snap", "baseline", "lock",
];

/// Classify any tool call for a secret-bearing file: `Some(reason)` denies,
/// `None` allows.
///
/// Why: the one entry point `pm_guard` calls, so a Bash command and a `Read`
/// tool call are decided by the same rule and reported with the same reason.
/// What: routes `Bash` to [`evaluate_secret_file_read_command`] over its
/// `command` string and every other tool to [`evaluate_secret_file_read_tool`].
/// Test: `the_unified_entry_point_routes_both_surfaces`.
pub(crate) fn evaluate_secret_file_read(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<String> {
    if tool_name == "Bash" {
        let command = tool_input
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return evaluate_secret_file_read_command(command);
    }
    evaluate_secret_file_read_tool(tool_name, tool_input)
}

/// Classify a Bash command that names a secret-bearing file: `Some(reason)`
/// denies, `None` allows.
///
/// Why: the inverted rule #7266 round 5 asks for. Rounds 1 to 4 asked "is this
/// verb one that prints a file?" and each round's critic answered with a verb
/// the list had never heard of. This asks "does this segment name a secret
/// file?" instead, which no new verb can change the answer to.
/// What: walks [`split_shell_segments`] (which already descends into an
/// `sh -c` wrapper) and, for each segment, collects every word that
/// [`names_a_secret_file`] answers for. A segment naming none allows. A segment
/// naming one allows only when [`segment_only_handles`] proves the segment is a
/// [`SAFE_HANDLING_VERBS`] or [`SAFE_GIT_SUBCOMMANDS`] call taking those words
/// as direct arguments; otherwise the first such word denies.
/// Test: `denies_the_reported_sed_line_range`,
/// `denies_every_bypass_the_earlier_rounds_missed`,
/// `allows_the_ordinary_command_corpus`.
pub(crate) fn evaluate_secret_file_read_command(command: &str) -> Option<String> {
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let named = secret_files_named_in(trimmed);
        let Some(first) = named.first() else {
            continue;
        };
        if segment_only_handles(trimmed, &named) {
            continue;
        }
        return Some(deny_reason(first, &describe_command(trimmed)));
    }
    None
}

/// Every distinct word in `text` that names a secret-bearing file, in order.
///
/// Why: see [`is_path_byte`] — the deny must survive a segment no lexer can
/// read, so the scan reads bytes rather than tokens.
/// What: cuts `text` at every non-path byte and keeps the words
/// [`names_a_secret_file`] answers for, without repeats.
/// Test: `secret_files_named_in_finds_a_name_inside_a_program_string`.
fn secret_files_named_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !is_path_byte(c)) {
        if word.is_empty() || !names_a_secret_file(word) {
            continue;
        }
        if !out.iter().any(|seen| seen == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// Whether one word names a file this guard refuses to let a command touch.
///
/// Why: the classifier alone is not enough once the rule fires under every
/// verb. `*credentials*`, `*secrets*` and `token*` have English words for
/// literal cores, so `echo "no secrets here"` and `grep -rn credentials src/`
/// would deny on a bare classifier hit — over-blocking of exactly the kind
/// #7266 round 4 had to reverse.
/// What: takes the word's basename, requires [`is_secret_read_target`], and
/// then requires FILE SHAPE: a leading `.`, an extension, a family that is a
/// filename rather than a word
/// (`pm_guard_bash::matches_only_name_substring_family`), or — for the word
/// families — a directory written in front of it (`./secrets`, `/etc/secrets`,
/// but not the directory `secrets/`).
/// Test: `allows_the_ordinary_command_corpus`,
/// `a_word_family_counts_only_when_it_is_written_as_a_path`.
fn names_a_secret_file(word: &str) -> bool {
    let base = command_basename(word);
    if base.is_empty() || !is_secret_read_target(&base) {
        return false;
    }
    if base.starts_with('.') || Path::new(&base).extension().is_some() {
        return true;
    }
    if !matches_only_name_substring_family(&base) {
        return true;
    }
    word.contains('/') && !word.ends_with('/')
}

/// Whether `segment` is a safe-verb call taking every one of `named` as a
/// direct argument.
///
/// Why: the narrow escape the deny needs to stay usable — an agent must still
/// be able to see that `.env` exists, stage `.env.example`, and delete
/// `.env.bak`. Granting that on the PROGRAM is sound only while the program is
/// the only thing that runs, which is why a nested command withdraws it.
/// What: refuses on any [`NESTED_COMMAND_MARKERS`] hit, then requires
/// `shlex::split` to succeed — so an unlexable segment can never be granted the
/// allowlist and fails closed — then requires the resolved program to be a
/// [`SAFE_HANDLING_VERBS`] entry, or `git` with a [`SAFE_GIT_SUBCOMMANDS`]
/// subcommand. Finally every word in `named` must reappear as a secret-shaped
/// word of an argument token that follows the program.
/// Test: `allows_only_the_safe_handling_verbs`,
/// `denies_a_safe_verb_wrapping_a_substitution`,
/// `denies_an_unlexable_segment_that_names_a_secret`.
fn segment_only_handles(segment: &str, named: &[String]) -> bool {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return false;
    }
    let Some(argv) = shlex::split(segment) else {
        return false;
    };
    let Some(start) = strip_wrapper_prefix(&argv) else {
        return false;
    };
    let Some(program) = argv.get(start).map(|t| command_basename(t)) else {
        return false;
    };
    let operand_start = if SAFE_HANDLING_VERBS.contains(&program.as_str()) {
        start + 1
    } else if program == "git" {
        let Some(sub) = git_subcommand(segment) else {
            return false;
        };
        if !SAFE_GIT_SUBCOMMANDS.contains(&sub.as_str()) {
            return false;
        }
        let Some(at) = argv.iter().position(|t| *t == sub) else {
            return false;
        };
        at + 1
    } else {
        return false;
    };
    let operands: Vec<String> = argv
        .get(operand_start..)
        .unwrap_or_default()
        .iter()
        .flat_map(|tok| secret_files_named_in(tok))
        .collect();
    named.iter().all(|n| operands.iter().any(|o| o == n))
}

/// How the deny reason names what the segment was doing.
///
/// What: the resolved program when the segment lexes, and an explicit
/// "cannot parse" when it does not — that second case is the fail-closed arm,
/// so the reason says so rather than naming a program it could not resolve.
fn describe_command(segment: &str) -> String {
    if shlex::split(segment).is_none() {
        return "a command this guard cannot parse".to_string();
    }
    match first_command_token(segment) {
        Some(verb) => format!("a `{verb}` command"),
        None => "a command this guard cannot parse".to_string(),
    }
}

/// Classify a native `Read` or `Grep` tool call for a secret-bearing target:
/// `Some(reason)` denies, `None` allows.
///
/// Why: the harness's own `Read` takes `offset`/`limit`, which is the exact
/// line-range shape issue #7266 reports — and a `Read` with no range is a
/// strictly larger exposure, so BOTH deny. `Grep` is the same leak through the
/// second native tool: with `output_mode="content"` it prints every matching
/// line verbatim, so `Grep(pattern=".", path="terraform.tfvars")` dumps the
/// file without a shell ever running. A tool call carries no shell to lex, so
/// these arms read their path fields directly.
/// What: `Some(reason)` when `tool_name` is `Read` and its `file_path`'s
/// basename satisfies [`is_secret_read_target`], or when `tool_name` is `Grep`
/// and [`evaluate_grep_tool`] answers; `None` for every other tool and for a
/// call with no readable path field.
/// Test: `denies_a_read_tool_call_with_a_range`,
/// `denies_a_read_tool_call_without_a_range`,
/// `allows_a_read_of_an_ordinary_file`,
/// `denies_a_grep_tool_call_on_a_secret_bearing_path`,
/// `denies_a_grep_tool_call_whose_glob_names_a_secret`,
/// `allows_a_grep_tool_call_over_a_directory_with_no_glob`,
/// `allows_every_tool_that_prints_no_file_bytes`.
pub(crate) fn evaluate_secret_file_read_tool(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<String> {
    match tool_name {
        "Read" => {
            let target = string_field(tool_input, "file_path")?;
            is_secret_read_target(target).then(|| deny_reason(target, "the `Read` tool"))
        }
        "Grep" => evaluate_grep_tool(tool_input),
        _ => None,
    }
}

/// Classify a `Grep` tool call: `Some(reason)` denies, `None` allows.
///
/// Why: `Grep` reaches a file two ways — `path` naming it directly, or `path`
/// naming a directory with `glob` selecting it by basename pattern. Both print
/// the matching lines under `output_mode="content"`, so both deny. The
/// `output_mode` field is deliberately NOT consulted: it defaults to
/// `files_with_matches` but any call may set it, and a guard that read the mode
/// would allow the dump whenever the field is omitted from the payload.
/// What: denies on a secret-shaped `path`, then on a `glob` that TARGETS a
/// credential family by name — [`is_secret_read_target`] decides both, reading
/// the glob as a pattern rather than as a literal filename (#7266 round 3). A
/// directory `path` with no `glob`, a `glob` carrying no literal character, and
/// a `glob` naming no family's literal core (`*.toml`, `*.txt`) are ordinary
/// tree-wide search and allow. Fails CLOSED on a `glob` whose brace alternation
/// the shared expander cannot resolve, exactly as the copy rule does.
/// Test: `denies_a_grep_tool_call_on_a_secret_bearing_path`,
/// `denies_a_grep_tool_call_whose_glob_names_a_secret`,
/// `denies_a_grep_tool_call_whose_glob_can_match_a_secret`,
/// `allows_a_grep_glob_that_targets_no_credential_family`,
/// `allows_a_grep_tool_call_over_a_directory_with_no_glob`.
fn evaluate_grep_tool(tool_input: Option<&serde_json::Value>) -> Option<String> {
    if let Some(path) = string_field(tool_input, "path")
        && is_secret_read_target(path)
    {
        return Some(deny_reason(path, "the `Grep` tool"));
    }
    let glob = string_field(tool_input, "glob")?;
    is_secret_read_target(glob).then(|| deny_reason(glob, "a `Grep` glob"))
}

/// A non-empty string field of a tool-input object.
fn string_field<'a>(tool_input: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    tool_input
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Whether `path` — a literal path OR a caller-supplied GLOB — names a file
/// this rule refuses to print.
///
/// Why: see the module doc. The shared pattern list is read through
/// [`secret_pattern_overlaps`] rather than a literal match, then narrowed by
/// [`has_transparent_source_extension`] so an ordinary source file that merely
/// carries `token`/`secrets`/`credentials` in its name stays readable.
/// What: expands a brace group with the shared expander (failing CLOSED when it
/// cannot), then answers `true` when ANY alternative both overlaps a
/// secret-bearing pattern and does not end in a transparent extension.
/// Test: `allows_reading_ordinary_source_files_that_match_a_substring_pattern`,
/// `denies_every_extension_typed_secret_family`,
/// `denies_a_grep_tool_call_whose_glob_can_match_a_secret`.
// #7266 round 3: `pub(crate)` so `pm_guard_bash::secret_file_copy`'s rename rule
// asks THIS function whether a destination is a name the read guard refuses,
// instead of reassembling the predicate from two exported halves — the two
// rules can then never disagree.
pub(crate) fn is_secret_read_target(path: &str) -> bool {
    let basename = command_basename(path);
    match expand_brace_alternatives(&basename) {
        // A brace shape the shared expander cannot resolve: fail closed,
        // exactly as the copy rule does.
        None => true,
        Some(candidates) => candidates.iter().any(|c| names_a_secret(c)),
    }
}

/// Whether one already-brace-expanded name or glob is a secret-bearing target.
fn names_a_secret(candidate: &str) -> bool {
    !selects_every_name(candidate)
        && secret_pattern_overlaps(candidate)
        && !has_transparent_source_extension(candidate)
}

/// Whether `candidate` is a wildcard carrying no literal character at all.
///
/// Why: `Grep(path = <dir>, glob = "*")` selects the same files as a `Grep` with
/// no `glob` at all, and a tree-wide `Grep` is ordinary work this rule allows.
/// Denying the spelled-out form while allowing the omitted one would be
/// incoherent, so a glob that names nothing in particular is treated as the
/// no-glob case. A glob carrying even one literal character (`*.env`, `.env*`)
/// is a targeted read and is screened.
/// Test: `allows_a_grep_tool_call_over_a_directory_with_no_glob`.
fn selects_every_name(candidate: &str) -> bool {
    !candidate.is_empty() && candidate.bytes().all(|b| b == b'*' || b == b'?')
}

/// Whether `basename` ends in one of [`TRANSPARENT_SOURCE_EXTENSIONS`].
fn has_transparent_source_extension(basename: &str) -> bool {
    Path::new(basename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| TRANSPARENT_SOURCE_EXTENSIONS.contains(&e.as_str()))
}

/// The basename of a command or path token, with a process-substitution wrapper
/// and a leading `\` quote removed.
fn command_basename(token: &str) -> String {
    // #7266 round 3: `diff <(cat .env) /dev/null` lexes to the tokens `<(cat`
    // and `.env)`, so the wrapper comes off before any basename match.
    let token = strip_process_substitution(token);
    let token = token.strip_prefix('\\').unwrap_or(token);
    let basename = Path::new(token)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(token);
    strip_process_substitution(basename).to_string()
}

/// The deny reason, naming the file, the command that named it, and the way
/// through (issue #7266).
fn deny_reason(target: &str, how: &str) -> String {
    format!(
        "naming `{target}` in {how} is refused (issue #7266) — its name is in this guard's \
         secret-bearing file class (`*.tfvars`, `*.tfvars.json`, `*.tfstate*`, `.env`/`.env.*`, \
         `*.pem`, `*.key`, an SSH private key `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`, \
         `.netrc`, `*.p12`/`*.pfx`/`*.jks`/`*.kdbx`, `*.ovpn`, or a name carrying \
         `credentials`/`secrets`/`token`). This rule keys on the FILE, not on the verb: four \
         earlier rounds enumerated reading verbs and each was bypassed by one the list did not \
         name — `sed -n '38,46p'` printed a live ngrok authtoken, then `dd if=`, `tar cf -`, \
         `php -r`, `deno eval` and `$(cat …)` did the same. Only `ls`, `stat`, `file`, `test`, \
         `rm` and `git add`/`rm`/`mv`/`status` may name such a file. Hand it to the tool that \
         needs it by absolute path (`-var-file`, `-state`, `--env-file`) instead of writing its \
         name into a command that could print it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(command: &str) -> Option<String> {
        evaluate_secret_file_read_command(command)
    }

    /// Every shape a critic drove through rounds 1 to 4, plus the round-4
    /// verdict's four new classes. All must DENY.
    ///
    /// Why: each round's list of reading verbs was bypassed by a verb it did
    /// not carry, so this corpus is the standing proof that the inverted rule
    /// no longer depends on the verb at all.
    const BYPASS_CORPUS: &[&str] = &[
        // Round 4's verdict, the four classes that reopened the issue.
        "echo \"$(cat .env)\"",
        "echo `cat .env`",
        "X=$(cat .env)",
        "dd if=.env of=/dev/stdout",
        "dd if=terraform.tfvars",
        "tar cf - .env",
        "php -r 'readfile(\".env\");'",
        "deno eval 'console.log(Deno.readTextFileSync(\".env\"))'",
        "perl -ne 'print' .env",
        "perl -pe 's/a/b/' terraform.tfvars",
        // Round 1 to 3's classes, which must stay closed.
        "sed -n '38,46p' terraform.tfvars",
        "python3 -c 'print(open(\"terraform.tfvars\").read())'",
        "xxd .env",
        "base64 .env",
        "basenc --base32 terraform.tfvars",
        "strings .env",
        "diff .env /dev/null",
        "diff <(cat .env) /dev/null",
        "cat <(cat .env)",
        "wc -l <(sed -n '1,5p' /repo/.env)",
        "cp .env /tmp/x",
        "mv .env x",
        "grep -r SECRET .env",
        "while read l; do echo x; done < .env",
        "read -r line <live.tfvars",
        "sudo cat /etc/app/.env",
        "sh -c \"head -n 2 live.tfvars\"",
        "cat *.env",
        "head -n 5 *.tfvars",
        "awk '{print} terraform.tfvars",
        "cat id_rsa",
        "cat ./secrets",
        "nl .netrc",
        "od -c server.key",
    ];

    /// Ordinary daily commands that must ALLOW.
    ///
    /// Why: #7266 round 3 passed its own bypass corpus while denying
    /// `Grep(glob = "*.toml")` and every other extension search. A deny rule's
    /// false-positive arm is scored at bypass severity here, so this corpus is
    /// weighted equally with [`BYPASS_CORPUS`].
    const ORDINARY_CORPUS: &[&str] = &[
        // Ordinary file classes, literal and globbed.
        "cat Cargo.toml",
        "sed -n '1,5p' README.md",
        "head -n 20 notes.txt",
        "tail -f build.log",
        "cat crates/trusty-mpm/src/main.rs",
        "grep -rn TODO --include=*.rs crates/",
        "grep -rn TODO --include=*.toml .",
        "grep -rn TODO --include=*.json .",
        "grep -rn TODO --include=*.txt .",
        "grep -rn TODO --include=*.log .",
        "grep -rn TODO --include=*.md .",
        "cat *.rs",
        "ls docs/*.md",
        // The word families, which are English words before they are filenames.
        "echo \"no secrets here\"",
        "grep -rn credentials src/",
        "npm install token-bucket token-bucket",
        "find secrets/ -name '*.md'",
        "cargo test -p trusty-mpm token",
        // Read-only redirection shapes (#2745).
        "cargo check -p trusty-mpm 2>/dev/null",
        "cargo build 2>&1 | grep error",
        "ls -la 2>/dev/null | head -n 5",
        // Source files that merely carry a substring pattern in the name.
        "cat crates/trusty-agents/src/llm/credentials.rs",
        "cat docs/design/UI/design-system/tokens.css",
        "cat crates/trusty-audit/src/grounding/secrets.rs",
        "cat .github/workflows/token-drift.yml",
        "cat website/src/lib/theme/tokens.test.ts",
        // The safe-handling verbs, on a secret file.
        "ls -la .env",
        "stat .env",
        "rm .env.bak",
        "test -f .env",
        "[ -f .env ]",
        "file .env",
        "git status",
        "git add .env.example",
        "git -C /repo/infra add terraform.tfvars",
        "git rm --cached .env",
        "git mv .env.old .env.older",
    ];

    #[test]
    fn denies_the_reported_sed_line_range() {
        // The exact shape from issue #7266's report.
        let reason = eval("sed -n '38,46p' terraform.tfvars").expect("denies");
        assert!(reason.contains("terraform.tfvars"), "{reason}");
        assert!(reason.contains("secret-bearing file class"), "{reason}");
        assert!(eval("sed -n '12,14p' infra/terraform.tfvars").is_some());
    }

    #[test]
    fn denies_every_bypass_the_earlier_rounds_missed() {
        for command in BYPASS_CORPUS {
            let reason =
                eval(command).unwrap_or_else(|| panic!("bypass corpus row allowed: `{command}`"));
            assert!(reason.contains("#7266"), "{reason}");
        }
    }

    #[test]
    fn allows_the_ordinary_command_corpus() {
        for command in ORDINARY_CORPUS {
            assert_eq!(
                eval(command),
                None,
                "ordinary corpus row denied: `{command}`"
            );
        }
    }

    #[test]
    fn allows_only_the_safe_handling_verbs() {
        // The allowlist, member by member, on the same file.
        for verb in SAFE_HANDLING_VERBS {
            let command = if *verb == "[" {
                "[ -f .env ]".to_string()
            } else if *verb == "test" {
                "test -f .env".to_string()
            } else {
                format!("{verb} .env")
            };
            assert_eq!(eval(&command), None, "safe verb denied: `{command}`");
        }
        for sub in SAFE_GIT_SUBCOMMANDS {
            let command = format!("git {sub} .env");
            assert_eq!(eval(&command), None, "safe git call denied: `{command}`");
        }
        // A neighbour of each allowlisted verb, which prints bytes.
        for command in ["lsof .env", "statx .env", "rmdir-cat .env", "filecat .env"] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
    }

    #[test]
    fn denies_a_git_subcommand_that_prints_file_bytes() {
        for command in [
            "git diff .env",
            "git log -p .env",
            "git show HEAD -- .env",
            "git stash push .env",
            "git -C /repo diff terraform.tfvars",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
    }

    #[test]
    fn denies_a_safe_verb_wrapping_a_substitution() {
        // `ls` is allowlisted, but the substitution runs a second command.
        for command in [
            "ls $(cat .env)",
            "ls `cat .env`",
            "stat <(cat .env)",
            "rm ${SECRET:-.env}",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
    }

    #[test]
    fn denies_an_unlexable_segment_that_names_a_secret() {
        // An unbalanced quote makes `shlex::split` return `None`. The word scan
        // still finds the file, and the allowlist can never be granted.
        let reason = eval("awk '{print} terraform.tfvars").expect("denies");
        assert!(reason.contains("terraform.tfvars"), "{reason}");
        assert!(reason.contains("cannot parse"), "{reason}");
        // The same shape behind an ALLOWLISTED verb still denies — lexing is
        // consulted only to grant the escape.
        let unlexable_safe = eval("ls -la '.env").expect("denies");
        assert!(unlexable_safe.contains("cannot parse"), "{unlexable_safe}");
    }

    #[test]
    fn denies_a_secret_named_in_a_later_segment() {
        assert!(eval("cd /repo && cat .env").is_some());
        assert!(eval("ls -la .env; cat .env").is_some());
        assert!(eval("git status | grep -c . ; xxd id_rsa").is_some());
    }

    #[test]
    fn a_word_family_counts_only_when_it_is_written_as_a_path() {
        // A bare word is prose; a word with a directory in front of it is a file.
        assert_eq!(eval("echo secrets"), None);
        assert_eq!(eval("ls credentials"), None);
        assert!(eval("cat ./secrets").is_some());
        assert!(eval("cat /etc/app/credentials").is_some());
        // A directory is not a file reference.
        assert_eq!(eval("find secrets/ -type f"), None);
        // A family whose spelling is a filename denies on the bare word.
        assert!(eval("cat id_rsa").is_some());
        assert!(eval("cat id_ed25519").is_some());
    }

    #[test]
    fn secret_files_named_in_finds_a_name_inside_a_program_string() {
        assert_eq!(
            secret_files_named_in("php -r 'readfile(\".env\");'"),
            vec![".env".to_string()]
        );
        assert_eq!(
            secret_files_named_in("dd if=terraform.tfvars of=/dev/stdout"),
            vec!["terraform.tfvars".to_string()]
        );
        // Repeats collapse, order is kept.
        assert_eq!(
            secret_files_named_in("cp .env .env.bak"),
            vec![".env".to_string(), ".env.bak".to_string()]
        );
        assert!(secret_files_named_in("cargo test -p trusty-mpm").is_empty());
    }

    #[test]
    fn the_unified_entry_point_routes_both_surfaces() {
        let bash = serde_json::json!({"command": "sed -n '38,46p' terraform.tfvars"});
        assert!(evaluate_secret_file_read("Bash", Some(&bash)).is_some());
        let read = serde_json::json!({"file_path": "/repo/.env", "offset": 12, "limit": 3});
        assert!(evaluate_secret_file_read("Read", Some(&read)).is_some());
        let allowed = serde_json::json!({"command": "cat Cargo.toml"});
        assert_eq!(evaluate_secret_file_read("Bash", Some(&allowed)), None);
        assert_eq!(evaluate_secret_file_read("Bash", None), None);
    }

    #[test]
    fn denies_every_extension_typed_secret_family() {
        for name in [
            "live.tfvars",
            "live.tfvars.json",
            "terraform.tfstate",
            ".env",
            ".env.production",
            "server.pem",
            "server.key",
            "id_rsa",
            "id_ed25519",
            ".netrc",
            "store.p12",
            "store.pfx",
            "store.jks",
            "vault.kdbx",
            "corp.ovpn",
            "credentials.json",
        ] {
            assert!(eval(&format!("cat {name}")).is_some(), "allowed `{name}`");
        }
    }

    #[test]
    fn denies_the_key_name_only_grep_the_earlier_rounds_carved_out() {
        // #7266 rounds 1 to 4 allowed `grep -o '^key_[a-z_]*' <file>` because
        // the pattern cannot cross the `=`. Round 5 withdraws the carve-out:
        // it was the one place where a verb's FLAGS decided the verdict, and a
        // second `-e` past the checked one (`grep -o -e 'pw=.*' -e '^K' .env`)
        // reached the values. The rule is the file now, so no grep spelling
        // survives.
        for command in [
            "grep -o '^key_[a-z_]*' terraform.tfvars",
            "grep -o '^[a-z_]*' .env",
            "grep --only-matching -e '^app_id' live.tfvars",
            "grep -o -e 'password=.*' -e '^KEY' .env",
            "grep -o '^.*' terraform.tfvars",
            "grep '^key_' terraform.tfvars",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
    }

    #[test]
    fn allows_reading_ordinary_source_files_that_match_a_substring_pattern() {
        for path in [
            "crates/trusty-agents/src/llm/credentials.rs",
            "docs/design/UI/design-system/tokens.css",
            "crates/trusty-audit/src/grounding/secrets.rs",
            ".github/workflows/token-drift.yml",
            "website/src/lib/theme/tokens.test.ts",
        ] {
            assert_eq!(eval(&format!("cat {path}")), None, "denied `{path}`");
        }
    }

    #[test]
    fn denies_a_read_tool_call_with_a_range() {
        let input = serde_json::json!({"file_path": "/repo/.env", "offset": 12, "limit": 3});
        let reason = evaluate_secret_file_read_tool("Read", Some(&input)).expect("denies");
        assert!(reason.contains("`Read` tool"), "{reason}");
        assert!(reason.contains("/repo/.env"), "{reason}");
    }

    #[test]
    fn denies_a_read_tool_call_without_a_range() {
        let input = serde_json::json!({"file_path": "/repo/infra/terraform.tfvars"});
        assert!(evaluate_secret_file_read_tool("Read", Some(&input)).is_some());
    }

    #[test]
    fn allows_a_read_of_an_ordinary_file() {
        let input = serde_json::json!({"file_path": "/repo/README.md"});
        assert_eq!(evaluate_secret_file_read_tool("Read", Some(&input)), None);
    }

    #[test]
    fn allows_every_tool_that_prints_no_file_bytes() {
        // `Grep` is deliberately absent from this list since #7266's fix round:
        // it is the second native tool that prints file bytes, so it has its
        // own arm above rather than a blanket allow.
        let input = serde_json::json!({"file_path": "/repo/.env"});
        for tool in ["Write", "Edit", "Bash", "Glob", "Task"] {
            assert_eq!(evaluate_secret_file_read_tool(tool, Some(&input)), None);
        }
        assert_eq!(evaluate_secret_file_read_tool("Read", None), None);
        assert_eq!(
            evaluate_secret_file_read_tool("Read", Some(&serde_json::json!({"file_path": ""}))),
            None
        );
    }

    #[test]
    fn denies_a_grep_tool_call_on_a_secret_bearing_path() {
        // `output_mode: "content"` prints every matching line verbatim, so a
        // pattern matching everything dumps the file with no shell involved.
        let input = serde_json::json!({
            "pattern": ".",
            "path": "/repo/infra/terraform.tfvars",
            "output_mode": "content",
        });
        let reason = evaluate_secret_file_read_tool("Grep", Some(&input)).expect("denies");
        assert!(reason.contains("`Grep` tool"), "{reason}");
        assert!(reason.contains("terraform.tfvars"), "{reason}");
        // The mode is not consulted: an omitted `output_mode` denies too.
        let bare = serde_json::json!({"pattern": "authtoken", "path": "/repo/.env"});
        assert!(evaluate_secret_file_read_tool("Grep", Some(&bare)).is_some());
    }

    #[test]
    fn denies_a_grep_tool_call_whose_glob_names_a_secret() {
        for glob in ["*.tfvars", "**/.env", "*.pem", "id_rsa*"] {
            let input = serde_json::json!({
                "pattern": ".",
                "path": "/repo/infra",
                "glob": glob,
                "output_mode": "content",
            });
            let reason = evaluate_secret_file_read_tool("Grep", Some(&input))
                .unwrap_or_else(|| panic!("glob `{glob}` must deny"));
            assert!(reason.contains(glob), "{reason}");
        }
        // Fails closed on a brace group the shared classifier cannot resolve.
        let unresolved = serde_json::json!({"pattern": ".", "glob": "notes.{md,txt"});
        assert!(evaluate_secret_file_read_tool("Grep", Some(&unresolved)).is_some());
    }

    #[test]
    fn denies_a_grep_tool_call_whose_glob_can_match_a_secret() {
        // Every one of these was ALLOWED on ec8ea341d: the glob was compared
        // to the denylist as though it were a filename, so only an entry
        // spelled with a `*` in the same place ever matched.
        for glob in [
            "*.env",
            "*.netrc",
            ".env*",
            "*.local",
            "*.tfvars",
            "*.key",
            "id_*",
            "token*",
            "*credentials*",
            "*.p12",
            "*.kdbx",
            "*.ovpn",
            "*.pfx",
            "*.jks",
            "*.tfstate",
        ] {
            let input = serde_json::json!({
                "pattern": ".",
                "path": "/repo/infra",
                "glob": glob,
                "output_mode": "content",
            });
            let reason = evaluate_secret_file_read_tool("Grep", Some(&input))
                .unwrap_or_else(|| panic!("glob `{glob}` must deny"));
            assert!(reason.contains(glob), "{reason}");
        }
    }

    #[test]
    fn allows_a_grep_glob_that_targets_no_credential_family() {
        // #7266 round 4: every one of these DENIED under round 3's sound
        // pattern-overlap screen, because `credentials.toml` and `.env.log` are
        // names the four unbounded families reach. They are ordinary tree
        // searches and must allow.
        for glob in [
            "*.rs",
            "**/*.md",
            "*.{rs,ts}",
            "*",
            "**/*",
            "*.toml",
            "*.txt",
            "*.log",
            "*.csv",
            "*.tf",
            "*test*",
            "*.yaml",
        ] {
            let input = serde_json::json!({"pattern": "TODO", "path": "/repo/src", "glob": glob});
            assert_eq!(
                evaluate_secret_file_read_tool("Grep", Some(&input)),
                None,
                "glob `{glob}` must allow"
            );
        }
    }

    #[test]
    fn allows_a_grep_tool_call_over_a_directory_with_no_glob() {
        // Grepping a tree is ordinary work and stays allowed.
        for input in [
            serde_json::json!({"pattern": "TODO", "path": "/repo/crates", "output_mode": "content"}),
            serde_json::json!({"pattern": "TODO"}),
            serde_json::json!({"pattern": "TODO", "path": "/repo/src", "glob": "*.rs"}),
            serde_json::json!({"pattern": "tfvars", "path": "/repo/docs", "glob": "*.md"}),
        ] {
            assert_eq!(
                evaluate_secret_file_read_tool("Grep", Some(&input)),
                None,
                "{input}"
            );
        }
    }

    #[test]
    fn denies_a_secret_read_through_process_substitution() {
        // The round-3 critic's exact commands. `shlex::split` yields `<(cat`
        // and `.env)`, so no operand rule saw the basename on ec8ea341d.
        for command in [
            "diff <(cat .env) /dev/null",
            "cat <(cat .env)",
            "diff <(base64 terraform.tfvars) /dev/null",
            "wc -l <(sed -n '1,5p' /repo/.env)",
            "paste <(cut -d= -f2 .env)",
        ] {
            let reason = eval(command).unwrap_or_else(|| panic!("`{command}` must deny"));
            assert!(reason.contains("#7266"), "{reason}");
        }
        // A process substitution over an ordinary file is untouched.
        assert_eq!(eval("diff <(cat README.md) /dev/null"), None);
    }

    #[test]
    fn command_basename_strips_a_process_substitution_wrapper() {
        assert_eq!(command_basename("<(cat"), "cat");
        assert_eq!(command_basename("/repo/infra/.env)"), ".env");
        assert_eq!(command_basename(".env)"), ".env");
        assert_eq!(command_basename("README.md"), "README.md");
    }
}
