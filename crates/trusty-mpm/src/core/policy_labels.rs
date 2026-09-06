//! The GitHub labels trusty-mpm applies BY POLICY, and the one `gh label
//! create` argv the crate builds for them (#6914).
//!
//! Why: `gh issue edit --add-label` and `gh issue create --label` both fail on a
//! label the repository has never seen, so every label the harness applies by
//! policy has to exist before the first issue that uses it. Two places need that
//! answer — session launch (`core::session_launch::workstream_label`) and
//! `tm issue seed-labels` — and before this module they each carried their own
//! table, which drifted: launch was still creating a retired `in-progress` /
//! `blocked` pair the `status:*` lifecycle replaced, and neither knew about the
//! other's labels.
//! What: the [`PolicyLabel`] value type, the policy set ([`policy_labels`] =
//! the [`CONVENTION_LABEL`] plus `ws/<session>` when a session name is known),
//! the `ws/` name/color derivation, and [`create_label_argv`] — the single place
//! in the crate that spells a `gh label create` command line.
//!
//! This module deliberately holds NO process-spawning code. Its two consumers
//! reach `gh` through different, already-established seams (the launch path's
//! `GhLabelRunner`, the `tm issue` path's `CommandRunner`), and both build their
//! command line here.
//! Test: `convention_label_is_stable`, `policy_labels_includes_workstream`,
//! `policy_labels_without_session_skips_workstream`,
//! `policy_labels_blank_session_skips_workstream`, `owned_namespace_is_ws_only`,
//! `label_color_is_stable_and_valid_hex`, `label_color_differs_across_names`,
//! `label_name_short_is_verbatim`, `label_name_stays_within_github_cap`,
//! `label_name_long_is_truncated_with_hash_suffix`,
//! `label_name_distinct_long_names_get_distinct_labels`,
//! `create_label_argv_full`, `create_label_argv_omits_empty_fields`,
//! `create_label_argv_repo_and_force`,
//! `configured_labels_match_builtin_when_block_absent`,
//! `configured_labels_append_extra_labels`,
//! `configured_labels_restyle_a_builtin_by_name`.
//!
//! #6918 made the table CONFIGURABLE without adding a second one:
//! [`policy_labels_configured`] folds the operator's `agents.ticketing` block
//! over [`policy_labels`], and every consumer calls that one function.

use serde::Deserialize;

use crate::core::trusty_tools_config::ResolvedTicketing;

/// GitHub's hard cap on a label name's length (the full `ws/<name>` string).
pub const GITHUB_LABEL_MAX_LEN: usize = 50;

/// The framework's own crate/component label.
///
/// Why: `tm-ticketing` requires an owning-component label on every issue filed
/// against the harness itself, and that label has to exist for the first such
/// filing to succeed.
/// What: name only — the color and description live in [`convention_label`].
pub const CONVENTION_LABEL: &str = "trusty-mpm";

/// Color and description for [`CONVENTION_LABEL`], matching the value the
/// trusty-tools repo already carries so seeding a fresh repo reads identically.
const CONVENTION_LABEL_COLOR: &str = "BFD4F2";
const CONVENTION_LABEL_DESCRIPTION: &str = "trusty-mpm platform and related work";

/// The `ws/` prefix marking trusty-mpm's OWN label namespace.
const WORKSTREAM_PREFIX: &str = "ws/";

/// A repository label as it exists (or should exist) on GitHub.
///
/// Why: both halves of an idempotent seed — "what policy wants" and "what the
/// repo has" — need the same name/color/description shape for a diff, and
/// `gh label list --json name,color,description` returns exactly that, so the
/// type doubles as the list DTO.
/// What: the label `name`, its 6-hex `color` (no leading `#`), and a
/// description (empty means "omit the flag").
/// Test: `create_label_argv_full`, and `gh_list_repo_labels_parses` in
/// `bin/tm/commands/ticket/labels.rs`, which deserializes into this type.
// Deliberately NOT `#[non_exhaustive]`: the `tm` binary is a separate crate and
// builds these as struct literals in its own tests and `gh label list` fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PolicyLabel {
    /// Label name (e.g. `ws/tm-tcode-01`).
    pub name: String,
    /// 6-hex color, no `#` (e.g. `BFD4F2`).
    #[serde(default)]
    pub color: String,
    /// Human description shown in the GitHub label UI; empty means none.
    #[serde(default)]
    pub description: String,
}

impl PolicyLabel {
    /// Build a label from owned or borrowed parts.
    pub fn new(
        name: impl Into<String>,
        color: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            color: color.into(),
            description: description.into(),
        }
    }
}

/// The [`CONVENTION_LABEL`], fully specified.
pub fn convention_label() -> PolicyLabel {
    PolicyLabel::new(
        CONVENTION_LABEL,
        CONVENTION_LABEL_COLOR,
        CONVENTION_LABEL_DESCRIPTION,
    )
}

/// The `ws/<session_name>` workstream label, or `None` when no session name is
/// known.
///
/// Why: the PM brief's `--label ws/<name>` convention needs the label to exist
/// before a session's first issue or PR. A blank name has nothing to label
/// with, so it is a clean skip rather than a malformed `ws/` label.
/// What: `Some` with the truncated-and-hashed name from [`label_name_for`] and
/// the stable color from [`label_color_for`]; `None` when `session_name` is
/// empty or whitespace.
/// Test: `policy_labels_includes_workstream`,
/// `policy_labels_blank_session_skips_workstream`.
pub fn workstream_label(session_name: &str) -> Option<PolicyLabel> {
    let name = session_name.trim();
    if name.is_empty() {
        return None;
    }
    Some(PolicyLabel::new(
        label_name_for(name),
        label_color_for(name),
        format!("trusty-mpm workstream {name}"),
    ))
}

/// Every label the harness applies by policy.
///
/// Why: one table, so session launch and `tm issue seed-labels` cannot drift
/// apart on which labels the harness owns — the drift #6914 records.
/// What: the [`CONVENTION_LABEL`], plus the `ws/<session>` label when
/// `session_name` names one. The issue-lifecycle (`status:*`) labels are NOT
/// here: those are project-configured, read from `issue-state.yaml`, and
/// `tm issue seed-labels` merges the two sets.
/// Test: `policy_labels_includes_workstream`,
/// `policy_labels_without_session_skips_workstream`.
pub fn policy_labels(session_name: Option<&str>) -> Vec<PolicyLabel> {
    let mut out = vec![convention_label()];
    out.extend(session_name.and_then(workstream_label));
    out
}

/// [`policy_labels`] with the operator's `agents.ticketing` block applied —
/// the ONE call every consumer of the label policy makes (#6918).
///
/// Why: #6914 made the built-in table the single source; #6918 makes it
/// configurable without reintroducing a second table. Consumers call this and
/// nothing else, so a project's extra label reaches `tm issue seed-labels` and
/// session launch by the same route the built-in ones do.
/// What: starts from [`policy_labels`], then folds in
/// [`ResolvedTicketing::extra_labels`]: an entry whose name matches a built-in
/// REPLACES it (that is how a project restyles `trusty-mpm`), any other entry
/// is appended in declaration order. A default [`ResolvedTicketing`] — which is
/// what an absent block resolves to — returns exactly [`policy_labels`]'s
/// output, so an absent block changes nothing.
/// Test: `configured_labels_match_builtin_when_block_absent`,
/// `configured_labels_append_extra_labels`,
/// `configured_labels_restyle_a_builtin_by_name`.
pub fn policy_labels_configured(
    ticketing: &ResolvedTicketing,
    session_name: Option<&str>,
) -> Vec<PolicyLabel> {
    let mut out = policy_labels(session_name);
    for label in &ticketing.extra_labels {
        match out.iter_mut().find(|existing| existing.name == label.name) {
            Some(existing) => *existing = label.clone(),
            None => out.push(label.clone()),
        }
    }
    out
}

/// Whether `name` sits in trusty-mpm's OWN label namespace.
///
/// Why: the force policy for `gh label create` turns on exactly this. `ws/` is
/// the framework's namespace, so refreshing its color/description is safe;
/// every other policy label is an ordinary repo label a project may already own
/// and have styled, and `--force` would silently rewrite that on every launch.
/// Test: `owned_namespace_is_ws_only`.
pub fn is_owned_namespace(name: &str) -> bool {
    name.starts_with(WORKSTREAM_PREFIX)
}

/// The `gh label create …` argv — the crate's single spelling of that command.
///
/// Why: #6914 — two independent argv builders (one at session launch, one in
/// `tm issue`) is the duplication that let the launch path keep creating retired
/// labels. Flag order and the omit-when-empty rules now live in one place.
/// What: `label create <name>`, then `--color`/`--description` when non-empty,
/// then `--repo <repo>` when the caller targets a specific repository, then
/// `--force` when the caller owns the label's namespace. Returned owned so the
/// caller can borrow it against either of the crate's two spawn seams.
/// Test: `create_label_argv_full`, `create_label_argv_omits_empty_fields`,
/// `create_label_argv_repo_and_force`.
pub fn create_label_argv(label: &PolicyLabel, repo: Option<&str>, force: bool) -> Vec<String> {
    let mut argv = vec![
        "label".to_string(),
        "create".to_string(),
        label.name.clone(),
    ];
    if !label.color.is_empty() {
        argv.push("--color".to_string());
        argv.push(label.color.clone());
    }
    if !label.description.is_empty() {
        argv.push("--description".to_string());
        argv.push(label.description.clone());
    }
    if let Some(repo) = repo {
        argv.push("--repo".to_string());
        argv.push(repo.to_string());
    }
    if force {
        argv.push("--force".to_string());
    }
    argv
}

/// The `ws/<name>` label name, truncated (with a stable disambiguating suffix)
/// to stay within GitHub's label-name length cap.
///
/// Why: GitHub caps a label name at [`GITHUB_LABEL_MAX_LEN`] characters.
/// Auto-derived session names are always well within that, but an OPERATOR
/// rename (`SessionManager::rename`, up to 64 chars) can produce a name whose
/// `ws/<name>` form overflows the cap — `gh label create`/`--add-label` would
/// then fail with a 400, exactly the failure this policy exists to prevent.
/// Simple truncation risks two DIFFERENT long names colliding on the same
/// truncated label; an 8-hex FNV-1a suffix — hashed from the FULL untruncated
/// name, not the truncated prefix — keeps them apart.
/// What: `ws/<name>` verbatim when it already fits. Otherwise
/// `ws/<truncated-name>-<8-hex-hash>`, where `<truncated-name>` is trimmed to
/// the nearest earlier `char` boundary so a multi-byte character is never
/// split.
/// Test: `label_name_short_is_verbatim`,
/// `label_name_long_is_truncated_with_hash_suffix`,
/// `label_name_stays_within_github_cap`,
/// `label_name_distinct_long_names_get_distinct_labels`.
fn label_name_for(session_name: &str) -> String {
    let full = format!("{WORKSTREAM_PREFIX}{session_name}");
    if full.len() <= GITHUB_LABEL_MAX_LEN {
        return full;
    }
    // 8 lowercase hex chars from the top 32 bits of the FULL name's hash —
    // stable per name, and keeps two names sharing a truncated prefix apart.
    let suffix = format!("{:08x}", (fnv1a_hash(session_name) >> 32) as u32);
    // Budget: "ws/" (3) + truncated name + "-" (1) + suffix (8) <= cap.
    let prefix_budget = GITHUB_LABEL_MAX_LEN - WORKSTREAM_PREFIX.len() - 1 - suffix.len();
    let truncated = truncate_at_char_boundary(session_name, prefix_budget);
    format!("{WORKSTREAM_PREFIX}{truncated}-{suffix}")
}

/// Truncate `s` to at most `max_bytes` bytes, backing off to the nearest
/// earlier `char` boundary so a multi-byte UTF-8 character is never split.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Derive a stable, name-specific 6-hex label color.
///
/// Scheme: FNV-1a (64-bit, chosen over `std`'s `DefaultHasher` because its
/// algorithm is explicitly not guaranteed stable across Rust versions — this
/// color must stay identical for a given name across every future rebuild)
/// hashes `session_name`; the hash mod 360 becomes an HSL hue, with fixed
/// saturation (55%) and lightness (45%) chosen to stay readable as GitHub label
/// text on both light and dark repo themes.
/// Test: `label_color_is_stable_and_valid_hex`,
/// `label_color_differs_across_names`.
fn label_color_for(session_name: &str) -> String {
    let hue = (fnv1a_hash(session_name) % 360) as f64;
    hsl_to_hex(hue, 0.55, 0.45)
}

/// FNV-1a 64-bit hash — deterministic across Rust versions/platforms, unlike
/// `std::collections::hash_map::DefaultHasher`.
fn fnv1a_hash(input: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Convert an HSL color (`h` in `[0, 360)`, `s`/`l` in `[0, 1]`) to an
/// uppercase 6-hex RGB string with no leading `#` (matching `gh label
/// create --color`'s expected form).
fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h_prime as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("{:02X}{:02X}{:02X}", to_byte(r1), to_byte(g1), to_byte(b1))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the policy set ──────────────────────────────────────────────────

    #[test]
    fn convention_label_is_stable() {
        let l = convention_label();
        assert_eq!(l.name, "trusty-mpm");
        assert_eq!(l.color.len(), 6);
        assert!(l.color.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!l.description.is_empty());
    }

    #[test]
    fn policy_labels_includes_workstream() {
        let names: Vec<String> = policy_labels(Some("tm-tcode-01"))
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["trusty-mpm", "ws/tm-tcode-01"]);
    }

    #[test]
    fn policy_labels_without_session_skips_workstream() {
        let names: Vec<String> = policy_labels(None).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["trusty-mpm"]);
    }

    #[test]
    fn policy_labels_blank_session_skips_workstream() {
        let names: Vec<String> = policy_labels(Some("   "))
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["trusty-mpm"]);
    }

    // ── the config-aware successor (#6918) ──────────────────────────────

    #[test]
    fn configured_labels_match_builtin_when_block_absent() {
        // #6918: the whole point of the default — an absent `agents.ticketing`
        // block must leave the seeded set byte-identical to #6914's.
        let default = ResolvedTicketing::default();
        for session in [None, Some("tm-tcode-01")] {
            assert_eq!(
                policy_labels_configured(&default, session),
                policy_labels(session),
                "session={session:?}"
            );
        }
    }

    #[test]
    fn configured_labels_append_extra_labels() {
        let cfg = ResolvedTicketing::default().with_extra_labels(vec![PolicyLabel::new(
            "area/cli",
            "0E8A16",
            "CLI surface",
        )]);
        let names: Vec<String> = policy_labels_configured(&cfg, Some("tm-tcode-01"))
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["trusty-mpm", "ws/tm-tcode-01", "area/cli"]);
    }

    #[test]
    fn configured_labels_restyle_a_builtin_by_name() {
        let cfg = ResolvedTicketing::default().with_extra_labels(vec![PolicyLabel::new(
            CONVENTION_LABEL,
            "FF0000",
            "ours",
        )]);
        let labels = policy_labels_configured(&cfg, None);
        assert_eq!(labels.len(), 1, "restyle must not duplicate: {labels:?}");
        assert_eq!(labels[0].color, "FF0000");
    }

    #[test]
    fn policy_labels_carry_no_retired_lifecycle_pair() {
        // #6914: `in-progress` / `blocked` predate the `status:*` lifecycle and
        // must never be seeded again, from either consumer.
        let names: Vec<String> = policy_labels(Some("tm-tcode-01"))
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert!(!names.iter().any(|n| n == "in-progress" || n == "blocked"));
    }

    #[test]
    fn owned_namespace_is_ws_only() {
        assert!(is_owned_namespace("ws/tm-tcode-01"));
        assert!(!is_owned_namespace("trusty-mpm"));
        assert!(!is_owned_namespace("status:coded"));
    }

    // ── color derivation ────────────────────────────────────────────────

    #[test]
    fn label_color_is_stable_and_valid_hex() {
        let a = label_color_for("tm-tcode-01");
        let b = label_color_for("tm-tcode-01");
        assert_eq!(a, b, "same name must always derive the same color");
        assert_eq!(a.len(), 6);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, a.to_ascii_uppercase(), "color must be uppercase hex");
    }

    #[test]
    fn label_color_differs_across_names() {
        let a = label_color_for("tm-tcode-01");
        let b = label_color_for("tm-dogfood-relaunch-01");
        assert_ne!(a, b, "distinct names should (overwhelmingly likely) differ");
    }

    // ── label name length cap ───────────────────────────────────────────

    #[test]
    fn label_name_short_is_verbatim() {
        assert_eq!(label_name_for("tm-tcode-01"), "ws/tm-tcode-01");
    }

    #[test]
    fn label_name_stays_within_github_cap() {
        // `SessionManager::rename` allows operator-chosen names up to 64
        // chars — well past the 47-char budget `ws/<name>` has under the
        // 50-char GitHub label cap.
        let long_name = "a".repeat(64);
        let label = label_name_for(&long_name);
        assert!(
            label.len() <= GITHUB_LABEL_MAX_LEN,
            "label {label:?} ({} chars) exceeds the {GITHUB_LABEL_MAX_LEN}-char GitHub cap",
            label.len()
        );
    }

    #[test]
    fn label_name_long_is_truncated_with_hash_suffix() {
        let long_name = "a".repeat(64);
        let label = label_name_for(&long_name);
        assert!(label.starts_with("ws/aaaa"), "got {label:?}");
        // "ws/" + 38-char truncated prefix + "-" + 8-hex suffix == 50.
        assert_eq!(label.len(), GITHUB_LABEL_MAX_LEN);
        let suffix = label.rsplit('-').next().expect("has a suffix segment");
        assert_eq!(suffix.len(), 8, "suffix {suffix:?} must be 8 hex chars");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn label_name_distinct_long_names_get_distinct_labels() {
        // Share an identical 60-char prefix (well past the ~38-char truncation
        // budget) and differ only in their last 4 chars — the hash suffix
        // (derived from the FULL name) must still tell them apart even though
        // their truncated prefixes are identical.
        let base = "x".repeat(60);
        let label_a = label_name_for(&format!("{base}1111"));
        let label_b = label_name_for(&format!("{base}2222"));
        assert_ne!(
            label_a, label_b,
            "two long names differing only past the truncation point must not collide"
        );
        assert!(label_a.len() <= GITHUB_LABEL_MAX_LEN);
        assert!(label_b.len() <= GITHUB_LABEL_MAX_LEN);
    }

    // ── the single argv builder ─────────────────────────────────────────

    #[test]
    fn create_label_argv_full() {
        let l = PolicyLabel::new("unicorn:done", "0075CA", "Done");
        assert_eq!(
            create_label_argv(&l, None, false),
            [
                "label",
                "create",
                "unicorn:done",
                "--color",
                "0075CA",
                "--description",
                "Done"
            ]
        );
    }

    #[test]
    fn create_label_argv_omits_empty_fields() {
        let l = PolicyLabel::new("T4", "", "");
        assert_eq!(
            create_label_argv(&l, None, false),
            ["label", "create", "T4"]
        );
    }

    #[test]
    fn create_label_argv_repo_and_force() {
        let l = PolicyLabel::new("ws/x", "0E8A16", "d");
        assert_eq!(
            create_label_argv(&l, Some("owner/repo"), true),
            [
                "label",
                "create",
                "ws/x",
                "--color",
                "0E8A16",
                "--description",
                "d",
                "--repo",
                "owner/repo",
                "--force"
            ]
        );
    }
}
