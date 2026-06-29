//! Human-memorable session naming.
//!
//! Why: tmux sessions were named `trusty-mpm-<full-uuid>`, which is unreadable
//! and impossible to tell apart at a glance — an operator running 16 sessions
//! sees 16 indistinguishable rows. A two-part adjective+noun name (Docker-style)
//! is glanceable, while deriving it deterministically from the UUID keeps it
//! stable and round-trippable (the same session always renders the same name).
//! What: two embedded wordlists and [`name_from_uuid`], which indexes into them
//! using the UUID's 128-bit value so the mapping is pure and deterministic.
//! Test: `cargo test -p trusty-mpm-core` asserts determinism, format, and that
//! distinct UUIDs generally produce distinct names.

use std::path::Path;

use uuid::Uuid;

/// Adjective half of the wordlist (50 short, neutral words).
const ADJECTIVES: &[&str] = &[
    "quiet", "brave", "silent", "swift", "calm", "bold", "bright", "clever", "gentle", "happy",
    "keen", "lively", "merry", "noble", "proud", "rapid", "sharp", "sleek", "steady", "sunny",
    "warm", "wise", "agile", "amber", "azure", "crisp", "deep", "eager", "fancy", "fierce",
    "frosty", "golden", "humble", "icy", "jolly", "lucky", "mellow", "mighty", "misty", "nimble",
    "polar", "royal", "rustic", "shiny", "snowy", "solar", "stark", "vivid", "witty", "zesty",
];

/// Noun half of the wordlist (50 short, neutral words).
const NOUNS: &[&str] = &[
    "falcon", "river", "crane", "otter", "maple", "comet", "harbor", "ember", "willow", "canyon",
    "meadow", "boulder", "cedar", "delta", "fjord", "glade", "harvest", "island", "jungle",
    "lagoon", "marsh", "nebula", "oasis", "pine", "quartz", "ridge", "summit", "tundra", "valley",
    "anchor", "badger", "cobra", "dolphin", "eagle", "ferret", "gibbon", "heron", "ibis", "jaguar",
    "koala", "lynx", "marten", "newt", "osprey", "puffin", "raven", "sparrow", "tiger", "viper",
    "walrus",
];

/// Short prefix kept distinct from the legacy `trusty-mpm-` prefix.
///
/// Why: the full `trusty-mpm-` prefix plus two words would push names past the
/// ~25-char budget; `tmpm-` keeps the result short while staying recognizable.
/// Exposed so the orphan-GC (and any other reconciler) can recognise a
/// trusty-mpm-managed tmux session by its name without hardcoding the literal.
pub const PREFIX: &str = "tmpm-";

/// Legacy long-form prefix for sessions created before the `tmpm-` switch.
///
/// Why: older tmux sessions are still named `trusty-mpm-<uuid>`; the orphan-GC
/// must treat them as managed too so it can reap their orphans, and must never
/// mistake them for unmanaged third-party sessions.
pub const LEGACY_PREFIX: &str = "trusty-mpm-";

/// True if `name` is a trusty-mpm-managed tmux session name.
///
/// Why: the orphan-GC's very first safety gate is "is this even ours?" — only
/// names carrying a managed prefix are ever eligible for reaping. Centralising
/// the check keeps the prefix convention in one place shared with
/// [`crate::core::external_session::SessionOrigin::classify`].
/// What: returns `true` when `name` starts with [`PREFIX`] (`tmpm-`) or the
/// [`LEGACY_PREFIX`] (`trusty-mpm-`).
/// Test: `managed_prefix_matches`, `managed_prefix_rejects_foreign`.
pub fn is_managed_session_name(name: &str) -> bool {
    name.starts_with(PREFIX) || name.starts_with(LEGACY_PREFIX)
}

/// Derive a stable, human-memorable session name from a UUID.
///
/// Why: gives tmux sessions glanceable names while keeping the name a pure
/// function of the session id, so any component can recompute it without a
/// lookup table.
/// What: returns `tmpm-<adjective>-<noun>`, choosing each word by indexing the
/// wordlists with the UUID's 128-bit integer value (modulo each list length).
/// Test: `deterministic`, `format_matches`, `distinct_uuids_distinct_names`.
pub fn name_from_uuid(uuid: &Uuid) -> String {
    let value = uuid.as_u128();
    let adj = ADJECTIVES[(value % ADJECTIVES.len() as u128) as usize];
    // Shift before the second modulo so the adjective and noun are not derived
    // from overlapping low bits (which would correlate the two words).
    let noun = NOUNS[((value / ADJECTIVES.len() as u128) % NOUNS.len() as u128) as usize];
    format!("{PREFIX}{adj}-{noun}")
}

/// Maximum length of the sanitized folder portion of a directory-derived name.
///
/// Why: tmux session names have practical length limits and long names clutter
/// the dashboard; truncating the folder keeps `tmpm-<folder>` glanceable.
const MAX_FOLDER_LEN: usize = 20;

/// Maximum length of the repo-slug portion inside a project-derived session name.
///
/// Why: the full name is `tmpm-<slug>-<8hex>` (5 + 1 + slug + 1 + 8 chars).
/// Capping the slug at 24 keeps the total under 40 chars — well within tmux's
/// practical limit — while still being uniquely identifiable.
const MAX_REPO_SLUG_LEN: usize = 24;

/// Fallback session name used when a directory yields an empty folder slug.
///
/// Why: a path like `/` or `///` sanitizes to nothing; a session still needs a
/// stable, valid tmux name.
const DIR_FALLBACK: &str = "tmpm-session";

/// Sanitize a repository name to a tmux-safe kebab-case slug.
///
/// Why: repo names may contain uppercase letters, dots, underscores, or other
/// characters that are not valid in tmux session names (which forbit `.` and `:`)
/// or that make names hard to type. Centralising the sanitization keeps the
/// `build_managed_session_name` logic pure and testable.
/// What: lowercases the input, maps runs of non-`[a-z0-9]` characters to a
/// single `-`, strips leading/trailing dashes, and caps the result at
/// [`MAX_REPO_SLUG_LEN`] chars (stripping any trailing dash left by the cut).
/// Returns an empty string when nothing alphanumeric survives.
/// Test: `slug_project_sanitizes_dots_colons_uppercase`,
/// `slug_project_collapses_and_trims`, `slug_project_truncates` in the test module.
fn slug_project(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = true; // start true → leading dash dropped
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.len() > MAX_REPO_SLUG_LEN {
        slug.truncate(MAX_REPO_SLUG_LEN);
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    slug
}

/// Build a human-identifiable managed session name incorporating the project.
///
/// Why: `tmpm-<adjective>-<noun>` names are glanceable but tell the operator
/// nothing about which project a session belongs to. Operators running `tmux ls`
/// should be able to tell at a glance which repo each session targets (issue
/// #1789). The short-id suffix ensures uniqueness even when the same repo spawns
/// several parallel sessions.
/// What: returns `tmpm-<slug>-<short-id>` when `project` is `Some` (where
/// `<slug>` is the sanitized repo name and `<short-id>` is the first 8 hex
/// characters of `session_id`). When `project` is `None` (no GitHub remote /
/// not a git repo) returns `tmpm-local-<short-id>` as the fallback.
///
/// Design decisions:
/// - The **repo name only** is used as the slug (not `owner-repo`) because the
///   repo alone is identifiable in most single-org setups; `owner-repo` would
///   push the total length up and clutter `tmux ls`.
/// - The fallback is `tmpm-local-<short-id>` rather than the adjective+noun
///   scheme so operators can distinguish "I know which project this is"
///   (`tmpm-trusty-tools-abc12345`) from "this is a local directory session"
///   (`tmpm-local-abc12345`).
///
/// Test: `build_managed_session_name_github_project`,
/// `build_managed_session_name_none_fallback`,
/// `build_managed_session_name_sanitized_repo` in the test module.
pub fn build_managed_session_name(project: Option<&str>, session_id: &uuid::Uuid) -> String {
    let short_id = &session_id.simple().to_string()[..8];
    match project.map(slug_project) {
        Some(slug) if !slug.is_empty() => format!("{PREFIX}{slug}-{short_id}"),
        _ => format!("{PREFIX}local-{short_id}"),
    }
}

/// Derive a session name from a project directory's basename.
///
/// Why: random adjective+noun names are stable but not meaningful — an operator
/// cannot tell which project a session belongs to from its name. Deriving the
/// name from the project folder (`tmpm-trusty-mpm`) makes sessions instantly
/// identifiable while staying a valid, short tmux session name.
/// What: takes the final path component, lowercases it, replaces every run of
/// non-alphanumeric characters with a single `-`, strips leading/trailing
/// dashes, truncates the slug to [`MAX_FOLDER_LEN`] chars, and returns
/// `tmpm-<slug>`. Falls back to [`DIR_FALLBACK`] when the slug is empty.
/// Test: `name_from_dir_basic`, `name_from_dir_sanitizes`,
/// `name_from_dir_collapses_and_trims`, `name_from_dir_truncates`,
/// `name_from_dir_empty_fallback`.
pub fn name_from_dir(path: &Path) -> String {
    let basename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Replace every non-alphanumeric char with `-`, lowercasing as we go.
    let mut slug = String::with_capacity(basename.len());
    for ch in basename.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else {
            // Non-alphanumeric (spaces, underscores, dots, existing dashes,
            // unicode) all collapse to a dash placeholder.
            slug.push('-');
        }
    }

    // Collapse consecutive dashes and strip leading/trailing dashes.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = true; // start true so a leading dash is dropped
    for ch in slug.chars() {
        if ch == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(ch);
            prev_dash = false;
        }
    }
    while collapsed.ends_with('-') {
        collapsed.pop();
    }

    // Truncate to the folder budget, then re-strip any trailing dash exposed
    // by the cut.
    if collapsed.len() > MAX_FOLDER_LEN {
        collapsed.truncate(MAX_FOLDER_LEN);
        while collapsed.ends_with('-') {
            collapsed.pop();
        }
    }

    if collapsed.is_empty() {
        DIR_FALLBACK.to_string()
    } else {
        format!("{PREFIX}{collapsed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let id = Uuid::parse_str("367c6c51-1025-419c-b6d6-be9a753e8914").unwrap();
        assert_eq!(name_from_uuid(&id), name_from_uuid(&id));
    }

    #[test]
    fn managed_prefix_matches() {
        assert!(is_managed_session_name("tmpm-brave-otter"));
        assert!(is_managed_session_name("trusty-mpm-deadbeef"));
        // Any generated name is, by construction, managed.
        let id = Uuid::parse_str("367c6c51-1025-419c-b6d6-be9a753e8914").unwrap();
        assert!(is_managed_session_name(&name_from_uuid(&id)));
    }

    #[test]
    fn managed_prefix_rejects_foreign() {
        // A bare shell session a developer started by hand must never match.
        assert!(!is_managed_session_name("work"));
        assert!(!is_managed_session_name("0"));
        // The prefix must be a true prefix, not a substring.
        assert!(!is_managed_session_name("my-tmpm-thing"));
        assert!(!is_managed_session_name("not-trusty-mpm-x"));
    }

    #[test]
    fn format_matches() {
        for _ in 0..200 {
            let name = name_from_uuid(&Uuid::new_v4());
            let rest = name.strip_prefix("tmpm-").expect("tmpm- prefix");
            let mut parts = rest.split('-');
            let adj = parts.next().expect("adjective");
            let noun = parts.next().expect("noun");
            assert!(parts.next().is_none(), "exactly two words: {name}");
            assert!(ADJECTIVES.contains(&adj), "adjective from list: {adj}");
            assert!(NOUNS.contains(&noun), "noun from list: {noun}");
            assert!(name.len() <= 25, "name under 25 chars: {name}");
        }
    }

    #[test]
    fn distinct_uuids_distinct_names() {
        // Across many random UUIDs the 2500-name space yields mostly-unique
        // names; assert a healthy unique ratio rather than total uniqueness
        // (collisions are expected by the pigeonhole principle).
        let mut names = std::collections::HashSet::new();
        for _ in 0..500 {
            names.insert(name_from_uuid(&Uuid::new_v4()));
        }
        assert!(
            names.len() > 400,
            "expected mostly-distinct names: {}",
            names.len()
        );
    }

    #[test]
    fn known_uuid_is_stable() {
        // Nil UUID maps to index 0 of both lists — pins the algorithm.
        assert_eq!(name_from_uuid(&Uuid::nil()), "tmpm-quiet-falcon");
    }

    #[test]
    fn name_from_dir_basic() {
        assert_eq!(
            name_from_dir(Path::new("/Users/masa/Projects/trusty-mpm")),
            "tmpm-trusty-mpm"
        );
    }

    #[test]
    fn name_from_dir_sanitizes() {
        // Spaces become dashes; underscores become dashes; result lowercased.
        assert_eq!(
            name_from_dir(Path::new("/home/foo/my project")),
            "tmpm-my-project"
        );
        assert_eq!(name_from_dir(Path::new("/srv/my_api_v2")), "tmpm-my-api-v2");
        assert_eq!(name_from_dir(Path::new("/x/MixedCase")), "tmpm-mixedcase");
    }

    #[test]
    fn name_from_dir_collapses_and_trims() {
        // Multiple separators collapse to one dash; leading/trailing stripped.
        assert_eq!(
            name_from_dir(Path::new("/x/--weird__  name--")),
            "tmpm-weird-name"
        );
        assert_eq!(name_from_dir(Path::new("/x/...dots...")), "tmpm-dots");
    }

    #[test]
    fn name_from_dir_truncates() {
        // The folder slug is capped at 20 chars; no trailing dash remains.
        let name = name_from_dir(Path::new("/x/this-is-a-very-long-folder-name"));
        let slug = name.strip_prefix("tmpm-").expect("tmpm- prefix");
        assert!(slug.len() <= 20, "slug under 20 chars: {slug}");
        assert!(!slug.ends_with('-'), "no trailing dash: {slug}");
        assert_eq!(name, "tmpm-this-is-a-very-long");
    }

    #[test]
    fn name_from_dir_empty_fallback() {
        // Paths that sanitize to nothing fall back to a stable default.
        assert_eq!(name_from_dir(Path::new("/")), "tmpm-session");
        assert_eq!(name_from_dir(Path::new("/x/----")), "tmpm-session");
        assert_eq!(name_from_dir(Path::new("")), "tmpm-session");
    }

    // ── build_managed_session_name ───────────────────────────────────────────

    /// Why: the primary use-case is a GitHub-sourced session where the repo name
    /// is known; the resulting name must be `tmpm-<repo>-<8hex>` and be tmux-safe.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_github_project() {
        let id = Uuid::parse_str("aad055ec-8014-cd16-f000-000000000000").unwrap();
        let name = build_managed_session_name(Some("trusty-tools"), &id);
        assert_eq!(name, "tmpm-trusty-tools-aad055ec");
    }

    /// Why: when no GitHub project can be determined the name must fall back to
    /// `tmpm-local-<8hex>` so the session is identifiable as a local session.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_none_fallback() {
        let id = Uuid::parse_str("aad055ec-8014-cd16-f000-000000000000").unwrap();
        let name = build_managed_session_name(None, &id);
        assert_eq!(name, "tmpm-local-aad055ec");
    }

    /// Why: repo names from GitHub may contain dots (`.`), colons (`:`),
    /// uppercase letters, or other chars tmux session names cannot contain.
    /// slug_project must remove them all.
    /// Test: itself.
    #[test]
    fn slug_project_sanitizes_dots_colons_uppercase() {
        // Dots and colons → dash; uppercase → lowercase.
        assert_eq!(slug_project("My.Repo:v2"), "my-repo-v2");
        // All-uppercase.
        assert_eq!(slug_project("TRUSTY_TOOLS"), "trusty-tools");
        // Leading/trailing separators stripped.
        assert_eq!(slug_project("...repo..."), "repo");
    }

    /// Why: consecutive non-alphanumeric runs must collapse to a single dash
    /// so the slug stays readable.
    /// Test: itself.
    #[test]
    fn slug_project_collapses_and_trims() {
        assert_eq!(slug_project("my__repo--v2"), "my-repo-v2");
        assert_eq!(
            slug_project("--leading-and-trailing--"),
            "leading-and-trailing"
        );
    }

    /// Why: very long repo names must be truncated so the total session name
    /// stays under practical tmux limits; no trailing dash after truncation.
    /// Test: itself.
    #[test]
    fn slug_project_truncates() {
        let long = "this-is-a-very-long-repository-name-that-exceeds-the-cap";
        let slug = slug_project(long);
        assert!(
            slug.len() <= MAX_REPO_SLUG_LEN,
            "slug must be ≤ {MAX_REPO_SLUG_LEN} chars: {slug}"
        );
        assert!(
            !slug.ends_with('-'),
            "no trailing dash after truncation: {slug}"
        );
    }

    /// Why: a sanitized repo slug derived from an `owner/repo` input (the full
    /// `source_id`) must produce the REPO portion only — the slash becomes a
    /// dash, so `bobmatnyc/trusty-tools` → `bobmatnyc-trusty-tools`. Callers
    /// must pass only the repo name to get `tmpm-trusty-tools-<8hex>`.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_sanitized_repo() {
        let id = Uuid::parse_str("aad055ec-8014-cd16-f000-000000000000").unwrap();
        // Uppercase, dots, colons stripped; short-id appended.
        let name = build_managed_session_name(Some("My.Repo:v2"), &id);
        assert_eq!(name, "tmpm-my-repo-v2-aad055ec");
        // An empty-after-sanitize input falls back to local.
        let name2 = build_managed_session_name(Some("..."), &id);
        assert_eq!(name2, "tmpm-local-aad055ec");
    }

    /// Why: collision-freedom guarantee — the short-id suffix must be present
    /// even when multiple sessions target the same repo.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_short_id_present() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let n1 = build_managed_session_name(Some("trusty-tools"), &id1);
        let n2 = build_managed_session_name(Some("trusty-tools"), &id2);
        // Both start with the repo slug.
        assert!(n1.starts_with("tmpm-trusty-tools-"), "n1={n1}");
        assert!(n2.starts_with("tmpm-trusty-tools-"), "n2={n2}");
        // But they differ (with overwhelming probability) because the UUID suffix differs.
        assert_ne!(n1, n2, "distinct UUIDs should produce distinct names");
    }
}
