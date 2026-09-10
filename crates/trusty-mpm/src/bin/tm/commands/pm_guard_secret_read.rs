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
//! Round 6 closes what round 5's word scan let through and withdraws what it
//! took too much of. A GLOB is now kept whole in the word rather than cut at
//! its wildcard, so `cat .en?`, `cat .e*` and `cat id_[r]sa` reach the same
//! pattern screen the `Grep` `glob` arm uses (see [`is_path_byte`] and
//! [`normalize_bracket_classes`]). The scan reads the LEXER'S tokens when the
//! segment lexes, so `cat '.en''v'` and `cat .e\nv` no longer hide the name in
//! quoting (see [`secret_words_in_segment`]); the raw byte scan stays as the
//! fail-closed arm for a segment no lexer can read. `git add` loses its grant
//! under `-p`/`-i`/`-e`, which print the file's diff (see
//! [`CONTENT_REVEALING_GIT_FLAGS`]). Going the other way, a search program's
//! first positional argument is a PATTERN and is skipped, so
//! `grep -rn "\.env" docs/` and `rg 'id_rsa' --type md` — how an agent finds
//! where a secret file is referenced — allow again (see
//! [`PATTERN_FIRST_SEARCH_PROGRAMS`]), and an SSH PUBLIC key reads freely (see
//! [`is_ssh_public_key_name`]).
//!
//! Round 7 closes one false positive of round 6's word scan. A Bash PARAMETER
//! EXPANSION is now recognised as its own shape, distinct from the brace
//! ALTERNATION that `{` and `}` were kept in the word for: `is_path_byte` cuts
//! at `:`, `#` and `%`, so every operator form — `${VAR:-x}`, `${VAR:=x}`,
//! `${VAR:?m}`, `${VAR:+x}`, `${VAR:2:3}`, `${VAR#p}`, `${VAR%%p}` — left an
//! unmatched `{VAR` that `expand_brace_alternatives` cannot resolve and
//! [`is_secret_read_target`] therefore failed CLOSED on.
//! `mkdir -p "${OUT_DIR:-build}"` denied with no secret named. The span is
//! rewritten to its parameter NAME and its OPERAND rather than skipped (see
//! [`rewrite_parameter_expansions`]), so an expansion that names a secret still
//! denies — `cat "${F:-.env}"` and `cat "${F:=id_rsa}"` both do, and
//! `cat ${F-.env}`, which round 6 ALLOWED, denies now too. `${VAR}` and `$VAR`
//! reach the same words they always did.
//!
//! What this costs, deliberately: naming a secret-shaped file in ANY command
//! now denies, including one that reads nothing — `git log --grep .env`,
//! `git commit -m "add .env.example"`, `cp .env .env.bak`. Rounds 1 to 4
//! allowed those and were bypassed four times; a rule an agent can route around
//! by renaming the verb protects nothing. There is no flag that buys an
//! exception — round 5's deny text advertised `--env-file`/`-var-file`/`-state`
//! and no such escape was ever implemented, so round 6 removed the claim rather
//! than build it: a reference flag is one more list to enumerate, which is the
//! shape that failed four times. `*.pem` covers a PUBLIC certificate as well as
//! a private key, so `openssl x509 -in cert.pem -noout -text` denies; splitting
//! the extension by what the file holds needs the file's bytes, which is the
//! read this rule refuses. The way through is to rephrase the message, to run
//! the tool so it picks the file up itself (`docker compose up` reads
//! `./.env`), or to ask the operator.
//!
//! Residual, named rather than silently allowed: a file called exactly
//! `secrets` or `token` with no extension and no directory in front of it is
//! not screened (see above); a path that reaches the command only through a
//! variable (`sed -n 1,5p "$F"`) is not resolved, because this rule reads words
//! and not the filesystem; a filename COMPUTED in-line by the same command —
//! `cat $(printf '\056env')`, `cat $(echo LmVudg== | base64 -d)`, a name
//! assembled by arithmetic — never appears as literal path text, so no word
//! scan can see it, an unbounded class of the same shape as the variable
//! indirection beside it (see `DOCUMENTED_RESIDUALS`); a GLOB whose only
//! literal is the TAIL of an `.env.<name>` file
//! (`Grep(glob = "*.production")`) names no family's core, a trade #7266
//! round 4 made to keep every ordinary extension search working; and a search
//! program's pattern is skipped only when it is written as the first positional
//! argument, so `rg --type md id_rsa` — pattern behind a value-taking flag —
//! still denies. Enumerating which flags take a value is the trade refused
//! there: a wrong entry would skip a real file operand, which is a bypass
//! rather than a false positive.
//!
//! `git show HEAD:terraform.tfvars` is DENIED, not residual: `:` is not a path
//! byte, so `HEAD:terraform.tfvars` cuts into `HEAD` and `terraform.tfvars`,
//! and `show` is not a [`SAFE_GIT_SUBCOMMANDS`] entry (round 5's doc listed it
//! as a gap; it never was one).
//!
//! Test: `denies_the_reported_sed_line_range`,
//! `denies_every_bypass_the_earlier_rounds_missed`,
//! `allows_the_ordinary_command_corpus`,
//! `allows_only_the_safe_handling_verbs`,
//! `denies_an_unlexable_segment_that_names_a_secret`,
//! `denies_a_glob_that_expands_onto_a_secret_file`,
//! `denies_a_name_reassembled_by_quoting_or_escaping`,
//! `denies_git_add_in_a_content_revealing_mode`,
//! `allows_a_secret_name_written_as_a_search_pattern`,
//! `denies_a_secret_file_operand_of_a_search_program`,
//! `allows_reading_an_ssh_public_key`,
//! `the_deny_text_advertises_no_flag_escape`,
//! `allows_a_parameter_expansion_that_names_no_secret`,
//! `denies_a_parameter_expansion_whose_operand_names_a_secret`,
//! `splits_a_parameter_expansion_into_its_name_and_operand`,
//! `the_documented_residuals_still_allow`, and the rest of this
//! module's `tests` submodule. The rule is proved WIRED end to end through the
//! real binary by `pm_guard_denies_a_line_range_read_of_a_secret_bearing_file`,
//! `pm_guard_denies_a_read_tool_call_on_a_secret_bearing_file`,
//! `pm_guard_denies_a_grep_tool_call_on_a_secret_bearing_file`,
//! `pm_guard_denies_a_grep_glob_that_can_match_a_secret`,
//! `pm_guard_denies_a_read_through_process_substitution`,
//! `pm_guard_denies_every_verb_bypass_of_the_secret_file_rule`,
//! `pm_guard_allows_the_safe_handling_verbs_on_a_secret_file`,
//! `pm_guard_still_allows_ordinary_reads_and_non_operand_mentions`,
//! `pm_guard_denies_a_glob_or_quote_join_that_names_a_secret`,
//! `pm_guard_denies_git_add_in_a_content_revealing_mode`,
//! `pm_guard_allows_a_secret_name_as_a_search_pattern_and_a_public_key`,
//! `pm_guard_deny_text_advertises_no_flag_escape` and
//! `pm_guard_reads_a_parameter_expansion_as_its_operand` in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::Path;

use crate::commands::hook_rewrite::{first_command_token, strip_wrapper_prefix};
use crate::commands::pm_guard_bash::{
    any_pattern_overlaps, expand_brace_alternatives, git_subcommand,
    matches_only_name_substring_family, secret_pattern_overlaps, split_shell_segments,
    strip_process_substitution,
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

/// git flags that turn a staging call into a call that PRINTS the file.
///
/// Why: #7266 round 6, critic CRITICAL 3 — [`SAFE_GIT_SUBCOMMANDS`] granted
/// `add` on the subcommand name alone, and `git add -p .env` walks the file's
/// diff hunk by hunk in the transcript. `-i` opens the same content in the
/// interactive picker and `-e` opens the whole diff in an editor. The grant is
/// a claim about what the subcommand does with its operands, and these three
/// flags change that answer, so they withdraw it the way
/// [`NESTED_COMMAND_MARKERS`] does.
/// What: exact long spellings, plus any clustered short flag carrying `p`, `i`
/// or `e`. `git add -A`, `git add -u` and `git add .env.example` are untouched.
/// Test: `denies_git_add_in_a_content_revealing_mode`,
/// `allows_only_the_safe_handling_verbs`.
const CONTENT_REVEALING_GIT_FLAGS: &[&str] = &["--patch", "--interactive", "--edit"];

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
/// What: ASCII alphanumerics plus the punctuation a real path uses, INCLUDING
/// the glob metacharacters `*`, `?`, `[` and `]`. A quote, `$`, `(`, `=`, `<`,
/// `:` and whitespace are all cuts, so `if=.env` and `"$(cat .env)"` each yield
/// the bare name.
///
/// The four glob bytes are kept in the word rather than cut at (#7266 round 6,
/// critic CRITICAL 1). Cutting at them threw the wildcard away and left a
/// remainder that matched nothing: `cat .en?` yielded `.en`, `cat .e*` yielded
/// `.e`, `cat id_rs?` yielded `id_rs`, and all three ALLOWED while naming a
/// glob the shell expands onto the real file. Kept in the word, each reaches
/// [`is_secret_read_target`], which has screened a caller's PATTERN — not just
/// a literal name — since round 3, and which the `Grep` `glob` arm has used all
/// along.
///
/// `{` and `}` are kept for brace ALTERNATION, which is not the only thing a
/// brace spells: [`rewrite_parameter_expansions`] removes every `${…}` span
/// before this cut runs, because an expansion's operator (`:`, `#`, `%`) IS a
/// cut and the `{VAR` it leaves behind fails closed (#7266 round 7).
/// Test: `secret_files_named_in_finds_a_name_inside_a_program_string`,
/// `denies_a_glob_that_expands_onto_a_secret_file`,
/// `allows_a_parameter_expansion_that_names_no_secret`.
fn is_path_byte(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '.' | '_' | '-' | '/' | '~' | '+' | '@' | '{' | '}' | ',' | '*' | '?' | '[' | ']'
        )
}

/// A name or glob with every bracket class collapsed to a single `?`.
///
/// Why: [`is_path_byte`] now keeps `[` and `]` in the word, and a bracket class
/// is the one glob shape the shared matcher does not implement — `cat id_[r]sa`
/// reached `is_secret_bearing_name` as the literal `id_[r]sa`, matched no
/// pattern, and ALLOWED (#7266 round 6, critic CRITICAL 1). A class matches
/// exactly one character, so `?` is its faithful stand-in, and `?` is a
/// WIDENING of it — every string the class reaches, `?` reaches too — which
/// puts the approximation on the deny side.
/// What: a balanced non-empty `[…]` becomes `?`; a stray `[` or `]` is dropped,
/// so a malformed class falls back to the name around it (`cat .env]` still
/// denies) rather than shielding it.
/// Test: `normalize_bracket_classes_collapses_a_class_and_drops_a_stray`,
/// `denies_a_glob_that_expands_onto_a_secret_file`.
fn normalize_bracket_classes(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => match chars[i + 1..].iter().position(|c| *c == ']') {
                Some(close) if close > 0 => {
                    out.push('?');
                    i += close + 2;
                }
                _ => i += 1,
            },
            ']' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The four SSH key families whose `.pub` half is a PUBLIC key.
///
/// Why: see [`is_ssh_public_key_name`].
/// What: the `id_*` entries of the shared denylist, minus their trailing `*`.
/// Test: `allows_reading_an_ssh_public_key`.
const SSH_KEY_FAMILY_PREFIXES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];

/// Whether `basename` is an SSH PUBLIC key, which this READ rule may print.
///
/// Why: #7266 round 6, critic HIGH — the shared denylist screens `id_rsa*`,
/// which reaches `id_rsa.pub` as well as `id_rsa`. A public key is published by
/// definition: it goes into `authorized_keys`, into a GitHub deploy key, into
/// `ssh-copy-id -i id_rsa.pub host`. Denying a read of it costs an agent real
/// work and protects nothing. The COPY rule (#7122) keeps the over-match on
/// purpose — reproducing a key file into a worktree is cheap to re-do by
/// absolute path — so this exemption lives here and does not touch
/// [`is_secret_read_target`], which that rule calls.
/// What: `true` when the lowercased basename starts with a
/// [`SSH_KEY_FAMILY_PREFIXES`] entry and ends in `.pub`, and carries no OTHER
/// family's literal core — `id_rsa_credentials.pub` is a `*credentials*` and
/// stays denied.
/// Test: `allows_reading_an_ssh_public_key`,
/// `denies_a_private_key_and_a_pub_name_carrying_another_family`.
fn is_ssh_public_key_name(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    lower.ends_with(".pub")
        && SSH_KEY_FAMILY_PREFIXES.iter().any(|p| lower.starts_with(p))
        && !["credentials", "secrets", "token"]
            .iter()
            .any(|w| lower.contains(w))
}

/// Whether `path` names a file the READ rule refuses to print.
///
/// Why: the read rule's own view of [`is_secret_read_target`]. It normalises a
/// bracket class the shared matcher cannot read, and it exempts an SSH public
/// key. Both are read-side answers: the copy rule calls
/// [`is_secret_read_target`] directly and is unchanged by either (#7266
/// round 6).
/// What: basename, then [`normalize_bracket_classes`], then
/// [`is_ssh_public_key_name`] as an exemption, then [`is_secret_read_target`].
/// Test: `allows_reading_an_ssh_public_key`,
/// `denies_a_glob_that_expands_onto_a_secret_file`.
fn denies_as_a_read_target(path: &str) -> bool {
    let base = normalize_bracket_classes(&command_basename(path));
    !base.is_empty() && !is_ssh_public_key_name(&base) && is_secret_read_target(&base)
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
        let named = secret_words_in_segment(trimmed);
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

/// Programs whose FIRST positional argument is a search pattern, not a file.
///
/// Why: #7266 round 6, critic HIGH — the word scan read `grep -rn "\.env"
/// docs/` as naming `.env` and denied it, and so for `rg 'id_rsa' --type md`
/// and `grep -rn '\.pem' README.md`. Searching the tree FOR the string is the
/// ordinary way an agent finds where a secret file is referenced, and it prints
/// no byte of that file. The pattern is the one argument of these programs that
/// is never a path, so it is the one argument the scan skips.
/// What: matched against the segment's resolved program basename, plus
/// `git grep`. Everything after the pattern is still scanned, so
/// `grep -r SECRET .env` denies on the operand.
/// Test: `allows_a_secret_name_written_as_a_search_pattern`,
/// `denies_a_secret_file_operand_of_a_search_program`.
const PATTERN_FIRST_SEARCH_PROGRAMS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack"];

/// Flags that supply a search pattern themselves, so no positional one exists.
///
/// Why: `grep -e '\.env' .env` and `grep -f patterns.txt .env` take every
/// positional argument as a FILE. Skipping the first one would skip the secret
/// operand — a bypass, not a false-positive fix.
/// What: exact long and short spellings, plus the `--regexp=`/`--file=` joined
/// forms; a clustered short flag carrying `e` or `f` (`grep -ne`) is caught by
/// the character test in [`pattern_argument_index`]. Any hit withdraws the skip
/// entirely, so the scan reads every token.
/// Test: `denies_a_secret_file_operand_of_a_search_program`.
const EXPLICIT_PATTERN_FLAGS: &[&str] = &["-e", "-f", "--regexp", "--file"];

/// Which token of `argv`, if any, is a search PATTERN rather than a path.
///
/// Why: see [`PATTERN_FIRST_SEARCH_PROGRAMS`]. This is the narrowest form of
/// the "written as a path operand" gate the word families already have — it
/// exempts one token of one program class, and only when nothing else in the
/// segment could have supplied the pattern.
/// What: `None` — scan everything — unless the segment runs no nested command,
/// resolves to a pattern-first search program (or `git grep`), and carries no
/// [`EXPLICIT_PATTERN_FLAGS`] spelling after it. Otherwise the index of the
/// first token after the program that does not start with `-`.
/// Test: `allows_a_secret_name_written_as_a_search_pattern`,
/// `denies_a_secret_file_operand_of_a_search_program`.
fn pattern_argument_index(segment: &str, argv: &[String]) -> Option<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return None;
    }
    let start = strip_wrapper_prefix(argv)?;
    let program = command_basename(argv.get(start)?);
    let rest_start = if PATTERN_FIRST_SEARCH_PROGRAMS.contains(&program.as_str()) {
        start + 1
    } else if program == "git" && git_subcommand(segment)? == "grep" {
        argv.iter().position(|t| t == "grep")? + 1
    } else {
        return None;
    };
    let rest = argv.get(rest_start..)?;
    let supplies_its_own_pattern = rest.iter().any(|t| {
        EXPLICIT_PATTERN_FLAGS.contains(&t.as_str())
            || t.starts_with("--regexp=")
            || t.starts_with("--file=")
            || (t.starts_with('-')
                && !t.starts_with("--")
                && t[1..].chars().any(|c| c == 'e' || c == 'f'))
    });
    if supplies_its_own_pattern {
        return None;
    }
    rest.iter()
        .position(|t| !t.starts_with('-'))
        .map(|i| rest_start + i)
}

/// Every distinct word of one SEGMENT that names a secret-bearing file.
///
/// Why: #7266 round 6, critic CRITICAL 2 — the scan ran on raw text even when
/// the segment lexed cleanly, so a quote join hid the name from it:
/// `cat '.en''v'` cut into `.en` and `v`, `cat .e\nv` into `.e` and `nv`, and
/// both ALLOWED while a shell reads `.env`. The lexer already knows what those
/// words really are, so when it succeeds its TOKENS are what gets scanned.
/// What: `shlex::split`'s tokens, each run through [`secret_files_named_in`],
/// minus the one token [`pattern_argument_index`] identifies as a search
/// pattern. A segment that does not lex falls back to the raw byte scan, which
/// is the fail-CLOSED arm — [`segment_only_handles`] can never grant the
/// allowlist to it either.
/// Test: `denies_a_name_reassembled_by_quoting_or_escaping`,
/// `allows_a_secret_name_written_as_a_search_pattern`,
/// `denies_an_unlexable_segment_that_names_a_secret`.
fn secret_words_in_segment(segment: &str) -> Vec<String> {
    let Some(argv) = shlex::split(segment) else {
        return secret_files_named_in(segment);
    };
    let pattern_at = pattern_argument_index(segment, &argv);
    let mut out: Vec<String> = Vec::new();
    for (index, token) in argv.iter().enumerate() {
        if Some(index) == pattern_at {
            continue;
        }
        for word in secret_files_named_in(token) {
            if !out.contains(&word) {
                out.push(word);
            }
        }
    }
    out
}

/// The operator spellings that separate a `${…}` parameter's NAME from the
/// word the expansion can produce, longest spelling first.
///
/// Why: the operand is the only part of an expansion that can carry a
/// filename, and it is only reachable once the operator in front of it is
/// removed. Leaving the operator on glues it to the name — `${F:-.env}` scans
/// as `-.env`, which matches no pattern and ALLOWS the very shape this rule
/// exists to refuse (#7266 round 7).
/// What: the `:`-guarded and bare default/assign/error/alternate forms, the
/// `#`/`%` prefix and suffix trims, the `/` substitutions, the case and
/// transform operators, and the bare `:` that opens a substring range. Order
/// is significant — a two-character spelling is tried before the
/// one-character spelling it starts with.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`,
/// `allows_a_parameter_expansion_that_names_no_secret`.
const PARAMETER_EXPANSION_OPERATORS: &[&str] = &[
    ":-", ":=", ":?", ":+", "##", "%%", "//", ",,", "^^", "#", "%", "/", "^", ",", "@", ":", "-",
    "=", "?", "+",
];

/// One `${…}` expansion's parameter NAME and the operand behind its operator.
///
/// Why: see [`PARAMETER_EXPANSION_OPERATORS`]. Both halves are returned rather
/// than the operand alone, so a parameter whose NAME is itself a secret-shaped
/// filename (`${id_rsa}`) keeps denying exactly as it did before round 7.
/// What: strips a leading `#` (length) or `!` (indirection) sigil, takes the
/// longest run of `[A-Za-z0-9_]` as the name — or one character when the
/// parameter is a special one like `@` or `*` — then removes the first
/// matching operator. Text after an operator this list does not carry is
/// returned whole, so an unrecognised form is scanned rather than skipped.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`.
fn split_parameter_expansion(inner: &str) -> (&str, &str) {
    let body = inner
        .strip_prefix('#')
        .or_else(|| inner.strip_prefix('!'))
        .unwrap_or(inner);
    let name_len = body
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(body.len());
    let (name, rest) = if name_len == 0 {
        let mut chars = body.chars();
        let taken = chars.next().map_or(0, char::len_utf8);
        body.split_at(taken)
    } else {
        body.split_at(name_len)
    };
    for op in PARAMETER_EXPANSION_OPERATORS {
        if let Some(operand) = rest.strip_prefix(op) {
            return (name, operand);
        }
    }
    (name, rest)
}

/// Index of the `}` closing the `{` at `open`, or `None` when nothing does.
///
/// What: counts nesting, so `${A:-${B}}` yields the OUTER close.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`.
fn matching_close_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, c) in chars.get(open..)?.iter().enumerate() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` with every `${…}` parameter expansion replaced by the words it can
/// actually name.
///
/// Why: #7266 round 7, critic CRITICAL 1. [`is_path_byte`] keeps `{` and `}`
/// for brace ALTERNATION (`cp secret.{tfvars,bak}`) but cuts at `:`, `#` and
/// `%`, so every expansion carrying an operator left an unmatched `{VAR`
/// behind. `expand_brace_alternatives` finds no closing brace for it, answers
/// `None`, and [`is_secret_read_target`] fails CLOSED — so
/// `mkdir -p "${OUT_DIR:-build}"`, `echo "${1:-default}"` and
/// `cp "${SRC%.rs}.bak" x` were all refused with no secret named.
///
/// A `${…}` span is parameter expansion and a bare `{…}` group is brace
/// alternation; only the first is rewritten here, so the alternation arm is
/// untouched. Failing OPEN on the whole span would reopen `${x:-.env}`, so the
/// span is not skipped — it is REWRITTEN to its name and its operand, and both
/// are scanned. `${VAR}` and `$VAR` reach the same words they always did.
/// What: rewrites each balanced `${…}` to ` <name> <operand> `, recursing into
/// the operand so a nested expansion is resolved too. An UNBALANCED `${` is
/// left exactly as it stands, which keeps that shape failing closed.
/// Test: `allows_a_parameter_expansion_that_names_no_secret`,
/// `denies_a_parameter_expansion_whose_operand_names_a_secret`.
fn rewrite_parameter_expansions(text: &str) -> String {
    if !text.contains("${") {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$'
            && chars.get(i + 1) == Some(&'{')
            && let Some(close) = matching_close_brace(&chars, i + 1)
        {
            let inner: String = chars[i + 2..close].iter().collect();
            let (name, operand) = split_parameter_expansion(&inner);
            out.push(' ');
            out.push_str(name);
            out.push(' ');
            out.push_str(&rewrite_parameter_expansions(operand));
            out.push(' ');
            i = close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Every distinct word in `text` that names a secret-bearing file, in order.
///
/// Why: see [`is_path_byte`] — the deny must survive a segment no lexer can
/// read, so the scan reads bytes rather than tokens.
/// What: rewrites every `${…}` parameter expansion first (see
/// [`rewrite_parameter_expansions`]), then cuts `text` at every non-path byte
/// and keeps the words [`names_a_secret_file`] answers for, without repeats.
/// Test: `secret_files_named_in_finds_a_name_inside_a_program_string`,
/// `allows_a_parameter_expansion_that_names_no_secret`.
fn secret_files_named_in(text: &str) -> Vec<String> {
    let scanned = rewrite_parameter_expansions(text);
    let mut out: Vec<String> = Vec::new();
    for word in scanned.split(|c: char| !is_path_byte(c)) {
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
    let base = normalize_bracket_classes(&command_basename(word));
    if base.is_empty() || !denies_as_a_read_target(&base) {
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
        if git_call_reveals_content(&argv, at + 1) {
            return false;
        }
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

/// Whether the git call's arguments from `after` carry a
/// [`CONTENT_REVEALING_GIT_FLAGS`] spelling.
///
/// Test: `denies_git_add_in_a_content_revealing_mode`.
fn git_call_reveals_content(argv: &[String], after: usize) -> bool {
    argv.get(after..).unwrap_or_default().iter().any(|t| {
        CONTENT_REVEALING_GIT_FLAGS.contains(&t.as_str())
            || (t.starts_with('-')
                && !t.starts_with("--")
                && t[1..].chars().any(|c| matches!(c, 'p' | 'i' | 'e')))
    })
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
            denies_as_a_read_target(target).then(|| deny_reason(target, "the `Read` tool"))
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
        && denies_as_a_read_target(path)
    {
        return Some(deny_reason(path, "the `Grep` tool"));
    }
    let glob = string_field(tool_input, "glob")?;
    denies_as_a_read_target(glob).then(|| deny_reason(glob, "a `Grep` glob"))
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

/// Extensions transparent to a GLOB but never to a literal name.
///
/// Why: #7266 round 6. `.json` is the tail of the two-part core `.tfvars.json`,
/// so `*.json` matches that core and `ls *.json` denied the moment the word
/// scan stopped cutting at the wildcard. That is round 4's trade again — taxing
/// every ordinary extension search is worse than the leak it prevents — so a
/// `.json` GLOB is an ordinary tree search here. A literal `credentials.json`
/// or `terraform.tfvars.json` is a named target and still denies, which is why
/// `json` cannot simply join [`TRANSPARENT_SOURCE_EXTENSIONS`].
/// Test: `allows_a_json_glob_but_not_a_json_credential_file`.
const GLOB_ONLY_TRANSPARENT_EXTENSIONS: &[&str] = &["json"];

/// Whether one already-brace-expanded name or glob is a secret-bearing target.
fn names_a_secret(candidate: &str) -> bool {
    if selects_every_name(candidate) || has_transparent_source_extension(candidate) {
        return false;
    }
    let is_glob = candidate.bytes().any(|b| b == b'*' || b == b'?');
    if is_glob && has_extension_in(candidate, GLOB_ONLY_TRANSPARENT_EXTENSIONS) {
        return false;
    }
    // A `?` is the one metacharacter the shared name matcher does not
    // implement, so a candidate carrying one is compared against the full
    // denylist entries as well (#7266 round 6).
    secret_pattern_overlaps(candidate)
        || (candidate.contains('?') && any_pattern_overlaps(candidate))
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
    has_extension_in(basename, TRANSPARENT_SOURCE_EXTENSIONS)
}

/// Whether `basename`'s extension, lowercased, is one of `extensions`.
fn has_extension_in(basename: &str, extensions: &[&str]) -> bool {
    Path::new(basename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| extensions.contains(&e.as_str()))
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
         `rm` and `git add`/`rm`/`mv`/`status` may name such a file, and `git add` loses that \
         grant under `-p`/`-i`/`-e`. There is no flag that buys an exception: run the tool so \
         that it picks the file up itself without you writing the name (`docker compose up` \
         reads `./.env`, `terraform apply` reads `./terraform.tfvars`), or ask the operator to \
         read it for you."
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
        // Round 6, critic CRITICAL 1: a glob the shell expands onto the file.
        // Every one of these ALLOWED on 71ad20438 — `is_path_byte` cut at the
        // wildcard and left a remainder matching nothing.
        "cat .en?",
        "cat .e*",
        "cat ./.*",
        "cat id_rs?",
        "cat *.p?m",
        "cat id_[r]sa",
        "cat ./.*rc",
        "sed -n '1,5p' terraform.tfvar?",
        "head -n 2 *.tfstat?",
        // Round 6, critic CRITICAL 2: the name reassembled by quoting or
        // escaping, which the raw byte scan cut into harmless halves.
        "cat '.en''v'",
        "cat .e\\nv",
        "cat \".en\"v",
        "cat 'terraform'.tfvars",
        "sed -n '1,5p' \"terra\"form.tfvars",
        // Round 6, critic CRITICAL 3: `git add` in a mode that prints the diff.
        "git add -p .env",
        "git add --patch .env",
        "git add -i .env",
        "git add --interactive .env",
        "git add -e .env",
        "git add --edit terraform.tfvars",
        // Round 6, critic HIGH: the pattern skip must not reach a file operand.
        "grep -r SECRET .env",
        "grep -o pattern .env",
        "rg . .env",
        "grep -e '\\.env' .env",
        "grep --regexp=SECRET .env",
        "grep \"$(cat .env)\" src/",
        "git grep SECRET .env",
        // Round 6, critic HIGH: a `.pub` name carrying another family's core,
        // and a private key spelled with a `.pub` somewhere other than the end.
        "cat id_rsa_credentials.pub",
        "cat id_rsa.pub.bak",
        // Round 7, critic CRITICAL 1's other half: an expansion whose OPERAND
        // is the secret. Reading the span as text and failing OPEN on it would
        // reopen every one of these. `cat ${F-.env}` ALLOWED on f40805985 —
        // the bare `-` operator glued onto the name and matched no pattern.
        "cat \"${F:-.env}\"",
        "cat \"${F:=id_rsa}\"",
        "cat \"${F:?terraform.tfvars}\"",
        "cat \"${F:+server.pem}\"",
        "cat ${F-.env}",
        "cat \"${F#*/}.env\"",
        "cat \"${DIR:-/etc}/.env\"",
        "rm ${SECRET:-.env}",
    ];

    /// Bypasses this rule DOES NOT catch, pinned so a change to one is visible.
    ///
    /// Why: #7266 round 7 — a filename the command COMPUTES never appears as
    /// literal path text, so a word scan cannot see it. The class is unbounded
    /// (every encoder, every printf escape, every arithmetic join), which is
    /// the shape four rounds of enumeration already failed against, so it is
    /// documented beside the variable-indirection residual rather than chased.
    /// These rows ALLOW today. A row that starts denying is not a regression —
    /// it is a residual that closed, and this list is what makes that visible.
    /// Test: `the_documented_residuals_still_allow`.
    const DOCUMENTED_RESIDUALS: &[&str] = &["cat $(printf '\\056env')"];

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
        // Round 6: the glob bytes are kept in the word now, so every ordinary
        // extension glob has to be re-proved through the pattern screen.
        "cat *.toml",
        "ls *.json",
        "rg foo src/*.rs",
        "ls -la target/*",
        "rm -rf build/*",
        "cat ~/.bashrc",
        "cat [a-z]*.rs",
        // Round 6, critic CRITICAL 3: the `git add` spellings that stage
        // without printing.
        "git add -A .env",
        "git add -u .env",
        "git add -f .env",
        // Round 6, critic HIGH: a secret NAME written as a search pattern.
        // Finding where a file is referenced prints no byte of it.
        "grep -rn \"\\.env\" docs/",
        "rg 'id_rsa' --type md",
        "grep -rn '\\.pem' README.md",
        "grep -rn terraform.tfvars docs/",
        "rg '\\.tfstate' --glob '*.md'",
        "git grep '\\.env' -- docs/",
        // Round 6, critic HIGH: an SSH PUBLIC key is published by definition.
        "cat id_rsa.pub",
        "cat ~/.ssh/id_rsa.pub",
        "ssh-copy-id -i id_rsa.pub host",
        "cat id_ed25519.pub",
        "cat id_ecdsa.pub",
        "cat id_dsa.pub",
        // Round 7, critic CRITICAL 1: a parameter expansion carrying an
        // operator. Every one of these DENIED on f40805985 with no secret
        // named — `is_path_byte` cut at the operator and left `{VAR` behind.
        "mkdir -p \"${OUT_DIR:-build}\"",
        "echo \"${1:-default}\"",
        "docker run -e \"FOO=${FOO:-bar}\" img",
        "cp \"${SRC%.rs}.bak\" x",
        "echo ${PATH#/usr}",
        "echo ${HOME##*/}",
        "echo \"${VERSION:=0.1.0}\"",
        "echo \"${NAME:?name required}\"",
        "echo \"${BRANCH:+--branch $BRANCH}\"",
        "echo ${LINE:2:3}",
        "echo ${FILE//old/new}",
        "echo ${VAR}",
        "echo $VAR",
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

    // --- #7266 round 6 -----------------------------------------------------

    #[test]
    fn denies_a_glob_that_expands_onto_a_secret_file() {
        // Critic CRITICAL 1. `is_path_byte` cut at `*`, `?`, `[` and `]`, so
        // `cat .en?` reached the classifier as `.en` and ALLOWED. Measured
        // live against the round-5 binary (71ad20438).
        for command in [
            "cat .en?",
            "cat .e*",
            "cat ./.*",
            "cat id_rs?",
            "cat *.p?m",
            "cat id_[r]sa",
        ] {
            let reason = eval(command).unwrap_or_else(|| panic!("`{command}` must deny"));
            assert!(reason.contains("#7266"), "{reason}");
        }
        // `.*rc` is DENIED and that is the pinned answer: the glob selects
        // `.netrc` as readily as `.bashrc`, and this rule screens what a
        // pattern can REACH, exactly as it does for `*.env`. Naming the file
        // an agent actually wants keeps working.
        assert!(eval("cat ./.*rc").is_some());
        assert_eq!(eval("cat ~/.bashrc"), None);
        // The Grep glob arm answers the same way for a bracket class.
        let bracket = serde_json::json!({"pattern": ".", "glob": "id_[r]sa"});
        assert!(evaluate_secret_file_read_tool("Grep", Some(&bracket)).is_some());
    }

    #[test]
    fn allows_a_json_glob_but_not_a_json_credential_file() {
        // Keeping the wildcard in the word made `*.json` match the two-part
        // core `.tfvars.json`, so every json search denied. A GLOB is a tree
        // search; a literal name is a target.
        for command in [
            "ls *.json",
            "cat *.json",
            "grep -rn TODO --include=*.json .",
        ] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
        for command in [
            "cat credentials.json",
            "cat terraform.tfvars.json",
            "cat secrets.json",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        let glob = serde_json::json!({"pattern": "TODO", "path": "/repo", "glob": "*.json"});
        assert_eq!(evaluate_secret_file_read_tool("Grep", Some(&glob)), None);
    }

    #[test]
    fn a_single_character_wildcard_reaches_the_full_denylist_entry() {
        // The core-overlap screen reduces to "does the candidate match this
        // core", so a candidate whose literal part sits OUTSIDE the core was
        // unreachable: `terraform.tfvar?` names the file and matched nothing.
        for command in [
            "sed -n '1,5p' terraform.tfvar?",
            "cat terraform.tfstat?",
            "cat server.pe?",
            "cat vault.kdb?",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        // An ordinary `?` glob still allows — the full-entry comparison is a
        // two-sided overlap, not a substring test.
        for command in ["ls file?.txt", "ls core.?", "cat notes?.md", "ls ??.rs"] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
    }

    #[test]
    fn normalize_bracket_classes_collapses_a_class_and_drops_a_stray() {
        assert_eq!(normalize_bracket_classes("id_[r]sa"), "id_?sa");
        assert_eq!(normalize_bracket_classes("[a-z]*.rs"), "?*.rs");
        // A stray bracket is dropped, so a malformed class cannot shield the
        // name around it.
        assert_eq!(normalize_bracket_classes(".env]"), ".env");
        assert_eq!(normalize_bracket_classes("[.env"), ".env");
        // An empty class carries no character to stand in for.
        assert_eq!(normalize_bracket_classes("a[]b"), "ab");
        assert_eq!(normalize_bracket_classes("README.md"), "README.md");
    }

    #[test]
    fn denies_a_name_reassembled_by_quoting_or_escaping() {
        // Critic CRITICAL 2. The word scan ran on raw text even when the
        // segment lexed, so `'` and `\` cut the name into harmless halves.
        for command in [
            "cat '.en''v'",
            "cat .e\\nv",
            "cat \".en\"v",
            "cat 'terraform'.tfvars",
        ] {
            let reason = eval(command).unwrap_or_else(|| panic!("`{command}` must deny"));
            assert!(reason.contains("#7266"), "{reason}");
        }
        // The raw scan is still what answers for a segment no lexer can read.
        assert!(shlex::split("awk '{print} terraform.tfvars").is_none());
        assert!(eval("awk '{print} terraform.tfvars").is_some());
    }

    #[test]
    fn denies_git_add_in_a_content_revealing_mode() {
        // Critic CRITICAL 3. `SAFE_GIT_SUBCOMMANDS` granted `add` on the
        // subcommand name alone; `-p` walks the file's diff in the transcript.
        for command in [
            "git add -p .env",
            "git add --patch .env",
            "git add -i .env",
            "git add --interactive .env",
            "git add -e .env",
            "git add --edit terraform.tfvars",
            "git -C /repo add -p terraform.tfvars",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        // The staging spellings that print nothing keep the grant.
        for command in [
            "git add .env.example",
            "git add -A .env",
            "git add -u .env",
            "git add -f .env",
            "git add -n .env",
        ] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
    }

    #[test]
    fn allows_a_secret_name_written_as_a_search_pattern() {
        // Critic HIGH. Searching the tree FOR the string prints no byte of the
        // file, and it is how an agent finds where the file is referenced.
        for command in [
            "grep -rn \"\\.env\" docs/",
            "rg 'id_rsa' --type md",
            "grep -rn '\\.pem' README.md",
            "grep -rn terraform.tfvars docs/",
            "git grep '\\.env' -- docs/",
        ] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
        // Every member of the program list, on its own idiom.
        for program in PATTERN_FIRST_SEARCH_PROGRAMS {
            let command = format!("{program} '\\.env' docs/");
            assert_eq!(eval(&command), None, "`{command}` must allow");
        }
    }

    #[test]
    fn denies_a_secret_file_operand_of_a_search_program() {
        // The other half: only the FIRST positional argument is a pattern.
        for command in [
            "grep -r SECRET .env",
            "grep -o pattern .env",
            "rg . .env",
            "grep -rn TODO docs/ .env",
            "git grep SECRET .env",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        // A flag that supplies the pattern itself withdraws the skip, so every
        // positional argument is read as a file again.
        for command in [
            "grep -e '\\.env' .env",
            "grep --regexp=SECRET .env",
            "grep -f patterns.txt .env",
            "grep -ne '\\.env' .env",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        // A nested command withdraws it too — the "pattern" can run anything.
        assert!(eval("grep \"$(cat .env)\" src/").is_some());
        assert!(eval("grep `cat .env` src/").is_some());
    }

    #[test]
    fn allows_reading_an_ssh_public_key() {
        // Critic HIGH. A public key is published by definition; denying a read
        // of it costs real work and protects nothing.
        for command in [
            "cat id_rsa.pub",
            "cat ~/.ssh/id_rsa.pub",
            "ssh-copy-id -i id_rsa.pub host",
            "cat id_ed25519.pub",
            "cat id_ecdsa.pub",
            "cat id_dsa.pub",
        ] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
        // Every family, through the predicate itself.
        for prefix in SSH_KEY_FAMILY_PREFIXES {
            assert!(is_ssh_public_key_name(&format!("{prefix}.pub")));
            assert!(!is_ssh_public_key_name(prefix));
        }
        // The `Read` tool answers the same way.
        let pubkey = serde_json::json!({"file_path": "/home/u/.ssh/id_rsa.pub"});
        assert_eq!(evaluate_secret_file_read_tool("Read", Some(&pubkey)), None);
    }

    #[test]
    fn denies_a_private_key_and_a_pub_name_carrying_another_family() {
        for command in [
            "cat id_rsa",
            "cat ~/.ssh/id_rsa",
            "cat id_rsa.pub.bak",
            "cat id_rsa_credentials.pub",
            "cat secrets.pub/id_rsa",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
        // The glob still denies: `id_rsa*` reaches the PRIVATE half too.
        let glob = serde_json::json!({"pattern": ".", "glob": "id_rsa*"});
        assert!(evaluate_secret_file_read_tool("Grep", Some(&glob)).is_some());
        // The #7122 copy rule is untouched by the exemption — it calls
        // `is_secret_read_target`, which still answers for a `.pub` name.
        assert!(is_secret_read_target("id_rsa.pub"));
    }

    #[test]
    fn the_deny_text_advertises_no_flag_escape() {
        // Critic HIGH + MEDIUM: round 5's reason offered
        // `--env-file`/`-var-file`/`-state` and no such escape existed.
        // Round 6 removed the claim rather than build it.
        let reason = eval("cat .env").expect("denies");
        for phantom in ["--env-file", "-var-file", "-state"] {
            assert!(
                !reason.contains(phantom),
                "the deny text still advertises `{phantom}`: {reason}"
            );
        }
        assert!(
            reason.contains("no flag that buys an exception"),
            "{reason}"
        );
    }

    #[test]
    fn git_show_of_a_committed_secret_is_denied_not_residual() {
        // Round 5's module doc listed this as a gap. `:` is not a path byte,
        // so the pathspec surfaces as its own word and `show` is not safe.
        for command in [
            "git show HEAD:terraform.tfvars",
            "git show main:.env",
            "git show HEAD~2:infra/terraform.tfvars",
        ] {
            assert!(eval(command).is_some(), "`{command}` must deny");
        }
    }

    // --- #7266 round 7 -----------------------------------------------------

    #[test]
    fn allows_a_parameter_expansion_that_names_no_secret() {
        // Critic CRITICAL 1. `${VAR:-x}` and its siblings each left an
        // unmatched `{VAR` for `expand_brace_alternatives`, which fails closed,
        // so the guard refused an ordinary command with no secret in it.
        // Measured live against the round-6 binary (f40805985).
        for command in [
            "mkdir -p \"${OUT_DIR:-build}\"",
            "echo \"${1:-default}\"",
            "docker run -e \"FOO=${FOO:-bar}\" img",
            "cp \"${SRC%.rs}.bak\" x",
            "echo ${PATH#/usr}",
            "echo ${HOME##*/}",
            "echo \"${VERSION:=0.1.0}\"",
            "echo \"${NAME:?name required}\"",
            "echo \"${BRANCH:+--branch $BRANCH}\"",
            "echo ${LINE:2:3}",
            "echo ${FILE//old/new}",
            "echo ${PREFIX^^}",
            "echo \"${A:-${B:-fallback}}\"",
        ] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
        // `${VAR}` and `$VAR` are unchanged by round 7 — they carry no
        // operator, so round 6 already read them as the bare name.
        for command in ["echo ${VAR}", "echo $VAR", "cat ${F}", "cat $F"] {
            assert_eq!(eval(command), None, "`{command}` must allow");
        }
        // Brace ALTERNATION is a different shape and keeps its own answer: a
        // group the shared expander cannot resolve still fails closed.
        assert_eq!(eval("cp notes.{md,txt} out/"), None);
        assert!(eval("cat secret.{tfvars,bak}").is_some());
    }

    #[test]
    fn denies_a_parameter_expansion_whose_operand_names_a_secret() {
        // The other half. Failing OPEN on the span — skipping it as "not a
        // path" — would reopen every one of these, which is what the round-6
        // critic warned a naive fix would do.
        for command in [
            "cat \"${F:-.env}\"",
            "cat \"${F:=id_rsa}\"",
            "cat \"${F:?terraform.tfvars}\"",
            "cat \"${F:+server.pem}\"",
            "cat \"${F#*/}.env\"",
            "cat \"${DIR:-/etc}/.env\"",
            "cat \"${A:-${B:-.env}}\"",
            "rm ${SECRET:-.env}",
        ] {
            let reason = eval(command).unwrap_or_else(|| panic!("`{command}` must deny"));
            assert!(reason.contains("#7266"), "{reason}");
        }
        // `cat ${F-.env}` ALLOWED on f40805985: the bare `-` operator glued to
        // the operand, and `-.env` matches no pattern.
        assert!(eval("cat ${F-.env}").is_some());
        // A parameter whose NAME is itself a secret-shaped filename keeps the
        // answer round 6 gave it.
        assert!(eval("echo ${id_rsa}").is_some());
    }

    #[test]
    fn splits_a_parameter_expansion_into_its_name_and_operand() {
        assert_eq!(
            split_parameter_expansion("OUT_DIR:-build"),
            ("OUT_DIR", "build")
        );
        assert_eq!(split_parameter_expansion("F-.env"), ("F", ".env"));
        assert_eq!(split_parameter_expansion("F:=id_rsa"), ("F", "id_rsa"));
        assert_eq!(split_parameter_expansion("HOME##*/"), ("HOME", "*/"));
        assert_eq!(split_parameter_expansion("SRC%.rs"), ("SRC", ".rs"));
        assert_eq!(split_parameter_expansion("LINE:2:3"), ("LINE", "2:3"));
        assert_eq!(split_parameter_expansion("VAR"), ("VAR", ""));
        // The length and indirection sigils sit before the NAME, not before a
        // word, so neither is read as an operator.
        assert_eq!(split_parameter_expansion("#VAR"), ("VAR", ""));
        assert_eq!(split_parameter_expansion("!VAR"), ("VAR", ""));
        // A special parameter is one character and no more.
        assert_eq!(split_parameter_expansion("@"), ("@", ""));
        assert_eq!(split_parameter_expansion(""), ("", ""));
        // Nesting resolves to the OUTER close, so the inner span is the
        // operand and gets rewritten in its turn.
        let chars: Vec<char> = "{A:-${B}}".chars().collect();
        assert_eq!(matching_close_brace(&chars, 0), Some(8));
        assert_eq!(
            matching_close_brace(&"{VAR".chars().collect::<Vec<_>>(), 0),
            None
        );
        // An unbalanced `${` is left as it stands, which keeps it failing
        // closed exactly as round 6 left it.
        assert_eq!(rewrite_parameter_expansions("cat ${VAR"), "cat ${VAR");
        assert!(eval("cat ${VAR").is_some());
    }

    #[test]
    fn the_documented_residuals_still_allow() {
        // These rows are the module doc's named residuals, asserted so the
        // gap is visible in the suite rather than only in prose. A row that
        // begins to deny means a residual closed — update the doc, not the
        // rule.
        for command in DOCUMENTED_RESIDUALS {
            assert_eq!(
                eval(command),
                None,
                "documented residual `{command}` now denies — update the module doc"
            );
        }
    }

    #[test]
    fn command_basename_strips_a_process_substitution_wrapper() {
        assert_eq!(command_basename("<(cat"), "cat");
        assert_eq!(command_basename("/repo/infra/.env)"), ".env");
        assert_eq!(command_basename(".env)"), ".env");
        assert_eq!(command_basename("README.md"), "README.md");
    }
}
