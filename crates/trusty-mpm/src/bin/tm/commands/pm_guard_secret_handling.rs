//! Narrow grants and refusal hints beside the #7266 secret-read rule (#8093,
//! #8110, #8520, #8660).
//!
//! Why: `pm_guard_secret_read` sits at its SLOC cap. Each grant here is one
//! command shape that names a secret-bearing file and prints none of its
//! bytes; each hint names the supported route for a shape the rule keeps
//! refusing, so an agent is not left guessing.
//! What: [`same_class_copy`] (#8093) and [`is_tokeninfo_url`] (#8110)
//! are grants the read rule asks before it denies; [`refusal_hint`] (#8520,
//! #8660) is appended to its refusal. A grant answers `false` for anything it
//! cannot read, so the read rule's deny stands.
//! Test: `pm_guard_secret_handling_tests.rs`.

use std::path::Path;

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::tokenize;
use crate::commands::pm_guard_secret_read::{
    NESTED_COMMAND_MARKERS, Scan, command_basename, is_secret_read_target, secret_files_named_in,
};

/// Short `cp` flags a same-class copy may carry. None of them prints a file's
/// bytes or changes which file is written.
const SAME_CLASS_COPY_FLAGS: &[char] = &['p', 'n', 'i', 'f', 'v'];

/// Bytes that make the shell rewrite a word before `cp` sees it.
const SHELL_REWRITE_BYTES: &[char] = &['$', '`', '*', '?', '[', ']', '{', '}', '~'];

/// Whether `segment` copies one secret-bearing file to a sibling name the read
/// rule also refuses (#8093).
///
/// Why: the read rule refuses every verb outside its safe list, so a dated
/// backup of a Terraform state beside itself had no permitted spelling. A copy
/// prints nothing, and a destination the read rule refuses cannot be read back,
/// so the copy launders nothing. The copy stays in the source's own directory,
/// so it cannot carry a key from `~/.ssh` into a repository that a docs-only
/// commit and a push would then publish.
/// What: `true` only for `cp [-pnifv…] [--] <src> <dst>` with no nested
/// command, both operands literal and in the same directory (compared
/// lexically), both [`is_secret_read_target`], and every word in `named` cut
/// from one of them. A long flag, a third operand, a redirect, an expansion or
/// another directory keeps the deny. The #7122 worktree-destination rule
/// judges the same command on its own. Residual: the destination is judged by
/// name, so a symlink or FIFO already standing under a secret-class name is
/// followed; making one needs a command this rule refuses, or the #8879
/// interpreter residual.
/// Test: `allows_a_same_class_copy_8093`, `denies_a_copy_out_of_the_class_8093`.
pub(crate) fn same_class_copy(segment: &str, named: &[String]) -> bool {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return false;
    }
    let Ok(argv) = tokenize(segment) else {
        return false;
    };
    let Some(start) = strip_wrapper_prefix(&argv) else {
        return false;
    };
    if argv.get(start).map(|p| command_basename(p)).as_deref() != Some("cp") {
        return false;
    }
    let mut operands: Vec<&str> = Vec::new();
    let mut options_ended = false;
    for token in &argv[start + 1..] {
        if !options_ended && token == "--" {
            options_ended = true;
        } else if !options_ended && token.starts_with('-') {
            let cluster = &token[1..];
            if cluster.is_empty() || !cluster.chars().all(|c| SAME_CLASS_COPY_FLAGS.contains(&c)) {
                return false;
            }
        } else {
            operands.push(token);
        }
    }
    let [source, dest] = operands.as_slice() else {
        return false;
    };
    let literal = |op: &str| !op.is_empty() && !op.contains(SHELL_REWRITE_BYTES);
    if !(literal(source) && literal(dest)) || directory_of(source) != directory_of(dest) {
        return false;
    }
    if !(is_secret_read_target(source) && is_secret_read_target(dest)) {
        return false;
    }
    let cut: Vec<String> = [*source, *dest]
        .iter()
        .flat_map(|op| secret_files_named_in(op, Scan::Argv))
        .collect();
    named.iter().all(|word| cut.contains(word))
}

/// The directory part of an operand as written, `.` when it has none.
fn directory_of(operand: &str) -> &Path {
    Path::new(operand)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Programs whose URL operand the #8110 grant reads (#8110).
pub(crate) const URL_FETCHERS: &[&str] = &["curl", "wget"];

/// Google's OAuth2 token-introspection endpoints, as `host/path` (#8110).
///
/// Why: each answers with a token's scopes, audience and expiry, never with a
/// credential. The list is explicit because a URL in general can print one —
/// a cloud metadata server's `…/service-accounts/default/token` returns an
/// access token — so a URL is not exempt for being remote.
const TOKENINFO_ENDPOINTS: &[&str] = &[
    "oauth2.googleapis.com/tokeninfo",
    "www.googleapis.com/oauth2/v1/tokeninfo",
    "www.googleapis.com/oauth2/v2/tokeninfo",
    "www.googleapis.com/oauth2/v3/tokeninfo",
];

/// Whether `token` is an `https://` URL of one of [`TOKENINFO_ENDPOINTS`]
/// (#8110).
///
/// Why: the word cut reads `https://oauth2.googleapis.com/tokeninfo` as the
/// path `//oauth2.googleapis.com/tokeninfo`, whose basename is in the `token*`
/// family, so a curl to Google's introspection endpoint was refused as a file
/// read.
/// What: case-insensitive `https://` plus a listed `host/path`, then nothing
/// or a `?` query. The query may carry no shell syntax except a plain `$NAME`
/// variable, so a substitution keeps the scan. The caller checks the program
/// is a [`URL_FETCHERS`] entry.
/// Test: `allows_a_url_operand_of_a_fetcher_8110`,
/// `denies_a_local_file_beside_or_inside_a_url_8110`.
pub(crate) fn is_tokeninfo_url(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://") else {
        return false;
    };
    let (endpoint, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !TOKENINFO_ENDPOINTS.contains(&endpoint) {
        return false;
    }
    if query.contains(|c: char| c.is_whitespace() || "`'\"\\(){}<>|;&#/".contains(c)) {
        return false;
    }
    // Only `$NAME`: `$(`, `${` and `$'…'` were refused above.
    query
        .match_indices('$')
        .all(|(at, _)| query[at + 1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}

/// The sentence a refusal of `word` in `segment` ends with, or `""`.
///
/// Why: two refused shapes kept sending agents to workarounds because the
/// refusal named no supported route. A Terraform root whose state lives in a
/// main checkout had no named applier (#8660), and a count-only `grep -c` of a
/// dotenv file, local or over `aws ssm`, read as a harmless integer (#8520).
/// What: a Terraform state or vars name gets the apply route; a `grep` count
/// or presence flag gets the reason a count is refused and what to run
/// instead. Neither changes the verdict.
/// Test: `a_terraform_refusal_names_who_applies_8660`,
/// `a_count_refusal_names_the_supported_route_8520`.
pub(crate) fn refusal_hint(word: &str, segment: &str) -> &'static str {
    let base = command_basename(word).to_ascii_lowercase();
    if base.contains(".tfvars") || base.contains(".tfstate") {
        return TERRAFORM_HINT;
    }
    if counts_matches(segment) {
        return COUNT_HINT;
    }
    ""
}

/// #8660: who applies a Terraform root whose state lives in a main checkout.
const TERRAFORM_HINT: &str = " A Terraform root whose state and `terraform.tfvars` live in a main \
     checkout is applied from that checkout, by the operator or by `local-ops` dispatched there: \
     `terraform apply` in the root reads `./terraform.tfvars` itself, so no command names it. A \
     worktree agent may run `terraform plan|apply -state=<that state file>` and names no vars \
     file (issue #8660).";

/// #8520: why a count is refused, and the route that replaces it.
const COUNT_HINT: &str = " A count or presence read (`grep -c`, `grep -q`, `grep -l`) is \
     refused too, on this host or sent to another through `ssh` or `aws ssm`: the pattern is \
     yours to choose, so repeated counts spell the value out. To learn whether a key is set, run \
     the consuming service's own config check or health endpoint, or ask the operator to run the \
     count (issue #8520).";

/// Whether `segment` runs a grep-family program with a count or presence flag,
/// read loosely enough to see one inside an `ssh`/`aws ssm` command string.
fn counts_matches(segment: &str) -> bool {
    let words: Vec<&str> = segment
        .split(|c: char| c.is_whitespace() || "'\"[],=;".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    let greps = words.iter().any(|w| {
        matches!(
            command_basename(w).as_str(),
            "grep" | "egrep" | "fgrep" | "rg"
        )
    });
    let counts = words.iter().any(|w| {
        matches!(
            *w,
            "--count" | "--quiet" | "--silent" | "--files-with-matches" | "--files-without-match"
        ) || (w.len() > 1
            && w.starts_with('-')
            && !w.starts_with("--")
            && w[1..].chars().all(|c| c.is_ascii_alphabetic())
            && w[1..].contains(['c', 'q', 'l', 'L']))
    });
    greps && counts
}

#[cfg(test)]
#[path = "pm_guard_secret_handling_tests.rs"]
mod tests;
