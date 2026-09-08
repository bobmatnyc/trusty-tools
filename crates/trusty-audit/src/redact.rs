//! Collapsing the operator's home-directory path out of client-facing text
//! (#7137).
//!
//! Why: every path this crate records about its own working area starts with
//! the operator's home directory, and three client-facing members carried it
//! verbatim — `package.toml`'s per-repository `gaps`, the same gaps in
//! `reports/index.md`, and each `manifest.toml`'s `[[repositories]] path`. The
//! recipient of an engagement package therefore learned the auditing operator's
//! local account name and machine layout, repeated once per repository. The
//! path is also worth nothing to them: it names a directory on a machine they
//! do not have.
//! What: [`home_paths`] replaces the home-directory prefix with `~`, the
//! spelling every shell and every reader already resolves. It is a textual
//! substitution over rendered output rather than a `Path` operation, because
//! the values it has to reach are embedded in prose a collector wrote
//! ("trusty-search could not index /Users/…/work/repos/acme") as often as they
//! are standalone fields.
//!
//! This is the crate's ONE home-path redaction (the "common entry point" rule).
//! Credential redaction is a different job with a different shape and lives in
//! `trusty_common::credentials::redact`; nothing here masks a secret.
//! Test: `redact_tests`.

use std::path::Path;

/// What a redacted home-directory prefix is rendered as.
pub const HOME_PLACEHOLDER: &str = "~";

/// `text` with every occurrence of this machine's home directory collapsed to
/// [`HOME_PLACEHOLDER`].
///
/// Why: the client-facing render sites (`crate::index_report::render`,
/// `crate::package::generated::render_metadata`, and the packaged
/// `manifest.toml`) each hold text assembled long after the home directory was
/// read, so they ask for it here rather than threading it through.
/// What: [`home_paths_under`] against `dirs::home_dir()`, and `text` unchanged
/// on a machine that reports no home directory — there is then no prefix to
/// match, and inventing one would rewrite unrelated text.
/// Test: `redact_tests::the_running_home_is_collapsed`.
pub fn home_paths(text: &str) -> String {
    match dirs::home_dir() {
        Some(home) => home_paths_under(text, &home),
        None => text.to_owned(),
    }
}

/// `text` with every occurrence of `home` collapsed to [`HOME_PLACEHOLDER`].
///
/// Why: the pure half of [`home_paths`], so a test states its own home value
/// instead of reading the one the test process happens to run under — this
/// crate's tests never write `HOME` (`crate::chain`'s note on the same point).
/// What: a plain substring replacement of `home`'s string form with any
/// trailing separator trimmed off first, so `/Users/ada/x` becomes `~/x`, a
/// bare `/Users/ada` becomes `~`, and a `home` value that arrived with a
/// trailing slash does not leave `~//x` behind. A `home` that is empty,
/// or the filesystem root, is left alone: both match nearly every absolute path
/// and would shred the text rather than redact it. Non-UTF-8 home paths are
/// left alone for the same reason — there is no needle to search for.
/// Test: `redact_tests::the_home_prefix_becomes_a_tilde`,
/// `redact_tests::a_degenerate_home_is_left_alone`,
/// `redact_tests::text_without_the_home_prefix_is_untouched`.
pub fn home_paths_under(text: &str, home: &Path) -> String {
    let Some(home) = home.to_str() else {
        return text.to_owned();
    };
    if home.is_empty() || home == "/" || home == std::path::MAIN_SEPARATOR_STR {
        return text.to_owned();
    }
    let trimmed = home.trim_end_matches(std::path::MAIN_SEPARATOR);
    if trimmed.is_empty() {
        return text.to_owned();
    }
    text.replace(trimmed, HOME_PLACEHOLDER)
}

/// Every string in `lines`, redacted by [`home_paths`].
///
/// A convenience for the two render sites that hold a `Vec<String>` of gap
/// lines; it exists so neither of them spells the `map`/`collect` itself.
/// Test: `redact_tests::a_gap_list_is_redacted_line_by_line`.
pub fn home_paths_in(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| home_paths(line)).collect()
}

/// The redacted text of a packaged `manifest.toml`, or `None` for any other
/// member.
///
/// Why: `manifest.toml`'s `[[repositories]] path` is the third place the
/// operator's home directory reached a recipient (#7137), and it is the only
/// one that arrives as bytes another program wrote — `tga audit` records the
/// absolute checkout it scanned. The path is meaningless on the recipient's
/// machine either way, so the packaged copy states `~`.
/// What: `Some(redacted)` when `entry` names a `manifest.toml` member, read
/// whole because a manifest is a few kilobytes of TOML, never the hundreds of
/// megabytes an extract database can run to. `None` for every other member and
/// for a manifest that is not valid UTF-8 — both then take the streaming copy
/// path unchanged.
///
/// # Errors
///
/// [`crate::error::AuditError::Package`] when the file cannot be read at all.
/// Test: `crate::package::package_tests::no_packaged_member_names_the_operators_home`.
pub fn packaged_manifest(
    entry: &str,
    source: &Path,
) -> Result<Option<String>, crate::error::AuditError> {
    if !entry.ends_with(crate::manifest::AuditManifest::FILE_NAME) {
        return Ok(None);
    }
    match std::fs::read(source) {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok().map(|text| home_paths(&text))),
        Err(source_error) => Err(crate::error::AuditError::Package {
            path: source.to_path_buf(),
            source: source_error,
        }),
    }
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    /// The shape #7137 reported: an operator's home path inside collector prose.
    ///
    /// Fails before the fix: `home_paths_under` does not exist.
    #[test]
    fn the_home_prefix_becomes_a_tilde() {
        let text = "trusty-search could not index \
                    /Users/operator/.trusty-tools/trusty-audit/work/repos/acme-api";
        let redacted = home_paths_under(text, Path::new("/Users/operator"));
        assert_eq!(
            redacted,
            "trusty-search could not index ~/.trusty-tools/trusty-audit/work/repos/acme-api"
        );
        assert!(!redacted.contains("/Users/operator"), "{redacted}");
    }

    /// A trailing separator on the home value must not leave `~/` doubled.
    #[test]
    fn a_trailing_separator_on_home_is_tolerated() {
        let redacted = home_paths_under("/Users/operator/work", Path::new("/Users/operator/"));
        assert_eq!(redacted, "~/work");
    }

    /// A bare home path, with nothing under it, is the placeholder alone.
    #[test]
    fn a_bare_home_path_becomes_the_placeholder() {
        assert_eq!(
            home_paths_under("/Users/operator", Path::new("/Users/operator")),
            "~"
        );
    }

    /// Root and empty match nearly every path, so they redact nothing.
    #[test]
    fn a_degenerate_home_is_left_alone() {
        let text = "/Users/operator/work/repos/acme";
        assert_eq!(home_paths_under(text, Path::new("/")), text);
        assert_eq!(home_paths_under(text, Path::new("")), text);
    }

    /// Text that never names the home directory comes back byte-identical.
    #[test]
    fn text_without_the_home_prefix_is_untouched() {
        let text = "trusty-search could not index /srv/checkouts/acme-api";
        assert_eq!(home_paths_under(text, Path::new("/Users/operator")), text);
    }

    /// The running-home wrapper is the same substitution, read from `dirs`.
    #[test]
    fn the_running_home_is_collapsed() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let Some(home) = home.to_str() else {
            return;
        };
        let redacted = home_paths(&format!("indexed {home}/x/y"));
        assert_eq!(redacted, "indexed ~/x/y", "home was {home}");
    }

    /// The list helper redacts each line rather than joining them.
    #[test]
    fn a_gap_list_is_redacted_line_by_line() {
        let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(str::to_owned)) else {
            return;
        };
        let lines = vec![format!("a: {home}/one"), format!("b: {home}/two")];
        assert_eq!(home_paths_in(&lines), vec!["a: ~/one", "b: ~/two"]);
    }
}
