//! `tm hook --pm-guard` — a `gh api` DELETE of a GitHub secret (#8875, the
//! #8869 design's `gh api -X DELETE` DENY row).
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
//! `every_gh_api_delete_arm_fails_closed_8875`; end to end,
//! `pm_guard_denies_a_gh_api_delete_of_a_secret_8875`.

use super::is_redirect_shaped;
use crate::commands::pm_guard_bash::{split_shell_segments, tokenize};
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

/// Programs whose operands are a command text of their own (`ssh host 'cmd'`,
/// `eval 'cmd'`), as the sibling credential rule reads them.
const OPERAND_RUNNERS: &[&str] = &["ssh", "eval"];

/// Refuse a `gh api` call that DELETEs a GitHub secret: `Some(reason)` denies.
///
/// Why: see the module doc. A pre-filter on the word `secrets` failed open
/// (#8875 round 2): `$'…sec\x72ets…'`, `sec{r..r}ets`, an endpoint piped in by
/// `xargs`, or an `$EP` bound in an earlier call name no `secrets` word, yet
/// the shell hands gh a secrets path.
/// What: each segment that runs `gh api` — or a non-literal program word then
/// `api` (`$G api`) — is read the way gh's pflag parser reads it (`-X DELETE`,
/// `-XDELETE`, `-X=DELETE`, `--method[=]DELETE`, any case, before or after the
/// endpoint). A DELETE, or a method the guard cannot read literally, denies
/// when its endpoint is absent, repeated or not literal, and — for one literal
/// endpoint — when that endpoint names a `secrets` word. A literal non-secret
/// DELETE and every literal non-DELETE pass, so the #8869 GET listing still
/// does. `ssh`/`eval` operands are joined and judged as a command of their
/// own.
///
/// Fail closed: an unlexable `gh api` segment, an unknown option, or a flag
/// missing its value denies when the command names `secrets`, or the call
/// carries a `delete` word or an expansion.
/// Test: `denies_a_gh_api_delete_of_any_secret_8875`,
/// `denies_a_delete_whose_endpoint_the_guard_cannot_read_8875`,
/// `keeps_the_get_listing_and_literal_non_secret_deletes_8875`,
/// `every_gh_api_delete_arm_fails_closed_8875`.
pub(crate) fn evaluate_gh_api_secret_delete(command: &str) -> Option<String> {
    let secrets_named = names_secrets(command);
    split_shell_segments(command)
        .iter()
        .any(|segment| segment_deletes_a_secret(segment, secrets_named))
        .then(deny_reason)
}

/// Whether one segment runs a `gh api` DELETE the guard must refuse.
fn segment_deletes_a_secret(segment: &str, secrets_named: bool) -> bool {
    let Ok(argv) = tokenize(segment) else {
        // #8875: fail closed on an unlexable `gh api` call.
        let flat = segment.replace(['\'', '"', '\\'], " ");
        let words: Vec<&str> = flat.split_whitespace().collect();
        return words.windows(2).any(|w| is_gh_api(w[0], w[1]))
            && (secrets_named || suspicious(&words));
    };
    // #8875 round 2: `ssh host 'gh api …'` runs its operands as a command.
    let runner = argv
        .iter()
        .position(|w| OPERAND_RUNNERS.contains(&command_basename(w).as_str()));
    if runner.is_some_and(|at| evaluate_gh_api_secret_delete(&argv[at + 1..].join(" ")).is_some()) {
        return true;
    }
    let Some(at) = argv.windows(2).position(|w| is_gh_api(&w[0], &w[1])) else {
        return false;
    };
    let tail = &argv[at + 2..];
    let Some((methods, endpoints)) = parse_gh_api(tail) else {
        // #8875: gh rejects this argv, so only a suspicious one is refused.
        let words: Vec<&str> = tail.iter().map(String::as_str).collect();
        return secrets_named || suspicious(&words);
    };
    let maybe_delete = methods
        .iter()
        .any(|m| m.eq_ignore_ascii_case("DELETE") || !is_literal_method(m));
    maybe_delete
        && match endpoints.as_slice() {
            [endpoint] if is_literal_endpoint(endpoint) => names_secrets(endpoint),
            // #8875 round 2: an endpoint from xargs/stdin, several, or one the
            // shell rewrites could be a secrets path.
            _ => true,
        }
}

/// Whether a word pair starts a `gh api` call: `gh` by basename, or a program
/// word the shell expands (`$G`, `` `which gh` ``), followed by `api`.
fn is_gh_api(program: &str, sub: &str) -> bool {
    sub == "api" && (command_basename(program) == "gh" || program.contains(['$', '`']))
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
     refused too; write the endpoint out literally. Ask the operator to delete a secret."
        .to_string()
}

#[cfg(test)]
#[path = "pm_guard_secret_gh_api_delete_tests.rs"]
mod tests;
