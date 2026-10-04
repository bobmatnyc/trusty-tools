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
use crate::commands::pm_guard_bash::{lone_inert_heredoc, tokenize};
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
/// What: `true` only for a whole command that is `cp [-pnifv…] [--] <src>
/// <dst>` with no wrapper, no other segment and no nested command, the
/// program word exactly `cp` (a path such as `./cp` gets no grant), both
/// operands literal and in the same directory (compared lexically), both
/// [`is_secret_read_target`], and every word in `named` cut from one of them.
/// A long flag, a third operand, a redirect, an expansion or another
/// directory keeps the deny. The #7122 worktree-destination rule judges the
/// same command on its own. Both operands are resolved against `cwd`, the
/// hook's working directory, unless absolute; one that cannot be resolved
/// keeps the deny. The source must be a regular file with one link, and the
/// destination nothing or such a file, since `cp` follows a symlink, writes
/// into a directory, and shares bytes through a hard link.
/// Residual: tests prove the refusal for a directory, a symlink and a hard
/// link. Nothing made between the hook and the copy is seen. Making a symlink,
/// FIFO or hard link under a secret-class name needs a command this rule
/// refuses, or the #8879 interpreter residual.
/// Test: `allows_a_same_class_copy_8093`, `denies_a_copy_out_of_the_class_8093`,
/// `denies_a_same_class_copy_onto_a_directory_or_symlink_8093`,
/// `denies_a_compound_or_wrapped_same_class_copy_8093`,
/// `denies_a_same_class_copy_through_a_hard_link_8093`,
/// `denies_a_same_class_copy_from_a_symlink_or_missing_source_8093`.
pub(crate) fn same_class_copy(segment: &str, named: &[String], cwd: Option<&Path>) -> bool {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return false;
    }
    let Ok(argv) = tokenize(segment) else {
        return false;
    };
    // #8093: a wrapper (`command`, `env -C`, `sudo`) may run another `cp` or
    // move the working directory, so only a bare `cp` is read.
    // #8093: the lexer has removed every backslash, so the word must be `cp`
    // itself; `./cp` or `bin/cp` can be `cat` under that name.
    if strip_wrapper_prefix(&argv) != Some(0) || argv.first().map(String::as_str) != Some("cp") {
        return false;
    }
    let mut operands: Vec<&str> = Vec::new();
    let mut options_ended = false;
    for token in &argv[1..] {
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
    if !named.iter().all(|word| cut.contains(word)) {
        return false;
    }
    // #8093: an operand that cannot be resolved cannot be inspected.
    let resolve = |op: &str| {
        let op = Path::new(op);
        match cwd {
            _ if op.is_absolute() => Some(op.to_path_buf()),
            Some(cwd) => Some(cwd.join(op)),
            None => None,
        }
    };
    let (Some(source), Some(dest)) = (resolve(source), resolve(dest)) else {
        return false;
    };
    match std::fs::symlink_metadata(&dest) {
        Ok(meta) if !is_lone_file(&meta) => return false,
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return false,
        _ => {}
    }
    std::fs::symlink_metadata(&source).is_ok_and(|meta| is_lone_file(&meta))
}

/// Whether `meta` is a regular file with one link (#8093). A symlink, a
/// directory, a FIFO or a hard-linked file answers `false`.
fn is_lone_file(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.file_type().is_file() && meta.nlink() == 1
    }
    // No link count to read: fail closed.
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// The directory part of an operand as written, `.` when it has none.
fn directory_of(operand: &str) -> &Path {
    Path::new(operand)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Whether the whole of `command` is `cat` writing one quoted here-document
/// body to a literal file (#7833).
///
/// Why: the #7266 body scan read `print(r.key)` and `rows.append({"id"…` in a
/// script an agent was writing as secret file names. `cat` with a quoted
/// delimiter writes the body verbatim and prints nothing, so no byte of any
/// file reaches the transcript. Writing it grants nothing new: the Write tool
/// already puts the same bytes at the same path, because
/// `evaluate_secret_file_read_tool` judges a Write's `file_path` and never its
/// `content`.
/// What: [`lone_inert_heredoc`] accepts the command, so its delimiter is a
/// quoted word the #9150 allowlist reads as the shell does, its operator line
/// is first and carries no `|`, `;`,
/// `&` or `$`, and nothing follows the terminator. The operator line must
/// also be `cat` with a `>`/`>>` redirect to a file that is not a device
/// ([`writes_to_a_device`]): a body `cat` prints, an interpreter runs, or an
/// unquoted delimiter expands keeps the scan. The operator line's own argv
/// keeps its scan, so `cat > .env <<'EOF'` denies. The #7266 read rule skips
/// its body scan and the copy rule blanks the body.
/// Residual, shared with the Write tool: a file a tool runs with no command
/// naming it (`.git/hooks/*`, an rc file such as `~/.zshenv` given as an
/// absolute path, `conftest.py` under pytest, `build.rs` under cargo) runs the
/// body unjudged. A relative destination inside `/dev`, or a destination
/// pre-placed as a symlink to `/dev/stdout`, is not seen: this function has no
/// cwd to resolve the destination from, and its caller in
/// `pm_guard_secret_read` sits at its line cap (#7833).
/// Test: `guard_7833_a_quoted_body_cat_writes_to_a_file_is_data`.
pub(crate) fn body_written_to_a_file(command: &str) -> bool {
    if lone_inert_heredoc(command).is_none() {
        return false;
    }
    let line = command.trim_start().lines().next().unwrap_or_default();
    // `lone_inert_heredoc` passed `cat` only with a redirect beside `<<WORD`.
    let Some(argv) = shlex::split(line) else {
        return false;
    };
    if argv.len() <= 2 || argv[0] != "cat" {
        return false;
    }
    let redirect: Vec<&str> = argv[1..]
        .iter()
        .map(String::as_str)
        .filter(|t| !t.starts_with("<<"))
        .collect();
    let dest = match redirect.as_slice() {
        [">" | ">>", path] => Some(*path),
        [glued] => glued.strip_prefix(">>").or_else(|| glued.strip_prefix('>')),
        _ => None,
    };
    // #8093: `/dev/stdout` and its kin print the body to the transcript.
    dest.is_some_and(|dest| !writes_to_a_device(dest))
}

/// Whether `dest` may name a file under `/dev` or `/proc` (#8093).
///
/// What: folds `.` and `..` lexically. An absolute path, or a relative one
/// whose `..` climbs past its start and so may reach `/`, answers `true` when
/// its first remaining component is `dev` or `proc`.
fn writes_to_a_device(dest: &str) -> bool {
    use std::path::Component;
    let mut parts = Vec::new();
    let mut rooted = false;
    for component in Path::new(dest).components() {
        match component {
            Component::RootDir => rooted = true,
            // An unmatched `..` may climb to `/` from an unknown cwd.
            Component::ParentDir => rooted |= parts.pop().is_none(),
            Component::Normal(part) => parts.push(part),
            Component::CurDir | Component::Prefix(_) => {}
        }
    }
    rooted && matches!(parts.first().and_then(|p| p.to_str()), Some("dev" | "proc"))
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
