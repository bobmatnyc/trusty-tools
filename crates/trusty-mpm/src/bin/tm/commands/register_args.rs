//! Positional-argument resolution for `tm register` (#4912).
//!
//! Why: `tm register` used to be `<ALIAS> <URL>` with both required. #4912 makes
//! the URL come first and the alias optional. Swapping positional order on its
//! own would be a SILENT reinterpretation — both positions take strings, so an
//! existing `tm register my-alias https://…` invocation would register an alias
//! named after the URL and a "URL" that is not one, with no error. A URL is
//! mechanically detectable, so this module detects which positional is the URL
//! and routes accordingly, keeping the legacy order working.
//! What: [`resolve_register_args`] maps the raw `(first, second?)` positionals
//! onto a `(alias, url)` pair, deriving the alias from the URL when only one
//! positional was given. Derivation delegates to trusty-common's
//! [`trusty_common::palace_id::owner_repo_from_git_remote`] — the workspace's
//! single git-URL→`owner-repo` parser — rather than re-implementing it here.
//! Test: `register_args_tests.rs`.

/// Resolve the `tm register` positionals into `(alias, url)`.
///
/// Why: see the module docs — accepting both positional orders is what keeps the
/// #4912 swap from silently reinterpreting existing invocations.
/// What: with two positionals, whichever one [`looks_like_url`] accepts becomes
/// the URL and the other becomes the alias; if both or neither look like a URL,
/// this errors rather than guessing. With one positional it must be the URL, and
/// the alias is derived as `owner-repo`.
///
/// Alias derivation falls back to the bare repo slug when the URL exposes no
/// owner segment (e.g. `https://example.com/repo` → `repo`); it errors only when
/// nothing at all can be extracted (empty input, host-only URL).
/// Test: `two_args_url_first`, `two_args_legacy_alias_first`, `one_arg_derives`,
/// `one_arg_non_url_errors`, `two_urls_error`, `two_non_urls_error`,
/// `derives_from_every_url_shape`, `no_owner_falls_back_to_repo`,
/// `host_only_url_errors`.
pub(crate) fn resolve_register_args(
    first: &str,
    second: Option<&str>,
) -> anyhow::Result<(String, String)> {
    let first = first.trim();
    match second.map(str::trim) {
        Some(second) => match (looks_like_url(first), looks_like_url(second)) {
            // #4912: the new order — `tm register <URL> [ALIAS]`.
            (true, false) => Ok((second.to_string(), first.to_string())),
            // #4912: the legacy order — `tm register <ALIAS> <URL>` still works.
            (false, true) => Ok((first.to_string(), second.to_string())),
            (true, true) => Err(anyhow::anyhow!(
                "both arguments look like URLs ('{first}' and '{second}') — \
                 refusing to guess which is the alias. Usage: tm register <url> [alias]"
            )),
            (false, false) => Err(anyhow::anyhow!(
                "neither argument looks like a repository URL ('{first}' and '{second}'). \
                 Usage: tm register <url> [alias]"
            )),
        },
        None => {
            if !looks_like_url(first) {
                return Err(anyhow::anyhow!(
                    "'{first}' does not look like a repository URL. \
                     Usage: tm register <url> [alias]"
                ));
            }
            let alias = derive_alias(first)?;
            Ok((alias, first.to_string()))
        }
    }
}

/// Derive the default `owner-repo` alias from a repository URL.
///
/// Why: `tm register <url>` with no alias needs a deterministic default, and it
/// must be the SAME slug the rest of the workspace already derives for that repo
/// (palace IDs, search index IDs) — so this delegates to trusty-common instead
/// of adding a second parser.
///
/// 🔴 The separator is a HYPHEN, not a slash, and that is deliberate: an alias
/// becomes a path segment wherever it is consumed (`<root>/projects/<alias>`,
/// socket names), so a literal `/` would nest into the filesystem. Do not
/// "fix" this to `owner/repo`. See `trusty_common::palace_id` module docs for
/// the same storage-safety invariant.
///
/// What: returns `owner-repo`, or the bare repo slug when the URL has no owner
/// segment. Errors when no repo segment can be extracted at all.
/// Test: `derives_from_every_url_shape`, `no_owner_falls_back_to_repo`,
/// `host_only_url_errors`.
fn derive_alias(url: &str) -> anyhow::Result<String> {
    trusty_common::palace_id::owner_repo_from_git_remote(url).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot derive an alias from '{url}' — no owner/repo path found. \
             Pass one explicitly: tm register {url} <alias>"
        )
    })
}

/// Decide whether a positional is a repository URL rather than an alias.
///
/// Why: this is the whole mechanism that lets both positional orders coexist.
/// It is safe to decide by shape because the two sets cannot overlap — a valid
/// alias must match `^[a-z0-9][a-z0-9._-]*$` (`validate_alias` in
/// `core::standalone::registry`), which admits no `/`, `:`, or `@`.
/// What: true when the string contains `://`, starts with `git@`, or contains a
/// `/` (the `host/owner/repo` shape, with or without a scheme).
/// Test: `looks_like_url_accepts_url_shapes`, `looks_like_url_rejects_aliases`.
fn looks_like_url(s: &str) -> bool {
    s.contains("://") || s.starts_with("git@") || s.contains('/')
}

#[cfg(test)]
#[path = "register_args_tests.rs"]
mod tests;
