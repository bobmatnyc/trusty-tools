//! Tests for the SSH `Host` alias table (#7196).
//!
//! Why: every one of these feeds a gate whose ALLOW deletes a checkout, so the
//! two directions matter equally — an alias that IS declared must resolve, and
//! one that is not must yield `None` so the caller can refuse.
//!
//! No test here reads the operator's real `~/.ssh/config`: each either parses a
//! literal or points [`SshHostAliases::load`] at a file it wrote itself.

use super::*;

#[test]
fn an_alias_block_rewrites_its_host() {
    let aliases = SshHostAliases::parse(
        "Host github-duetto\n  HostName github.com\n  User git\n  IdentityFile ~/.ssh/id_duetto\n",
    );
    assert_eq!(
        aliases.hostname_for("github-duetto").as_deref(),
        Some("github.com")
    );
}

/// A host nothing renames answers `None` — the caller, not this table, decides
/// what that means.
#[test]
fn an_undeclared_host_is_not_rewritten() {
    let aliases = SshHostAliases::parse("Host github-duetto\n  HostName github.com\n");
    assert_eq!(aliases.hostname_for("github.com"), None);
    assert_eq!(aliases.hostname_for("gh-bob"), None);
}

/// ssh keeps the FIRST value obtained for a keyword, so an earlier block wins
/// over a later one that also matches.
#[test]
fn the_first_matching_block_wins() {
    let aliases = SshHostAliases::parse(
        "Host gh-bob\n  HostName github.com\n\nHost gh-*\n  HostName ghe.example\n",
    );
    assert_eq!(
        aliases.hostname_for("gh-bob").as_deref(),
        Some("github.com")
    );
    assert_eq!(
        aliases.hostname_for("gh-other").as_deref(),
        Some("ghe.example")
    );
}

#[test]
fn wildcard_patterns_match_a_family_of_aliases() {
    let aliases = SshHostAliases::parse("Host github-* gh?\n  HostName github.com\n");
    for alias in ["github-duetto", "github-personal", "github-", "gh1"] {
        assert_eq!(
            aliases.hostname_for(alias).as_deref(),
            Some("github.com"),
            "{alias}"
        );
    }
    for alias in ["gitlab-duetto", "gh12", "gh"] {
        assert_eq!(aliases.hostname_for(alias), None, "{alias}");
    }
}

#[test]
fn a_negated_pattern_excludes_the_host() {
    let aliases = SshHostAliases::parse("Host github-* !github-legacy\n  HostName github.com\n");
    assert_eq!(
        aliases.hostname_for("github-duetto").as_deref(),
        Some("github.com")
    );
    assert_eq!(aliases.hostname_for("github-legacy"), None);
}

#[test]
fn percent_h_expands_to_the_query() {
    let aliases = SshHostAliases::parse("Host gh-*\n  HostName %h.example\n");
    assert_eq!(
        aliases.hostname_for("gh-bob").as_deref(),
        Some("gh-bob.example")
    );
}

#[test]
fn equals_separated_directives_parse() {
    for text in [
        "Host=github-duetto\nHostName=github.com\n",
        "Host = github-duetto\nHostName = github.com\n",
        "  Host   github-duetto  \n\tHostName\tgithub.com\n",
    ] {
        assert_eq!(
            SshHostAliases::parse(text)
                .hostname_for("github-duetto")
                .as_deref(),
            Some("github.com"),
            "{text:?}"
        );
    }
}

/// Keywords and hostnames are case-insensitive; a full-line `#` is a comment.
#[test]
fn comments_and_case_are_ignored() {
    let aliases = SshHostAliases::parse(
        "# work identity\nHOST GitHub-Duetto\n  hostname GitHub.COM\n# trailing note\n",
    );
    assert_eq!(
        aliases.hostname_for("github-duetto").as_deref(),
        Some("github.com")
    );
    assert_eq!(
        aliases.hostname_for("GITHUB-DUETTO").as_deref(),
        Some("github.com")
    );
}

/// 🔴 `Match` is not followed, and must not leak: a `HostName` written inside a
/// `Match` block belongs to that block's conditions, not to the `Host` above
/// it. Attributing it upward would rewrite a host on a condition this parser
/// never evaluated.
#[test]
fn a_match_block_is_not_attributed_to_the_host_above_it() {
    let aliases = SshHostAliases::parse(
        "Host github-duetto\n  User git\n\nMatch host github-duetto exec \"true\"\n  \
         HostName ghe.example\n",
    );
    assert_eq!(aliases.hostname_for("github-duetto"), None);
}

/// A block that sets no `HostName` renames nothing and must not shadow a later
/// block that does.
#[test]
fn a_block_without_a_hostname_does_not_shadow_a_later_one() {
    let aliases = SshHostAliases::parse(
        "Host github-*\n  User git\n\nHost github-duetto\n  HostName github.com\n",
    );
    assert_eq!(
        aliases.hostname_for("github-duetto").as_deref(),
        Some("github.com")
    );
}

#[test]
fn a_config_file_on_disk_is_parsed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("config");
    std::fs::write(&path, "Host github-duetto\n  HostName github.com\n").expect("write config");
    assert_eq!(
        SshHostAliases::load(&path)
            .hostname_for("github-duetto")
            .as_deref(),
        Some("github.com")
    );
}

/// A config that is not there rewrites nothing — which the caller turns into a
/// refusal, never a guess.
#[test]
fn a_missing_config_file_rewrites_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let aliases = SshHostAliases::load(&tmp.path().join("no-such-config"));
    assert_eq!(aliases.hostname_for("github-duetto"), None);
    assert_eq!(SshHostAliases::empty().hostname_for("github-duetto"), None);
}
