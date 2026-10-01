//! `tm hook --pm-guard` — a `gh api` DELETE of a GitHub secret (#8875, the
//! #8869 design's `gh api -X DELETE` DENY row), and the other calls that
//! delete one: `gh secret delete`, a `gh alias` that runs a DELETE, and a
//! `curl` DELETE of a `secrets` path.
//!
//! Why: the read rule judges a path by its basename, and in
//! `repos/O/R/actions/secrets/NAME` that is the secret's name, which is not a
//! secret-shaped word, so the rule never looked. The call deletes a repo's,
//! org's or environment's Actions, Dependabot or Codespaces secret — damage,
//! not a leak — so it is judged by method and endpoint, not by the file class.
//! What: [`evaluate_gh_api_secret_delete`]. A child of
//! `pm_guard_secret_consumers` (split out for the 500-SLOC cap), so it shares
//! that module's redirect reading.
//! Test: `denies_a_gh_api_delete_of_any_secret_8875`,
//! `keeps_the_get_listing_and_literal_non_secret_deletes_8875`,
//! `denies_a_delete_whose_endpoint_the_guard_cannot_read_8875`,
//! `denies_the_round_three_forms_8875`,
//! `every_gh_api_delete_arm_fails_closed_8875`; end to end,
//! `pm_guard_denies_a_gh_api_delete_of_a_secret_8875`.

use super::is_redirect_shaped;
use crate::commands::pm_guard_bash::{blank_spans, data_bodies, split_shell_segments, tokenize};
use crate::commands::pm_guard_secret_read::command_basename;

/// `gh api` switches: they take no value (pflag also accepts `--name=bool`).
const GH_API_SWITCHES: &[&str] = &[
    "--include",
    "--paginate",
    "--silent",
    "--slurp",
    "--verbose",
    "--help",
];

/// `gh api` long options that take one value.
const GH_API_VALUED: &[&str] = &[
    "--method",
    "--header",
    "--raw-field",
    "--field",
    "--input",
    "--jq",
    "--template",
    "--cache",
    "--preview",
    "--hostname",
];

/// `curl` short options that take a value (the value is the rest of the word
/// or the next word), besides `-X`.
const CURL_VALUED_SHORT: &[char] = &[
    'A', 'b', 'c', 'C', 'd', 'D', 'e', 'E', 'F', 'H', 'K', 'm', 'o', 'P', 'Q', 'r', 't', 'T', 'u',
    'U', 'w', 'x', 'y', 'Y', 'z',
];

/// `curl` long options that take a value, besides `--request` and `--url`.
const CURL_VALUED_LONG: &[&str] = &[
    "header",
    "data",
    "data-raw",
    "data-binary",
    "data-urlencode",
    "data-ascii",
    "json",
    "user",
    "output",
    "user-agent",
    "referer",
    "cookie",
    "cookie-jar",
    "form",
    "write-out",
    "proxy",
    "oauth2-bearer",
    "max-time",
    "connect-timeout",
    "config",
    "cert",
    "key",
    "cacert",
    "retry",
    "upload-file",
    "dump-header",
];

/// Programs whose operands are a command text of their own (`ssh host 'cmd'`,
/// `eval 'cmd'`), as the sibling credential rule reads them.
const OPERAND_RUNNERS: &[&str] = &["ssh", "eval"];

/// Re-judge nesting (`ssh`/`eval` operands, `$S -c`, a shell alias) the rule
/// follows before refusing — the `credential_print` cap.
const MAX_DEPTH: usize = 8;

/// Refuse a call that DELETEs a GitHub secret: `Some(reason)` denies.
///
/// Why: see the module doc. A pre-filter on the word `secrets` failed open
/// (#8875 round 2): `$'…sec\x72ets…'`, `sec{r..r}ets`, an endpoint piped in by
/// `xargs`, or an `$EP` bound in an earlier call name no `secrets` word, yet
/// the shell hands gh a secrets path. Round 3 (owner ruling): an unlexable
/// call is refused outright, with no word heuristic to slip past.
/// What: each segment that runs `gh api` — the program `gh` by basename or a
/// word the shell rewrites (`$G`, `g[h]`), then `api` or a rewritten word
/// (`$A`, `{api,}`) — is read the way gh's pflag parser reads it (`-X DELETE`,
/// `-XDELETE`, `-X=DELETE`, `--method[=]DELETE`, any case, before or after the
/// endpoint). A DELETE, or a method the guard cannot read literally, denies
/// when its endpoint is absent, repeated or not literal, and — for one literal
/// endpoint — when that endpoint names a `secrets` word. A literal non-secret
/// DELETE and every literal non-DELETE pass, so the #8869 GET listing still
/// does. Also refused: `gh secret delete|remove`, a `gh alias set` whose
/// expansion is an `api` DELETE or a `secret delete`, `gh alias import`, and a
/// `curl` DELETE of a `secrets` URL or one it cannot read. `ssh`/`eval`
/// operands and a non-literal shell's `-c` text are judged as commands of
/// their own, up to [`MAX_DEPTH`] deep.
///
/// Fail closed: an unlexable segment that starts one of these calls denies; a
/// `gh api` call with an unknown option or a flag missing its value denies
/// when the command names `secrets` or the call carries a `delete` word or an
/// expansion; nesting past [`MAX_DEPTH`] denies.
/// Test: `denies_a_gh_api_delete_of_any_secret_8875`,
/// `denies_a_delete_whose_endpoint_the_guard_cannot_read_8875`,
/// `denies_the_round_three_forms_8875`,
/// `keeps_the_get_listing_and_literal_non_secret_deletes_8875`,
/// `every_gh_api_delete_arm_fails_closed_8875`.
pub(crate) fn evaluate_gh_api_secret_delete(command: &str) -> Option<String> {
    // #9001: an inert data here-document body is no shell call.
    judge(&without_inert_bodies(command), 0).then(deny_reason)
}

/// `command` with each inert data here-document body blanked (#9001 case 4).
///
/// Why: each line of a `python3 - <<'EOF'` body was judged as a shell
/// segment, and a Python line that does not lex, with two bracketed words in
/// a row (`d[k] {x}`), read as a rewritten `gh api` call.
/// What: a body handed to a non-shell program (`data_bodies`) is blanked when
/// it is inert: neither it nor its operator line names a `gh` or `curl` word
/// with quotes and backslashes removed; an unquoted-delimiter body runs no `$(…)` or backtick; and a body
/// carrying a `$` sits in a command that names no `gh`/`curl` anywhere, so no
/// variable set beside it can spell the call. Any other body, and every
/// operator line, is judged as before.
/// Test: `a_python_heredoc_with_no_gh_or_curl_is_no_secret_delete_9001`,
/// `a_heredoc_body_that_names_gh_or_runs_a_substitution_still_denies_9001`.
fn without_inert_bodies(command: &str) -> String {
    let command_names_a_call = names_gh_or_curl(command);
    let spans: Vec<(usize, usize)> = data_bodies(command)
        .iter()
        .filter(|body| {
            let text = &command[body.span.0..body.span.1];
            let operator = &command[body.operator_line.0..body.operator_line.1];
            !names_gh_or_curl(text)
                && !names_gh_or_curl(operator)
                && !(body.expands && (text.contains("$(") || text.contains('`')))
                && !(text.contains('$') && command_names_a_call)
        })
        .map(|body| body.span)
        .collect();
    blank_spans(command, &spans)
}

/// Whether `text`, quotes and backslashes removed, names a `gh`/`curl` word.
fn names_gh_or_curl(text: &str) -> bool {
    [
        text.replace(['\'', '"', '\\'], ""),
        text.replace(['\'', '"', '\\'], " "),
    ]
    .iter()
    .any(|flat| {
        flat.split(|c: char| !(c.is_ascii_alphanumeric() || "-_./".contains(c)))
            .any(|w| matches!(command_basename(w).as_str(), "gh" | "curl"))
    })
}

/// Whether `command`, reached `depth` re-judges deep, deletes a secret.
fn judge(command: &str, depth: usize) -> bool {
    // #8875 round 3: past the cap the guard cannot see the call.
    if depth > MAX_DEPTH {
        return true;
    }
    let secrets_named = names_secrets(command);
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_deletes_a_secret(segment, secrets_named, depth))
}

/// Whether one segment runs a call the guard must refuse.
fn segment_deletes_a_secret(segment: &str, secrets_named: bool, depth: usize) -> bool {
    let Ok(argv) = tokenize(segment) else {
        // #8875 round 3 (owner ruling): an unlexable call is refused outright.
        return starts_a_watched_call(segment);
    };
    reruns_a_refused_command(&argv, depth)
        || gh_calls(&argv, "api").any(|(_, tail)| gh_api_deletes(tail, secrets_named))
        || gh_calls(&argv, "secret").any(|(sub, tail)| secret_subcommand_deletes(sub, tail))
        || gh_calls(&argv, "alias").any(|(sub, tail)| alias_defines_a_delete(sub, tail, depth))
        || argv
            .iter()
            .enumerate()
            .any(|(at, w)| possibly_named(w, "curl") && curl_deletes_a_secret(&argv[at + 1..]))
}

/// Whether an unlexable segment starts a `gh api|secret|alias` or `curl` call,
/// read with quotes and backslashes both removed and turned into spaces.
fn starts_a_watched_call(segment: &str) -> bool {
    [
        segment.replace(['\'', '"', '\\'], ""),
        segment.replace(['\'', '"', '\\'], " "),
    ]
    .iter()
    .any(|flat| {
        let words: Vec<&str> = flat.split_whitespace().collect();
        words.iter().any(|w| command_basename(w) == "curl")
            || words.windows(2).any(|w| {
                ["api", "secret", "alias"]
                    .iter()
                    .any(|sub| possibly_named(w[0], "gh") && possibly_sub(w[1], sub))
            })
    })
}

/// `ssh`/`eval` operands, and a non-literal shell's `-c` text (`$S -c '…'`),
/// judged as a command of their own.
fn reruns_a_refused_command(argv: &[String], depth: usize) -> bool {
    // #8875 round 2: `ssh host 'gh api …'` runs its operands as a command.
    let runner = argv
        .iter()
        .position(|w| OPERAND_RUNNERS.contains(&command_basename(w).as_str()));
    if runner.is_some_and(|at| judge(&argv[at + 1..].join(" "), depth + 1)) {
        return true;
    }
    // #8875 round 3: a shell named by an expansion is no `sh -c` wrapper the
    // segment splitter knows.
    argv.windows(3).any(|w| {
        let c_flag = w[1].starts_with('-') && !w[1].starts_with("--") && w[1].contains('c');
        is_rewritten(&w[0]) && c_flag && judge(&w[2], depth + 1)
    })
}

/// The subcommand word and the argv tail after each `gh <sub>` pair in `argv`.
fn gh_calls<'a>(argv: &'a [String], sub: &'a str) -> impl Iterator<Item = (&'a str, &'a [String])> {
    argv.windows(2)
        .enumerate()
        .filter(move |(_, w)| possibly_named(&w[0], "gh") && possibly_sub(&w[1], sub))
        .map(move |(at, w)| (w[1].as_str(), &argv[at + 2..]))
}

/// Whether a program word could run `name`: by basename, or a word the shell
/// rewrites (`$G`, `` `which gh` ``, `g[h]`, `/opt/homebrew/bin/g[h]`).
fn possibly_named(program: &str, name: &str) -> bool {
    command_basename(program) == name || is_rewritten(program)
}

/// Whether a subcommand word could be `sub`: literally, or rewritten (`$A`,
/// `{api,}`).
fn possibly_sub(word: &str, sub: &str) -> bool {
    word == sub || is_rewritten(word)
}

/// Whether the shell rewrites a word before the program sees it: an
/// expansion, a glob or a brace.
fn is_rewritten(word: &str) -> bool {
    word.contains(['$', '`', '*', '?', '[', ']', '{', '}'])
}

/// Whether a `gh api` argv tail DELETEs a secret, or one the guard cannot read.
fn gh_api_deletes(tail: &[String], secrets_named: bool) -> bool {
    let Some((methods, endpoints)) = parse_gh_api(tail) else {
        // #8875: gh rejects this argv, so only a suspicious one is refused.
        let words: Vec<&str> = tail.iter().map(String::as_str).collect();
        return secrets_named || suspicious(&words);
    };
    maybe_delete(&methods)
        && match endpoints.as_slice() {
            [endpoint] if is_literal_endpoint(endpoint) => names_secrets(endpoint),
            // #8875 round 2: an endpoint from xargs/stdin, several, or one the
            // shell rewrites could be a secrets path.
            _ => true,
        }
}

/// Whether one of `methods` is DELETE, or a value the guard cannot read.
fn maybe_delete(methods: &[&str]) -> bool {
    methods
        .iter()
        .any(|m| m.eq_ignore_ascii_case("DELETE") || !is_literal_method(m))
}

/// #8875 round 3: whether a `gh secret` argv tail deletes: any `delete` or
/// `remove` word (the subcommand and its alias, wherever the `-R`/`--env`/
/// `--org` flags sit), or — after a literal `secret` — a rewritten first
/// operand. A rewritten `sub` (`$A $B $C`, a nested `$(echo …`) needs the word.
fn secret_subcommand_deletes(sub: &str, tail: &[String]) -> bool {
    tail.iter()
        .any(|w| matches!(w.as_str(), "delete" | "remove"))
        || (sub == "secret"
            && tail
                .iter()
                .find(|w| !w.starts_with('-'))
                .is_some_and(|w| is_rewritten(w)))
}

/// #8875 round 3: whether a `gh alias` argv tail defines an alias that deletes,
/// or one whose definition the guard cannot read (`import`, `set NAME -`, or a
/// rewritten verb after a literal `alias`).
fn alias_defines_a_delete(sub: &str, tail: &[String], depth: usize) -> bool {
    let shell = tail
        .iter()
        .any(|w| w == "--shell" || (w.starts_with('-') && !w.starts_with("--") && w.contains('s')));
    let words: Vec<&str> = tail
        .iter()
        .map(String::as_str)
        .filter(|w| *w == "-" || !w.starts_with('-'))
        .collect();
    match words.as_slice() {
        ["import", ..] | ["set", _, "-", ..] => true,
        [verb, ..] if sub == "alias" && is_rewritten(verb) => true,
        ["set", _, expansion, ..] => expansion_deletes(expansion, shell, depth),
        _ => false,
    }
}

/// Whether a `gh alias set` expansion runs a DELETE: a shell alias (`!…` or
/// `--shell`) judged as a command, an `api` call with a DELETE or unreadable
/// method, or a `secret delete|remove`.
fn expansion_deletes(expansion: &str, shell: bool, depth: usize) -> bool {
    if let Some(script) = expansion.strip_prefix('!').or(shell.then_some(expansion)) {
        return judge(script, depth + 1);
    }
    let Ok(words) = tokenize(expansion) else {
        return true;
    };
    let secret_delete = words
        .windows(2)
        .any(|w| w[0] == "secret" && matches!(w[1].as_str(), "delete" | "remove"));
    let api_delete = words.first().is_some_and(|w| w == "api")
        && parse_gh_api(&words[1..]).is_none_or(|(methods, _)| maybe_delete(&methods));
    secret_delete || api_delete
}

/// #8875 round 3: whether a `curl` argv tail DELETEs a `secrets` URL, or a URL
/// the guard cannot read (absent, or one the shell or curl's own globbing
/// rewrites). Another option's value that lands among the URLs is harmless:
/// it names no `secrets` word.
fn curl_deletes_a_secret(tail: &[String]) -> bool {
    let mut methods = Vec::new();
    let mut urls = Vec::new();
    let mut i = 0;
    while let Some(tok) = tail.get(i) {
        i += 1;
        let mut take = |joined: Option<&'_ str>| match joined {
            Some(v) => v.to_string(),
            None => {
                i += 1;
                tail.get(i - 1).cloned().unwrap_or_default()
            }
        };
        if let Some(long) = tok.strip_prefix("--") {
            let (name, joined) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            match name {
                "request" => methods.push(take(joined)),
                "url" => urls.push(take(joined)),
                _ if CURL_VALUED_LONG.contains(&name) && joined.is_none() => i += 1,
                _ => {}
            }
        } else if let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.is_empty()) {
            let valued = cluster
                .char_indices()
                .find(|(_, c)| *c == 'X' || CURL_VALUED_SHORT.contains(c));
            if let Some((k, letter)) = valued {
                let rest = &cluster[k + 1..];
                let value = take((!rest.is_empty()).then_some(rest));
                if letter == 'X' {
                    methods.push(value);
                }
            }
        } else {
            urls.push(tok.clone());
        }
    }
    let methods: Vec<&str> = methods.iter().map(String::as_str).collect();
    maybe_delete(&methods)
        && (urls.is_empty()
            || urls
                .iter()
                .any(|u| names_secrets(u) || is_rewritten(u) || u.starts_with('~')))
}

/// Whether an unreadable call carries a `delete` word or an expansion.
fn suspicious(words: &[&str]) -> bool {
    words
        .iter()
        .any(|w| w.to_ascii_lowercase().contains("delete") || w.contains(['$', '`']))
}

/// The method values and the endpoint words of a `gh api` argv tail, read as
/// pflag reads it; `None` for an unknown option or a flag missing its value.
fn parse_gh_api(args: &[String]) -> Option<(Vec<&str>, Vec<&str>)> {
    let mut methods = Vec::new();
    let mut endpoints = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        i += 1;
        if tok == "--" {
            endpoints.extend(args[i..].iter().map(String::as_str));
            break;
        }
        if is_redirect_shaped(tok) {
            // A bare operator (`>`, `2>`, `<`) takes the next word as its file.
            i += usize::from(tok.ends_with(['<', '>']));
            continue;
        }
        let (is_method, joined) = if let Some(long) = tok.strip_prefix("--") {
            let (name, joined) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            let name = &tok[..name.len() + 2];
            if GH_API_SWITCHES.contains(&name) {
                continue;
            }
            GH_API_VALUED.contains(&name).then_some(())?;
            (name == "--method", joined)
        } else if let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.is_empty()) {
            // `-i` is gh api's one short switch; the first valued letter takes
            // the rest of the word, past an optional `=`, as its value.
            let mut chars = cluster.trim_start_matches(['i', 'h']).chars();
            let Some(letter) = chars.next() else {
                continue;
            };
            matches!(letter, 'X' | 'H' | 'f' | 'F' | 'q' | 't' | 'p').then_some(())?;
            let rest = chars.as_str();
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            (letter == 'X', (!rest.is_empty()).then_some(rest))
        } else {
            endpoints.push(tok);
            continue;
        };
        let value = match joined {
            Some(v) => v,
            None => {
                i += 1;
                args.get(i - 1)?.as_str()
            }
        };
        if is_method {
            methods.push(value);
        }
    }
    Some((methods, endpoints))
}

/// Whether a method value is a plain word; `"$M"` or `""` could be DELETE.
fn is_literal_method(method: &str) -> bool {
    !method.is_empty() && method.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Whether `text`, de-quoted, percent-decoded and lower-cased, carries the word
/// `secrets` between non-alphanumeric bytes (`…/secrets/N`, `organization-secrets`).
fn names_secrets(text: &str) -> bool {
    let decoded = percent_decode(&text.replace(['\'', '"', '\\'], "")).to_ascii_lowercase();
    decoded
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| word == "secrets")
}

/// `text` with each `%XX` escape decoded; a malformed escape is kept as is.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match hex
            .filter(|_| bytes[i] == b'%')
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether an endpoint word reaches gh as written: no expansion (`$`, `$'…'`,
/// backtick), no glob, no brace expansion other than gh's `{owner}`/`{repo}`/
/// `{branch}` placeholders, no leading `~`, and a `?` only as a query mark.
fn is_literal_endpoint(endpoint: &str) -> bool {
    let unplaced = ["{owner}", "{repo}", "{branch}"]
        .iter()
        .fold(endpoint.to_string(), |text, p| text.replace(p, ""));
    let query_ok = endpoint
        .split_once('?')
        .is_none_or(|(_, query)| !query.contains(['/', '?']));
    query_ok
        && !endpoint.starts_with('~')
        && !unplaced.contains(['$', '`', '*', '[', ',', '{', '}'])
}

/// The #8875 refusal; it never echoes the command.
fn deny_reason() -> String {
    "a `gh api` DELETE of a GitHub secret (`…/secrets/NAME` of a repo, org, user or \
     environment, Actions, Dependabot or Codespaces) is refused (issue #8875) — it destroys a \
     secret other workflows depend on. A GET that lists secret names is still allowed (issue \
     #8869). A `gh api` DELETE whose endpoint this guard cannot read literally (a variable, \
     `$'…'`, a brace or glob, or one fed in by `xargs`), or whose method it cannot read, is \
     refused too; write the endpoint out literally. So are `gh secret delete`, a `gh alias` \
     that runs a DELETE, a `curl` DELETE of a secrets URL, and a `gh api`, `gh secret`, \
     `gh alias` or `curl` call this guard cannot parse. Ask the operator to delete a secret."
        .to_string()
}

#[cfg(test)]
#[path = "pm_guard_secret_gh_api_delete_tests.rs"]
mod tests;
