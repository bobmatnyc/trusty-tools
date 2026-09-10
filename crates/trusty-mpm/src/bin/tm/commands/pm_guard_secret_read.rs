//! `tm hook --pm-guard` — line-range and partial READS of a secret-bearing
//! file (issue #7266).
//!
//! Why: a `local-ops` agent told "never print tfvars values" printed an ngrok
//! authtoken by running `sed -n '38,46p' terraform.tfvars` to locate an
//! insertion line. The brief forbade `cat` by implication, and a line-range
//! print is not `cat`, so nothing stopped it — the sibling
//! [`super::pm_guard_bash`] `secret_file_copy` rule screens only a `cp`/`mv` of
//! such a file INTO a worktree, never a read of one in place. Issue #7266
//! records this as the fifth exposure of the same class through a different
//! command each time, which is why this rule keys on the CLASS of verb that
//! prints file bytes rather than on the spellings already seen.
//!
//! What: [`evaluate_secret_file_read_command`] denies a Bash command when a
//! content-printing verb ([`CONTENT_PRINTING_VERBS`]), a shell input
//! redirection, or an inline interpreter program
//! ([`INLINE_PROGRAM_INTERPRETERS`]) names a file operand whose basename is
//! secret-bearing, and [`evaluate_secret_file_read_tool`] denies a `Read` tool
//! call on the same class of path — with or without an `offset`/`limit` range,
//! since a partial read prints values exactly as a whole one does.
//! [`evaluate_secret_file_read_command`] fails CLOSED: a segment this guard
//! cannot lex still has its words examined, and a secret-shaped word denies.
//!
//! The file classifier is NOT a second list — it is
//! [`is_secret_bearing_source`], the one
//! `pm_guard_bash::secret_file_copy` already owns, read at this rule's scope by
//! [`is_secret_read_target`]. The only narrowing is
//! [`has_transparent_source_extension`]: that list's three name-SUBSTRING
//! patterns (`*credentials*`, `*secrets*`, `token*`) match 24 ordinary tracked
//! files in this repository (`credentials.rs`, `tokens.css`, `secrets.rs`,
//! `token.rs`), and a read guard that refuses `cat crates/…/credentials.rs` is
//! a rule agents route around. Every extension-typed family in that list
//! (`*.tfvars`, `.env*`, `*.pem`, `*.key`, `id_rsa*`, `*.p12`, …) is untouched
//! by the narrowing, because none of those spellings ends in a source or
//! markup extension.
//!
//! The one sanctioned read stays allowed: [`is_key_name_only_grep`] permits a
//! `grep -o` whose pattern is anchored at line start and can match only
//! identifier characters (`grep -o '^key_[a-z_]*' file`), which issue #7266's
//! closure names as the safe-read pattern. Such a pattern cannot cross the `=`
//! that separates a key from its value, so no value can reach the transcript.
//!
//! Residual bypasses, deliberate and documented rather than silently allowed:
//! a `.yml`/`.yaml` credential manifest read by name (`secrets.yaml`) is
//! carved out with the rest of the markup extensions; `git show
//! HEAD:terraform.tfvars` prints a COMMITTED copy through a verb this rule has
//! no opinion about; and a file operand that reaches the verb only through a
//! variable (`sed -n 1,5p "$F"`) is not resolved here — this rule reads
//! basenames, not the filesystem.
//!
//! Test: `denies_the_reported_sed_line_range`, `denies_a_tail_of_a_dotenv`,
//! `denies_a_grep_of_a_tfvars_json`, `denies_a_read_tool_call_with_a_range`,
//! `allows_a_sed_line_range_of_an_ordinary_file`, `allows_cat_of_a_manifest`,
//! `allows_a_tfvars_mention_that_is_not_a_file_operand`, and the rest of this
//! module's `tests` submodule. The rule is proved WIRED — a call site this
//! module's own tests could not miss — end to end through the real binary by
//! `pm_guard_denies_a_line_range_read_of_a_secret_bearing_file`,
//! `pm_guard_denies_a_read_tool_call_on_a_secret_bearing_file` and
//! `pm_guard_still_allows_ordinary_reads_and_non_operand_mentions` in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::Path;

use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard_bash::{is_secret_bearing_source, split_shell_segments};

/// Verbs that print a named file's bytes to the transcript.
///
/// Why: the incident's `sed -n` is one spelling of one behaviour — reading a
/// file and printing part of it. Naming the behaviour's whole verb class is
/// what stops the next exposure arriving through `head`, `cut` or `xxd`
/// (issue #7266 counts five spellings already).
/// What: matched against the BASENAME of any token in a segment's argv, so a
/// wrapper (`sudo cat`, `xargs head`) does not hide the verb.
/// Test: `denies_every_content_printing_verb`.
const CONTENT_PRINTING_VERBS: &[&str] = &[
    "cat", "tac", "head", "tail", "sed", "awk", "gawk", "mawk", "nawk", "cut", "grep", "egrep",
    "fgrep", "rg", "less", "more", "bat", "nl", "strings", "od", "xxd", "hexdump", "paste", "fold",
    "rev", "column", "pr",
];

/// Verbs whose FIRST positional argument is a pattern or program, not a file.
///
/// Why: `grep -n TOKEN app.tfvars` and `sed -n '12,14p' f` both carry a
/// non-file first positional; treating it as a file operand would let
/// `grep .env README.md` deny on its pattern.
/// Test: `allows_a_grep_whose_pattern_looks_like_a_secret_name`.
const PATTERN_LEADING_VERBS: &[&str] = &[
    "sed", "awk", "gawk", "mawk", "nawk", "grep", "egrep", "fgrep", "rg",
];

/// The `grep` family, whose key-name-only carve-out [`is_key_name_only_grep`] decides.
const GREP_VERBS: &[&str] = &["grep", "egrep", "fgrep", "rg"];

/// Flags whose FOLLOWING token is a pattern or script, not a file operand.
///
/// Why: when one of these is present the first positional is already a file,
/// so [`file_operands`] must not drop it — `sed -n -e '1,5p' terraform.tfvars`
/// would otherwise skip the very file it reads.
const PATTERN_FLAGS: &[&str] = &["-e", "--expression", "--regexp", "-f", "--file"];

/// Interpreters that run their `-c`/`-e` argument as an inline program.
///
/// Why: `python -c 'print(open("terraform.tfvars").read())'` prints the file
/// through a verb no filename-operand rule sees — the path is a substring of
/// one program token. Issue #7266 names this shape explicitly.
/// Test: `denies_an_inline_python_program_that_opens_a_secret`.
const INLINE_PROGRAM_INTERPRETERS: &[&str] = &[
    "python", "python2", "python3", "ruby", "perl", "node", "php", "deno",
];

/// File extensions whose content is source or markup, never a credential value.
///
/// Why: see the module doc — the shared classifier's `*credentials*`,
/// `*secrets*` and `token*` patterns match 24 tracked files in this repository
/// alone. Every extension-typed secret family in that list is unaffected,
/// because none of `*.tfvars`, `.env*`, `*.pem`, `*.key`, `id_rsa*`, `*.p12`,
/// `*.pfx`, `*.jks`, `*.kdbx`, `*.ovpn` or `.netrc` ends in one of these.
/// `.json` and `.toml` are deliberately ABSENT: `credentials.json` and
/// `secrets.toml` are real credential-store spellings, and `*.tfvars.json` is
/// itself a JSON secret.
/// Test: `allows_reading_ordinary_source_files_that_match_a_substring_pattern`.
const TRANSPARENT_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "css", "scss", "sass", "svelte", "vue", "py",
    "rb", "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "md", "mdx", "html", "htm",
    "sh", "bash", "zsh", "yml", "yaml", "snap", "baseline", "lock",
];

/// Classify any tool call for a read of a secret-bearing file: `Some(reason)`
/// denies, `None` allows.
///
/// Why: the one entry point `pm_guard` calls, so a Bash command and a `Read`
/// tool call are decided by the same rule and reported with the same reason —
/// the verb that prints the bytes is the leak, whichever surface issues it.
/// What: routes `Bash` to [`evaluate_secret_file_read_command`] over its
/// `command` string and `Read` to [`evaluate_secret_file_read_tool`] over its
/// `file_path`; every other tool allows.
/// Test: see the module doc's test list.
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

/// Classify a Bash command for a read of a secret-bearing file: `Some(reason)`
/// denies, `None` allows.
///
/// Why: the one Bash entry point `pm_guard` calls, kept to the same shape as
/// the sibling ABSOLUTE guards so the policy underneath stays testable.
/// What: walks [`split_shell_segments`] (which already descends into an `sh -c`
/// wrapper), and for each segment checks, in order, an input redirection, an
/// inline interpreter program, then a content-printing verb's file operands.
/// An unlexable segment falls back to a word scan and denies on a secret-shaped
/// word rather than skipping.
/// Test: see the module doc's test list.
pub(crate) fn evaluate_secret_file_read_command(command: &str) -> Option<String> {
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        // #7266: fail closed — a segment this guard cannot lex still names its
        // words, and one of them being secret-shaped is enough to refuse.
        let Some(argv) = shlex::split(trimmed) else {
            if let Some(target) = first_secret_word(trimmed) {
                return Some(deny_reason(&target, "a command this guard cannot parse"));
            }
            continue;
        };
        if let Some(target) = redirection_operand(&argv) {
            return Some(deny_reason(&target, "shell input redirection"));
        }
        let verb = first_command_token(trimmed).map(command_basename);
        if let Some(verb) = verb.as_deref()
            && INLINE_PROGRAM_INTERPRETERS.contains(&verb)
            && has_inline_program_flag(&argv)
            && let Some(target) = first_secret_word(trimmed)
        {
            return Some(deny_reason(&target, &format!("an inline `{verb}` program")));
        }
        let Some(verb_idx) = argv
            .iter()
            .position(|tok| CONTENT_PRINTING_VERBS.contains(&command_basename(tok).as_str()))
        else {
            continue;
        };
        let printing_verb = command_basename(&argv[verb_idx]);
        let tail = &argv[verb_idx + 1..];
        let Some(target) = file_operands(&printing_verb, tail)
            .into_iter()
            .find(|operand| is_secret_read_target(operand))
        else {
            continue;
        };
        if is_key_name_only_grep(&printing_verb, tail) {
            continue;
        }
        return Some(deny_reason(&target, &format!("a `{printing_verb}` read")));
    }
    None
}

/// Classify a `Read` tool call for a secret-bearing target: `Some(reason)`
/// denies, `None` allows.
///
/// Why: the harness's own `Read` tool takes `offset`/`limit`, which is the
/// exact line-range shape issue #7266 reports — and a `Read` with no range is a
/// strictly larger exposure, so BOTH deny. A tool call carries no shell to lex,
/// so this arm reads `file_path` directly.
/// What: `Some(reason)` when `tool_name` is `Read` and its `file_path`'s
/// basename satisfies [`is_secret_read_target`]; `None` for every other tool
/// and for a call with no readable `file_path`.
/// Test: `denies_a_read_tool_call_with_a_range`,
/// `denies_a_read_tool_call_without_a_range`, `allows_a_read_of_an_ordinary_file`,
/// `allows_every_non_read_tool`.
pub(crate) fn evaluate_secret_file_read_tool(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<String> {
    if tool_name != "Read" {
        return None;
    }
    let target = tool_input
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())?;
    if !is_secret_read_target(target) {
        return None;
    }
    Some(deny_reason(target, "the `Read` tool"))
}

/// Whether `path`'s basename is secret-bearing at THIS rule's scope.
///
/// Why: see the module doc — the shared classifier is read as-is, then narrowed
/// by [`has_transparent_source_extension`] so an ordinary source file that
/// merely carries `token`/`secrets`/`credentials` in its name stays readable.
/// Test: `allows_reading_ordinary_source_files_that_match_a_substring_pattern`,
/// `denies_every_extension_typed_secret_family`.
fn is_secret_read_target(path: &str) -> bool {
    let basename = command_basename(path);
    is_secret_bearing_source(&basename) && !has_transparent_source_extension(&basename)
}

/// Whether `basename` ends in one of [`TRANSPARENT_SOURCE_EXTENSIONS`].
fn has_transparent_source_extension(basename: &str) -> bool {
    Path::new(basename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| TRANSPARENT_SOURCE_EXTENSIONS.contains(&e.as_str()))
}

/// The basename of a command or path token, with a leading `\` quote removed.
fn command_basename(token: &str) -> String {
    let token = token.strip_prefix('\\').unwrap_or(token);
    Path::new(token)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(token)
        .to_string()
}

/// The file operands of a content-printing verb's argument tail, in order.
///
/// Why: `head -n 3 .env.production` and `grep -n TOKEN app.tfvars` must both
/// resolve to the file they read and nothing else — a flag carries no path,
/// and a pattern-leading verb's first positional is a pattern, not a file.
/// What: skips flags (honoring a `--` end-of-flags marker), consumes the value
/// of a [`PATTERN_FLAGS`] entry, and drops the first positional for a
/// [`PATTERN_LEADING_VERBS`] entry only when no pattern flag already supplied
/// the pattern.
/// Test: `file_operands_drops_a_leading_pattern`,
/// `file_operands_keeps_the_file_when_a_pattern_flag_is_present`.
fn file_operands(verb: &str, tail: &[String]) -> Vec<String> {
    let mut positional = Vec::new();
    let mut positional_only = false;
    let mut has_pattern_flag = false;
    let mut skip_value = false;
    for tok in tail {
        if skip_value {
            skip_value = false;
            has_pattern_flag = true;
            continue;
        }
        if !positional_only && tok == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && tok.starts_with('-') && tok.len() > 1 {
            if PATTERN_FLAGS.contains(&tok.as_str()) {
                skip_value = true;
            } else if PATTERN_FLAGS.iter().any(|f| {
                tok.starts_with(&format!("{f}="))
                    || (!f.starts_with("--") && tok.starts_with(f) && tok.len() > f.len())
            }) {
                has_pattern_flag = true;
            }
            continue;
        }
        positional.push(tok.clone());
    }
    if !has_pattern_flag && PATTERN_LEADING_VERBS.contains(&verb) && !positional.is_empty() {
        positional.remove(0);
    }
    positional
}

/// The operand of a `<` input redirection in `argv`, if any.
///
/// Why: `while read l; do …; done < .env` prints a secret through no
/// content-printing verb at all — the shell opens the file.
/// What: answers for both the separated (`< .env`) and attached (`<.env`)
/// spellings, and only when the operand is secret-shaped.
/// Test: `denies_shell_input_redirection_from_a_secret`.
fn redirection_operand(argv: &[String]) -> Option<String> {
    let mut expect_operand = false;
    for tok in argv {
        if expect_operand {
            expect_operand = false;
            if is_secret_read_target(tok) {
                return Some(tok.clone());
            }
            continue;
        }
        let redirect = tok
            .strip_suffix('<')
            .is_some_and(|fd| fd.is_empty() || fd.chars().all(|c| c.is_ascii_digit()));
        if redirect {
            expect_operand = true;
            continue;
        }
        if let Some((fd, operand)) = tok.split_once('<')
            && (fd.is_empty() || fd.chars().all(|c| c.is_ascii_digit()))
            && !operand.is_empty()
            && is_secret_read_target(operand)
        {
            return Some(operand.to_string());
        }
    }
    None
}

/// Whether `argv` carries an inline-program flag (`-c`, `-e`, or a cluster
/// containing one).
fn has_inline_program_flag(argv: &[String]) -> bool {
    argv.iter().any(|tok| tok == "-c" || tok == "-e")
}

/// The first word in `segment` whose basename is secret-shaped, if any.
///
/// Why: an inline interpreter program and an unlexable segment both hide a path
/// INSIDE a token (`open("terraform.tfvars")`), where no operand rule reaches
/// it. Splitting on every character a path cannot contain surfaces it.
/// What: cuts `segment` at any byte outside the path-ish set
/// (alphanumerics and `. _ - / ~ + @ { } ,`) and returns the first resulting
/// word [`is_secret_read_target`] answers for.
/// Test: `denies_an_inline_python_program_that_opens_a_secret`,
/// `denies_an_unlexable_segment_that_names_a_secret`.
fn first_secret_word(segment: &str) -> Option<String> {
    segment
        .split(|c: char| {
            !(c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@' | '{' | '}' | ','))
        })
        .filter(|w| !w.is_empty())
        .find(|w| is_secret_read_target(w))
        .map(str::to_string)
}

/// Whether a `grep` invocation can print only KEY NAMES, never a value.
///
/// Why: issue #7266's closure names `grep -o '^key_[a-z_]*' file` as the safe
/// read, and a guard that refused the pattern it recommends would just be
/// routed around.
/// What: requires `-o`/`--only-matching` (so only the matched span prints) AND
/// a pattern that [`is_key_name_only_pattern`] proves cannot cross the `=`
/// separating a key from its value.
/// Test: `allows_the_documented_key_name_only_grep`,
/// `denies_a_grep_o_whose_pattern_can_match_a_value`.
fn is_key_name_only_grep(verb: &str, tail: &[String]) -> bool {
    if !GREP_VERBS.contains(&verb) {
        return false;
    }
    let mut only_matching = false;
    let mut pattern: Option<&str> = None;
    let mut first_positional: Option<&str> = None;
    let mut expect_pattern = false;
    let mut positional_only = false;
    for tok in tail {
        if expect_pattern {
            pattern = Some(tok);
            expect_pattern = false;
            continue;
        }
        if !positional_only && tok == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && tok.starts_with('-') && tok.len() > 1 {
            if tok == "--only-matching" {
                only_matching = true;
            } else if tok == "-e" || tok == "--regexp" {
                expect_pattern = true;
            } else if let Some(rest) = tok.strip_prefix("--regexp=") {
                pattern = Some(rest);
            } else if !tok.starts_with("--") && tok.contains('o') {
                only_matching = true;
            }
            continue;
        }
        if first_positional.is_none() {
            first_positional = Some(tok);
        }
    }
    let Some(pattern) = pattern.or(first_positional) else {
        return false;
    };
    only_matching && is_key_name_only_pattern(pattern)
}

/// Whether a regex can match only identifier characters anchored at line start.
///
/// Why: such a match can never include the `=` that separates a key from its
/// value, nor anything after it, so `grep -o` of it prints key names only.
/// What: requires a leading `^`, a non-empty remainder, and every remaining
/// byte in `[A-Za-z0-9_\-\[\]*+?]` — which excludes `.`, `\`, `=`, `|` and a
/// second `^`, so neither a wildcard nor a negated class nor an alternation can
/// reach a value.
/// Test: `allows_the_documented_key_name_only_grep`,
/// `denies_a_grep_o_whose_pattern_can_match_a_value`.
fn is_key_name_only_pattern(pattern: &str) -> bool {
    let Some(rest) = pattern.strip_prefix('^') else {
        return false;
    };
    !rest.is_empty()
        && rest.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'[' | b']' | b'*' | b'+' | b'?')
        })
}

/// The deny reason, naming the file, how it was about to be read, and the
/// sanctioned alternative (issue #7266).
fn deny_reason(target: &str, how: &str) -> String {
    format!(
        "reading `{target}` through {how} is refused (issue #7266) — its name is in this guard's \
         secret-bearing file class (`*.tfvars`, `*.tfvars.json`, `*.tfstate*`, `.env`/`.env.*`, \
         `*.pem`, `*.key`, an SSH private key `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`, \
         `.netrc`, `*.p12`/`*.pfx`/`*.jks`/`*.kdbx`, `*.ovpn`, or a name carrying \
         `credentials`/`secrets`/`token`), and a line range redacts nothing — `sed -n '38,46p'` \
         printed a live ngrok authtoken into a transcript exactly this way. Read only the KEY \
         NAMES with `grep -o '^[a-z_]*' <file>`, which this guard allows, or hand the file to \
         the tool that needs it by absolute path (`-var-file`, `-state`, `--env-file`) without \
         printing it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(command: &str) -> Option<String> {
        evaluate_secret_file_read_command(command)
    }

    #[test]
    fn denies_the_reported_sed_line_range() {
        // The exact shape from issue #7266's report.
        let reason = eval("sed -n '38,46p' terraform.tfvars").expect("denies");
        assert!(reason.contains("terraform.tfvars"), "{reason}");
        assert!(reason.contains("secret-bearing file class"), "{reason}");
        assert!(eval("sed -n '12,14p' infra/terraform.tfvars").is_some());
    }

    #[test]
    fn denies_a_tail_of_a_dotenv() {
        let reason = eval("tail -n 3 .env.production").expect("denies");
        assert!(reason.contains(".env.production"), "{reason}");
    }

    #[test]
    fn denies_a_grep_of_a_tfvars_json() {
        let reason = eval("grep -n TOKEN secrets/app.tfvars.json").expect("denies");
        assert!(reason.contains("app.tfvars.json"), "{reason}");
    }

    #[test]
    fn denies_every_content_printing_verb() {
        for verb in CONTENT_PRINTING_VERBS {
            // A pattern-leading verb's first positional is its script or
            // pattern, not a file — `sed live.tfvars` really does read stdin —
            // so those spellings carry one before the file operand.
            let command = if PATTERN_LEADING_VERBS.contains(verb) {
                format!("{verb} p live.tfvars")
            } else {
                format!("{verb} live.tfvars")
            };
            assert!(eval(&command).is_some(), "{verb} allowed `{command}`");
        }
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
    fn denies_a_read_behind_a_wrapper() {
        assert!(eval("sudo cat /etc/app/.env").is_some());
        assert!(eval("sh -c \"head -n 2 live.tfvars\"").is_some());
    }

    #[test]
    fn denies_shell_input_redirection_from_a_secret() {
        let reason = eval("while read l; do echo x; done < .env").expect("denies");
        assert!(reason.contains("shell input redirection"), "{reason}");
        assert!(eval("read -r line <live.tfvars").is_some());
    }

    #[test]
    fn denies_an_inline_python_program_that_opens_a_secret() {
        let reason = eval("python3 -c 'print(open(\"terraform.tfvars\").read())'").expect("denies");
        assert!(reason.contains("inline `python3` program"), "{reason}");
    }

    #[test]
    fn denies_an_unlexable_segment_that_names_a_secret() {
        // An unbalanced quote makes `shlex::split` return `None`; the word scan
        // still sees the file and refuses.
        let reason = eval("awk '{print} terraform.tfvars").expect("denies");
        assert!(reason.contains("cannot parse"), "{reason}");
    }

    #[test]
    fn allows_a_sed_line_range_of_an_ordinary_file() {
        assert_eq!(eval("sed -n '1,5p' README.md"), None);
    }

    #[test]
    fn allows_cat_of_a_manifest() {
        assert_eq!(eval("cat Cargo.toml"), None);
    }

    #[test]
    fn allows_a_tfvars_mention_that_is_not_a_file_operand() {
        assert_eq!(eval("git log --grep tfvars"), None);
        assert_eq!(eval("git log --grep .env --oneline"), None);
    }

    #[test]
    fn allows_a_grep_whose_pattern_looks_like_a_secret_name() {
        assert_eq!(eval("grep -rn tfvars docs/"), None);
        assert_eq!(eval("grep .env README.md"), None);
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
    fn allows_the_documented_key_name_only_grep() {
        assert_eq!(eval("grep -o '^key_[a-z_]*' terraform.tfvars"), None);
        assert_eq!(eval("grep -o '^[a-z_]*' .env"), None);
        assert_eq!(eval("grep --only-matching -e '^app_id' live.tfvars"), None);
    }

    #[test]
    fn denies_a_grep_o_whose_pattern_can_match_a_value() {
        // `^.*` is anchored but `.` reaches past the `=`.
        assert!(eval("grep -o '^.*' terraform.tfvars").is_some());
        // Unanchored: the match can start inside the value.
        assert!(eval("grep -o 'token' terraform.tfvars").is_some());
        // No `-o`: the whole matching LINE prints.
        assert!(eval("grep '^key_' terraform.tfvars").is_some());
    }

    #[test]
    fn file_operands_drops_a_leading_pattern() {
        let tail: Vec<String> = ["-n", "TOKEN", "app.tfvars"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(file_operands("grep", &tail), vec!["app.tfvars".to_string()]);
    }

    #[test]
    fn file_operands_keeps_the_file_when_a_pattern_flag_is_present() {
        let tail: Vec<String> = ["-n", "-e", "1,5p", "live.tfvars"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(file_operands("sed", &tail), vec!["live.tfvars".to_string()]);
        assert!(eval("sed -n -e '1,5p' live.tfvars").is_some());
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
    fn allows_every_non_read_tool() {
        let input = serde_json::json!({"file_path": "/repo/.env"});
        for tool in ["Write", "Edit", "Bash", "Grep", "Glob", "Task"] {
            assert_eq!(evaluate_secret_file_read_tool(tool, Some(&input)), None);
        }
        assert_eq!(evaluate_secret_file_read_tool("Read", None), None);
        assert_eq!(
            evaluate_secret_file_read_tool("Read", Some(&serde_json::json!({"file_path": ""}))),
            None
        );
    }
}
