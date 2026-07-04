//! Human-memorable, lifecycle-managed session naming (shared source of truth).
//!
//! Why (SPEC-ONESM-01 / DOC-33): TWO tmux session managers exist — trusty-mpm's
//! `SessionManager` and trusty-agents' `TmManager`. trusty-mpm's lifecycle
//! management (reconcile / prune / adopt / orphan-GC) recognises a session as
//! "ours" only when its name carries a managed prefix ([`is_managed_session_name`]).
//! Historically this naming lived in `trusty-mpm/src/core/names.rs`, so any
//! session trusty-agents created was named by an INDEPENDENT scheme
//! (`<project>-<harness>-<serial>`) that carried no managed prefix — and was
//! therefore orphaned by every trusty-mpm lifecycle pass. Hoisting the ONE
//! canonical naming rule into `trusty-common` (a crate BOTH already depend on,
//! without trusty-agents needing a `trusty-mpm` edge) lets both managers emit
//! byte-for-byte identical, mutually-recognised names. trusty-mpm re-exports
//! this module verbatim from `core::names` for source compatibility.
//!
//! (Original rationale, retained:)
//! Why: tmux sessions were named `trusty-mpm-<full-uuid>`, which is unreadable
//! and impossible to tell apart at a glance — an operator running 16 sessions
//! sees 16 indistinguishable rows. A two-part adjective+noun name (Docker-style)
//! is glanceable, while deriving it deterministically from the UUID keeps it
//! stable and round-trippable (the same session always renders the same name).
//! Issue #1955 replaced the #1789/#1791 `tmpm-<repo-slug>-<8hex>` scheme with
//! `tm-<project-leaf>-NN`: a per-project two-digit serial that reuses gaps left
//! by decommissioned sessions, so `tmux ls` stays short and glanceable for a
//! daily-driver workflow instead of growing an ever-larger opaque hex suffix.
//! What: two embedded wordlists and [`name_from_uuid`]; folder-basename
//! derivation via [`name_from_dir`]/[`leaf_slug_from_dir`]; and the
//! serial-numbered [`build_managed_session_name`]/[`build_session_name`] pair
//! that is now the primary entry point for `tm session new`.
//! Test: `cargo test -p trusty-common -- session_naming` asserts determinism,
//! format, serial
//! allocation (including gap-reuse and exhaustion), and that legacy prefixes
//! remain recognized by [`is_managed_session_name`].

use std::collections::HashSet;
use std::path::Path;

use thiserror::Error;
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

/// Prefix used by every NEWLY generated managed tmux session name (issue #1955).
///
/// Why: `tm-` is short, matches the `tm` CLI binary name, and reads as
/// unambiguously "trusty-mpm managed" without the extra two characters of the
/// prior `tmpm-` scheme. Every generator function in this module (
/// [`name_from_uuid`], [`name_from_dir`], [`build_managed_session_name`],
/// [`build_session_name`]) emits this prefix; [`LEGACY_PREFIX_TMPM`] and
/// [`LEGACY_PREFIX_FULL`] are recognized on READ (matching/listing/adoption)
/// but never produced for a new session.
pub const PREFIX: &str = "tm-";

/// Prefix used by sessions created between #1789 and #1955
/// (`tmpm-<repo>-<8hex>`, `tmpm-<folder>`, or `tmpm-<adjective>-<noun>`).
///
/// Why: operators may have LIVE tmux sessions named under this scheme at the
/// moment the daemon upgrades to #1955; they must remain recognizable and
/// manageable (list/attach/stop/decommission) by [`is_managed_session_name`]
/// even though no new session is ever created with this prefix again.
pub const LEGACY_PREFIX_TMPM: &str = "tmpm-";

/// Legacy long-form prefix for sessions created before the `tmpm-` switch (#1789).
///
/// Why: even older tmux sessions are named `trusty-mpm-<uuid>`; the orphan-GC
/// must treat them as managed too so it can reap their orphans, and must never
/// mistake them for unmanaged third-party sessions.
pub const LEGACY_PREFIX_FULL: &str = "trusty-mpm-";

/// True if `name` is a trusty-mpm-managed tmux session name (current OR legacy).
///
/// Why: the orphan-GC's very first safety gate is "is this even ours?" — only
/// names carrying a managed prefix are ever eligible for reaping. Centralising
/// the check keeps ALL THREE prefix generations (current `tm-`, #1789-era
/// `tmpm-`, and pre-#1789 `trusty-mpm-`) in one place shared with
/// [`crate::core::external_session::SessionOrigin::classify`], so a daemon
/// upgrade never orphans or mis-classifies a session created under an older
/// scheme.
/// What: returns `true` when `name` starts with [`PREFIX`] (`tm-`),
/// [`LEGACY_PREFIX_TMPM`] (`tmpm-`), or [`LEGACY_PREFIX_FULL`] (`trusty-mpm-`).
/// Test: `managed_prefix_matches`, `managed_prefix_matches_legacy_tmpm`,
/// `managed_prefix_rejects_foreign`.
pub fn is_managed_session_name(name: &str) -> bool {
    name.starts_with(PREFIX)
        || name.starts_with(LEGACY_PREFIX_TMPM)
        || name.starts_with(LEGACY_PREFIX_FULL)
}

/// Strip whichever managed prefix `name` carries (current or legacy), for display.
///
/// Why (#1955): the dashboard/coordinator short-prefix helpers
/// (`tui::dashboard::session_prefix`, `daemon::coordinator::derive_prefix`)
/// need to show an operator-typed short name (`aipowerranking`) instead of the
/// full tmux name. Before #1955 they hardcoded `strip_prefix("tmpm-")`; a
/// session created under the NEW `tm-` scheme (or an old `trusty-mpm-` one)
/// would have shown its prefix un-stripped. Centralising the strip here keeps
/// both display sites — and any future one — correct for all three
/// generations without duplicating the prefix list.
/// What: returns `name` with [`LEGACY_PREFIX_TMPM`], [`PREFIX`], or
/// [`LEGACY_PREFIX_FULL`] removed (whichever matches first); returns `name`
/// unchanged if none match. Deliberately checks [`LEGACY_PREFIX_TMPM`]
/// (`tmpm-`) BEFORE [`PREFIX`] (`tm-`) — longest/most-specific prefix first —
/// even though `tm-` requires a literal trailing dash as its 3rd byte and so
/// can never actually match a `tmpm-` name (`tmpm-`'s 3rd byte is `p`, not
/// `-`; verified by `strip_managed_prefix_no_partial_overlap_with_legacy_tmpm`
/// and a standalone `strip_prefix` check — see the #1966 review thread).
/// Ordering longest-first anyway is defense-in-depth against a FUTURE prefix
/// choice that really does overlap (e.g. if `PREFIX` were ever shortened to
/// bare `"tm"` without the dash), and removes any ambiguity for readers who
/// reasonably expect prefix-matching code to be ordered longest-first.
/// Test: `strip_managed_prefix_strips_current`,
/// `strip_managed_prefix_strips_legacy`, `strip_managed_prefix_passthrough`,
/// `strip_managed_prefix_no_partial_overlap_with_legacy_tmpm`.
pub fn strip_managed_prefix(name: &str) -> &str {
    name.strip_prefix(LEGACY_PREFIX_TMPM)
        .or_else(|| name.strip_prefix(PREFIX))
        .or_else(|| name.strip_prefix(LEGACY_PREFIX_FULL))
        .unwrap_or(name)
}

/// Derive a stable, human-memorable session name from a UUID.
///
/// Why: gives tmux sessions glanceable names while keeping the name a pure
/// function of the session id, so any component can recompute it without a
/// lookup table. Used as the "no project known at all" fallback by the legacy
/// (non-managed) `POST /sessions` registration path.
/// What: returns `tm-<adjective>-<noun>`, choosing each word by indexing the
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
/// the dashboard; truncating the folder keeps `tm-<folder>` glanceable.
const MAX_FOLDER_LEN: usize = 20;

/// Maximum length of the leaf-slug portion inside a project-derived session name.
///
/// Why: the full name is `tm-<slug>-NN` (3 + slug + 1 + 2 chars). Capping the
/// slug at 24 keeps the total under 30 chars — well within tmux's practical
/// limit — while still being uniquely identifiable.
const MAX_REPO_SLUG_LEN: usize = 24;

/// Fallback session name used when a directory yields an empty folder slug.
///
/// Why: a path like `/` or `///` sanitizes to nothing; a session still needs a
/// stable, valid tmux name.
const DIR_FALLBACK: &str = "tm-session";

/// Fallback leaf used when neither a project name nor a directory basename
/// sanitizes to anything usable.
const LOCAL_LEAF_FALLBACK: &str = "local";

/// Highest serial representable in the zero-padded `NN` suffix.
///
/// Why: `NN` is a fixed two-digit field by design (issue #1955) — allowing a
/// third digit would break the glanceable fixed-width convention the ticket
/// asked for. 99 concurrent/undecommissioned sessions for ONE project is
/// already an extreme edge case in daily-driver usage.
const MAX_SERIAL: u8 = 99;

/// Lowercase `input`, collapsing every run of non-alphanumeric characters to a
/// single `-`, stripping leading/trailing dashes, and capping the result at
/// `max_len` characters (re-stripping any trailing dash exposed by the cut).
///
/// Why: both the repo/project-name slug and the directory-basename slug need
/// IDENTICAL sanitization (tmux forbids `.`/`:`, mixed case is hard to type,
/// and very long names clutter `tmux ls`); factoring the shared algorithm out
/// keeps [`slug_project`] and [`leaf_slug_from_dir`] from drifting apart.
/// What: single left-to-right pass building the slug and tracking whether the
/// previous emitted character was a dash, so consecutive separators collapse
/// to one `-` without a second cleanup pass.
/// Test: exercised indirectly via `slug_project_*` and `leaf_slug_from_dir_*`.
fn sanitize_slug(input: &str, max_len: usize) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut prev_dash = true; // start true so a leading dash is dropped
    for ch in input.chars() {
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
    if slug.len() > max_len {
        slug.truncate(max_len);
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    slug
}

/// Sanitize a repository/project name to a tmux-safe kebab-case slug.
///
/// Why: repo names may contain uppercase letters, dots, underscores, or other
/// characters that are not valid in tmux session names (which forbid `.` and
/// `:`) or that make names hard to type. Centralising the sanitization keeps
/// [`build_managed_session_name`] pure and testable. Idempotent: re-slugifying
/// an already-sanitized input returns it unchanged, so callers may pass either
/// a raw project name or a pre-slugified one safely.
/// What: delegates to [`sanitize_slug`] with [`MAX_REPO_SLUG_LEN`]. Returns an
/// empty string when nothing alphanumeric survives.
/// Test: `slug_project_sanitizes_dots_colons_uppercase`,
/// `slug_project_collapses_and_trims`, `slug_project_truncates` in the test module.
fn slug_project(name: &str) -> String {
    sanitize_slug(name, MAX_REPO_SLUG_LEN)
}

/// Sanitize a directory's basename to a tmux-safe kebab-case leaf slug.
///
/// Why (#1955): the project "leaf name" — the example in the ticket is
/// `/Users/masa/Projects/trusty-tools` → `trusty-tools` — is simply the
/// sanitized basename of the project directory. Exposing this separately from
/// [`name_from_dir`] lets [`build_managed_session_name`] combine it with a
/// serial without also inheriting `name_from_dir`'s `tm-` prefix.
/// What: takes `path`'s final component (empty string if absent, e.g. `/`),
/// then sanitizes it via [`sanitize_slug`] capped at [`MAX_FOLDER_LEN`].
/// Returns an empty string when nothing alphanumeric survives — callers decide
/// the fallback (see [`build_managed_session_name`]'s `"local"` fallback).
/// Test: `leaf_slug_from_dir_basic`, `leaf_slug_from_dir_sanitizes`,
/// `leaf_slug_from_dir_empty_for_root`.
pub fn leaf_slug_from_dir(path: &Path) -> String {
    let basename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    sanitize_slug(&basename, MAX_FOLDER_LEN)
}

/// Derive a session name from a project directory's basename (no serial).
///
/// Why: random adjective+noun names are stable but not meaningful — an operator
/// cannot tell which project a session belongs to from its name. Deriving the
/// name from the project folder (`tm-trusty-mpm`) makes sessions instantly
/// identifiable while staying a valid, short tmux session name. Used by the
/// legacy (non-managed) `POST /sessions` registration path and the offline
/// `tm launch` fallback; the managed `tm session new` path uses
/// [`build_managed_session_name`] instead, which adds a per-project serial.
/// What: delegates the basename sanitization to [`leaf_slug_from_dir`] and
/// returns `tm-<slug>`, falling back to [`DIR_FALLBACK`] when the slug is empty.
/// Test: `name_from_dir_basic`, `name_from_dir_sanitizes`,
/// `name_from_dir_collapses_and_trims`, `name_from_dir_truncates`,
/// `name_from_dir_empty_fallback`.
pub fn name_from_dir(path: &Path) -> String {
    let slug = leaf_slug_from_dir(path);
    if slug.is_empty() {
        DIR_FALLBACK.to_string()
    } else {
        format!("{PREFIX}{slug}")
    }
}

/// Errors from allocating a serial-numbered managed session name (issue #1955).
///
/// Why: the `tm-<leaf>-NN` scheme caps the serial at two digits (01-99); when
/// every slot for a project is in use, allocation must fail loudly with an
/// actionable message rather than silently overflowing to three digits
/// (breaking the fixed-width convention) or colliding with an existing name.
/// What: a single variant carrying the project leaf whose serial space is
/// exhausted.
/// Test: `allocate_serial_exhausted_errors`, `build_session_name_exhausted`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SessionNameError {
    /// All 99 serials (`01`-`99`) for this leaf are currently in use.
    #[error(
        "no free session-name serial (01-99) for project '{0}' — decommission or stop an \
         existing `tm-{0}-NN` session before creating another"
    )]
    SerialExhausted(String),
}

/// Parse the `NN` serial suffix from `name` if it is a `tm-<leaf>-NN` name for
/// exactly this `leaf`.
///
/// Why: allocating the next free serial requires knowing which serials are
/// already taken FOR THIS PROJECT specifically — a `tm-other-project-01`
/// session must never block `tm-trusty-tools-01` from being assigned.
/// What: strips the [`PREFIX`] and the `<leaf>-` segment; if what remains is
/// exactly two ASCII digits in `01`-`99` (`00` is rejected — [`allocate_serial`]
/// never hands one out, so a `tm-<leaf>-00` name is foreign/hand-crafted, not
/// one of ours), parses and returns them. Any other shape (wrong leaf,
/// wrong/legacy prefix, non-numeric, non-2-digit, or `00` suffix — e.g. a
/// legacy `tmpm-<repo>-<8hex>` name) returns `None`, so legacy/foreign names
/// never collide with the new serial space.
/// Test: `parse_serial_matches_own_leaf`, `parse_serial_rejects_other_leaf`,
/// `parse_serial_rejects_legacy_hex_suffix`, `parse_serial_rejects_serial_00`.
fn parse_serial_for_leaf(name: &str, leaf: &str) -> Option<u8> {
    let rest = name.strip_prefix(PREFIX)?;
    let rest = rest.strip_prefix(leaf)?;
    let digits = rest.strip_prefix('-')?;
    if digits.len() != 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let serial = digits.parse::<u8>().ok()?;
    if serial == 0 { None } else { Some(serial) }
}

/// Find the lowest unused two-digit serial (01-99) for `leaf`.
///
/// Why (#1955): operators run several concurrent/historical sessions per
/// project; the serial must be stable, glanceable, and REUSE gaps left by
/// decommissioned sessions — e.g. if `01`, `02`, `03` exist and `02` is
/// decommissioned, the NEXT session created for that project gets `02` again,
/// not `04`. This keeps `tmux ls` readable for a daily-driver workflow instead
/// of counting up indefinitely after many short-lived sessions.
/// What: collects the serials already in use for `leaf` from `existing_names`
/// (via [`parse_serial_for_leaf`]), then scans `1..=99` and returns the first
/// serial NOT in that set. Returns [`SessionNameError::SerialExhausted`] if
/// all 99 are taken. Callers decide what counts as "existing" — the session
/// manager unions live tmux names with non-decommissioned store records so a
/// decommissioned session's serial is immediately reusable.
/// Test: `allocate_serial_starts_at_one`, `allocate_serial_reuses_gap`,
/// `allocate_serial_ignores_other_leaf`, `allocate_serial_exhausted_errors`.
pub fn allocate_serial(leaf: &str, existing_names: &[String]) -> Result<u8, SessionNameError> {
    let taken: HashSet<u8> = existing_names
        .iter()
        .filter_map(|n| parse_serial_for_leaf(n, leaf))
        .collect();
    (1..=MAX_SERIAL)
        .find(|n| !taken.contains(n))
        .ok_or_else(|| SessionNameError::SerialExhausted(leaf.to_string()))
}

/// Build a `tm-<leaf>-NN` managed session name from an already-chosen leaf.
///
/// Why: separating "what is the leaf" (a priority decision made by
/// [`build_managed_session_name`]) from "what is the next free serial for that
/// leaf" (this function) keeps each piece independently testable.
/// What: sanitizes `leaf` (idempotent — safe to pass an already-sanitized
/// slug), substituting [`LOCAL_LEAF_FALLBACK`] when sanitization yields
/// nothing, allocates the lowest free serial via [`allocate_serial`], and
/// returns `tm-<leaf>-NN`.
/// Test: `build_session_name_first_serial`, `build_session_name_gap_reuse`,
/// `build_session_name_empty_leaf_falls_back_to_local`,
/// `build_session_name_exhausted`.
pub fn build_session_name(
    leaf: &str,
    existing_names: &[String],
) -> Result<String, SessionNameError> {
    let slug = slug_project(leaf);
    let slug = if slug.is_empty() {
        LOCAL_LEAF_FALLBACK.to_string()
    } else {
        slug
    };
    let serial = allocate_serial(&slug, existing_names)?;
    Ok(format!("{PREFIX}{slug}-{serial:02}"))
}

/// Derive the `tm-<leaf>-NN` name for a new managed session (issue #1955).
///
/// Why: `SessionManager::create_with_id` must turn "what do we know about this
/// session" — an optional GitHub-parsed project name, and the directory the
/// session is rooted in — into ONE canonical leaf, then hand it to
/// [`build_session_name`] for serial allocation. Centralising the leaf-priority
/// decision here keeps the manager's create path simple and testable in
/// isolation from tmux/store state.
/// What: leaf priority is (1) `project` when `Some` and it sanitizes to
/// something non-empty (the GitHub repo name — preferred over `cwd` because
/// for a cloned workspace `cwd` is `<root>/<owner>/<repo>/<session-id>/`, whose
/// basename is the session UUID, not useful); (2) the basename of `cwd`
/// otherwise. [`build_session_name`] further falls back to `"local"` if that
/// also sanitizes to nothing (e.g. `cwd` is `/`). The leaf is combined with the
/// next free per-leaf serial from `existing_names`.
/// Test: `build_managed_session_name_github_project`,
/// `build_managed_session_name_dir_fallback`,
/// `build_managed_session_name_local_fallback`,
/// `build_managed_session_name_sanitized_repo`.
pub fn build_managed_session_name(
    project: Option<&str>,
    cwd: &Path,
    existing_names: &[String],
) -> Result<String, SessionNameError> {
    // Guard against callers passing `owner/repo` instead of just `repo`. The
    // slash would sanitize to a dash and silently produce `owner-repo-NN`,
    // making the name unrecognisably long. This fires only in dev/test builds.
    debug_assert!(
        project.is_none_or(|p| !p.contains('/')),
        "build_managed_session_name: pass the repo segment only, not owner/repo — got {project:?}"
    );
    // Slug the `project` branch once and reuse the result (#1966 review
    // follow-up): computing `slug_project(p)` here and letting
    // `build_session_name` re-derive it internally was harmless (idempotent)
    // but wasteful and easy to misread as two different values. The
    // `leaf_slug_from_dir(cwd)` fallback branch still gets slugified a second
    // time inside `build_session_name` (it calls `slug_project` on whatever
    // leaf it is handed, by design, so every caller — not just this one — gets
    // a guaranteed-sanitized name); that second pass is a no-op here too,
    // since `leaf_slug_from_dir`'s own cap ([`MAX_FOLDER_LEN`] = 20) is
    // already tighter than `slug_project`'s ([`MAX_REPO_SLUG_LEN`] = 24).
    let leaf = match project.map(slug_project) {
        Some(slug) if !slug.is_empty() => slug,
        _ => leaf_slug_from_dir(cwd),
    };
    build_session_name(&leaf, existing_names)
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
        assert!(is_managed_session_name("tm-brave-otter"));
        assert!(is_managed_session_name("trusty-mpm-deadbeef"));
        // Any generated name is, by construction, managed.
        let id = Uuid::parse_str("367c6c51-1025-419c-b6d6-be9a753e8914").unwrap();
        assert!(is_managed_session_name(&name_from_uuid(&id)));
        assert!(is_managed_session_name(
            &build_managed_session_name(Some("trusty-tools"), Path::new("/tmp/x"), &[]).unwrap()
        ));
        assert!(is_managed_session_name(
            &build_managed_session_name(None, Path::new("/"), &[]).unwrap()
        ));
    }

    /// Why (#1955): live sessions created under the pre-#1955 `tmpm-` scheme
    /// must remain recognized as managed after the daemon upgrades, so
    /// list/attach/stop/decommission keep working for them.
    /// Test: itself.
    #[test]
    fn managed_prefix_matches_legacy_tmpm() {
        assert!(is_managed_session_name("tmpm-brave-otter"));
        assert!(is_managed_session_name("tmpm-trusty-tools-abc12345"));
    }

    #[test]
    fn managed_prefix_rejects_foreign() {
        // A bare shell session a developer started by hand must never match.
        assert!(!is_managed_session_name("work"));
        assert!(!is_managed_session_name("0"));
        // The prefix must be a true prefix, not a substring.
        assert!(!is_managed_session_name("my-tm-thing"));
        assert!(!is_managed_session_name("my-tmpm-thing"));
        assert!(!is_managed_session_name("not-trusty-mpm-x"));
    }

    #[test]
    fn strip_managed_prefix_strips_current() {
        assert_eq!(
            strip_managed_prefix("tm-trusty-tools-01"),
            "trusty-tools-01"
        );
    }

    #[test]
    fn strip_managed_prefix_strips_legacy() {
        assert_eq!(
            strip_managed_prefix("tmpm-aipowerranking"),
            "aipowerranking"
        );
        assert_eq!(strip_managed_prefix("trusty-mpm-abc123"), "abc123");
    }

    #[test]
    fn strip_managed_prefix_passthrough() {
        assert_eq!(strip_managed_prefix("frontend"), "frontend");
    }

    /// Why: `PREFIX` (`tm-`) and `LEGACY_PREFIX_TMPM` (`tmpm-`) share the two
    /// leading characters `tm`; a naive `starts_with("tm")` (missing the
    /// trailing dash) or a strip order that checked a `"tm"`/`"tmp"` fragment
    /// before the full `tmpm-` prefix would strip only `tm` off a legacy name,
    /// leaving a mangled `pm-<rest>` instead of `<rest>`. This regression-locks
    /// the exact worked example from issue #1955's review follow-up: a legacy
    /// `tmpm-quiet-falcon` session must display as `quiet-falcon`, never
    /// `pm-quiet-falcon`.
    /// What: asserts the full `tmpm-` prefix is stripped cleanly, not just its
    /// `tm`/`tm-`-shaped prefix overlap with the current [`PREFIX`].
    /// Test: itself.
    #[test]
    fn strip_managed_prefix_no_partial_overlap_with_legacy_tmpm() {
        assert_eq!(strip_managed_prefix("tmpm-quiet-falcon"), "quiet-falcon");
        assert_ne!(strip_managed_prefix("tmpm-quiet-falcon"), "pm-quiet-falcon");
        assert_ne!(strip_managed_prefix("tmpm-quiet-falcon"), "-quiet-falcon");
    }

    /// Why: mirrors the strip-side regression lock above for the boolean
    /// classifier — [`is_managed_session_name`] must recognize a legacy
    /// `tmpm-`-prefixed name as managed via the FULL prefix, not merely
    /// because it happens to also match a shorter/overlapping fragment.
    /// What: asserts classification is stable across the whole `tmpm-` family
    /// used by the #1955 worked example.
    /// Test: itself.
    #[test]
    fn managed_prefix_matches_legacy_tmpm_quiet_falcon() {
        assert!(is_managed_session_name("tmpm-quiet-falcon"));
    }

    #[test]
    fn format_matches() {
        for _ in 0..200 {
            let name = name_from_uuid(&Uuid::new_v4());
            let rest = name.strip_prefix("tm-").expect("tm- prefix");
            let mut parts = rest.split('-');
            let adj = parts.next().expect("adjective");
            let noun = parts.next().expect("noun");
            assert!(parts.next().is_none(), "exactly two words: {name}");
            assert!(ADJECTIVES.contains(&adj), "adjective from list: {adj}");
            assert!(NOUNS.contains(&noun), "noun from list: {noun}");
            assert!(name.len() <= 23, "name under 23 chars: {name}");
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
        assert_eq!(name_from_uuid(&Uuid::nil()), "tm-quiet-falcon");
    }

    #[test]
    fn name_from_dir_basic() {
        assert_eq!(
            name_from_dir(Path::new("/Users/masa/Projects/trusty-mpm")),
            "tm-trusty-mpm"
        );
    }

    #[test]
    fn name_from_dir_sanitizes() {
        // Spaces become dashes; underscores become dashes; result lowercased.
        assert_eq!(
            name_from_dir(Path::new("/home/foo/my project")),
            "tm-my-project"
        );
        assert_eq!(name_from_dir(Path::new("/srv/my_api_v2")), "tm-my-api-v2");
        assert_eq!(name_from_dir(Path::new("/x/MixedCase")), "tm-mixedcase");
    }

    #[test]
    fn name_from_dir_collapses_and_trims() {
        // Multiple separators collapse to one dash; leading/trailing stripped.
        assert_eq!(
            name_from_dir(Path::new("/x/--weird__  name--")),
            "tm-weird-name"
        );
        assert_eq!(name_from_dir(Path::new("/x/...dots...")), "tm-dots");
    }

    #[test]
    fn name_from_dir_truncates() {
        // The folder slug is capped at 20 chars; no trailing dash remains.
        let name = name_from_dir(Path::new("/x/this-is-a-very-long-folder-name"));
        let slug = name.strip_prefix("tm-").expect("tm- prefix");
        assert!(slug.len() <= 20, "slug under 20 chars: {slug}");
        assert!(!slug.ends_with('-'), "no trailing dash: {slug}");
        assert_eq!(name, "tm-this-is-a-very-long");
    }

    #[test]
    fn name_from_dir_empty_fallback() {
        // Paths that sanitize to nothing fall back to a stable default.
        assert_eq!(name_from_dir(Path::new("/")), "tm-session");
        assert_eq!(name_from_dir(Path::new("/x/----")), "tm-session");
        assert_eq!(name_from_dir(Path::new("")), "tm-session");
    }

    // ── leaf_slug_from_dir ────────────────────────────────────────────────

    #[test]
    fn leaf_slug_from_dir_basic() {
        assert_eq!(
            leaf_slug_from_dir(Path::new("/Users/masa/Projects/trusty-tools")),
            "trusty-tools"
        );
    }

    #[test]
    fn leaf_slug_from_dir_sanitizes() {
        assert_eq!(
            leaf_slug_from_dir(Path::new("/x/My_Project.v2")),
            "my-project-v2"
        );
    }

    #[test]
    fn leaf_slug_from_dir_empty_for_root() {
        assert_eq!(leaf_slug_from_dir(Path::new("/")), "");
    }

    // ── slug_project ──────────────────────────────────────────────────────

    /// Why: repo names from GitHub may contain dots (`.`), colons (`:`),
    /// uppercase letters, or other chars tmux session names cannot contain.
    /// slug_project must remove them all.
    /// Test: itself.
    #[test]
    fn slug_project_sanitizes_dots_colons_uppercase() {
        assert_eq!(slug_project("My.Repo:v2"), "my-repo-v2");
        assert_eq!(slug_project("TRUSTY_TOOLS"), "trusty-tools");
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

    // ── allocate_serial / build_session_name ────────────────────────────────

    /// Why: the first session ever created for a project must get serial `01`,
    /// not `00` (the ticket's convention is 1-indexed) or a random value.
    /// Test: itself.
    #[test]
    fn allocate_serial_starts_at_one() {
        assert_eq!(allocate_serial("trusty-tools", &[]).unwrap(), 1);
    }

    /// Why (#1955 worked example): sessions 01, 02, 03 exist for a project;
    /// 02 is decommissioned (removed from the "existing" set the caller
    /// passes in); the NEXT new session must reuse 02, not jump to 04.
    /// Test: itself.
    #[test]
    fn allocate_serial_reuses_gap() {
        let existing = vec![
            "tm-trusty-tools-01".to_string(),
            "tm-trusty-tools-03".to_string(),
        ];
        // 02 was decommissioned and is absent from `existing` — must be reused.
        assert_eq!(allocate_serial("trusty-tools", &existing).unwrap(), 2);
    }

    /// Why: a serial in use by a DIFFERENT project's session must never block
    /// this project's allocation — the serial space is scoped per leaf.
    /// Test: itself.
    #[test]
    fn allocate_serial_ignores_other_leaf() {
        let existing = vec!["tm-other-project-01".to_string()];
        assert_eq!(allocate_serial("trusty-tools", &existing).unwrap(), 1);
    }

    /// Why (#1966 review follow-up): `parse_serial_for_leaf`'s doc comment names
    /// this test directly; it was previously covered only indirectly through
    /// `allocate_serial_*`. Pins the happy path in isolation: a `tm-<leaf>-NN`
    /// name for exactly this leaf must parse to its serial.
    /// Test: itself.
    #[test]
    fn parse_serial_matches_own_leaf() {
        assert_eq!(
            parse_serial_for_leaf("tm-trusty-tools-07", "trusty-tools"),
            Some(7)
        );
    }

    /// Why (#1966 review follow-up): mirrors `allocate_serial_ignores_other_leaf`
    /// but exercises `parse_serial_for_leaf` directly, as its doc comment names.
    /// A name belonging to a different leaf must never parse as a serial for
    /// this one, even when the other leaf is a prefix/suffix-adjacent string.
    /// Test: itself.
    #[test]
    fn parse_serial_rejects_other_leaf() {
        assert_eq!(
            parse_serial_for_leaf("tm-other-project-01", "trusty-tools"),
            None
        );
    }

    /// Why: legacy `tmpm-<repo>-<8hex>` names must never be mistaken for a
    /// `tm-<leaf>-NN` serial (the suffix is 8 hex chars, not 2 digits).
    /// Test: itself.
    #[test]
    fn parse_serial_rejects_legacy_hex_suffix() {
        assert_eq!(
            parse_serial_for_leaf("tmpm-trusty-tools-abc12345", "trusty-tools"),
            None
        );
    }

    /// Why (#1966 review follow-up): [`allocate_serial`] scans `1..=MAX_SERIAL`
    /// and never hands out `00`, so a `tm-<leaf>-00` name is foreign or
    /// hand-crafted, not one this scheme ever produced. Without an explicit
    /// reject, such a name would silently parse as serial `0` and could be
    /// mistaken for a managed session with that serial.
    /// Test: itself.
    #[test]
    fn parse_serial_rejects_serial_00() {
        assert_eq!(
            parse_serial_for_leaf("tm-trusty-tools-00", "trusty-tools"),
            None
        );
    }

    /// Why: when all 99 serials for a project are taken, allocation must fail
    /// with an actionable, typed error rather than panicking or overflowing.
    /// Test: itself.
    #[test]
    fn allocate_serial_exhausted_errors() {
        let existing: Vec<String> = (1..=99u8)
            .map(|n| format!("tm-trusty-tools-{n:02}"))
            .collect();
        let err = allocate_serial("trusty-tools", &existing).unwrap_err();
        assert_eq!(
            err,
            SessionNameError::SerialExhausted("trusty-tools".to_string())
        );
    }

    #[test]
    fn build_session_name_first_serial() {
        assert_eq!(
            build_session_name("trusty-tools", &[]).unwrap(),
            "tm-trusty-tools-01"
        );
    }

    #[test]
    fn build_session_name_gap_reuse() {
        let existing = vec![
            "tm-trusty-tools-01".to_string(),
            "tm-trusty-tools-03".to_string(),
        ];
        assert_eq!(
            build_session_name("trusty-tools", &existing).unwrap(),
            "tm-trusty-tools-02"
        );
    }

    #[test]
    fn build_session_name_empty_leaf_falls_back_to_local() {
        assert_eq!(build_session_name("...", &[]).unwrap(), "tm-local-01");
    }

    #[test]
    fn build_session_name_exhausted() {
        let existing: Vec<String> = (1..=99u8).map(|n| format!("tm-proj-{n:02}")).collect();
        assert!(matches!(
            build_session_name("proj", &existing),
            Err(SessionNameError::SerialExhausted(_))
        ));
    }

    // ── build_managed_session_name ───────────────────────────────────────────

    /// Why: the primary use-case is a GitHub-sourced session where the repo name
    /// is known; the resulting name must be `tm-<repo>-01` on the first call.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_github_project() {
        let name =
            build_managed_session_name(Some("trusty-tools"), Path::new("/tmp/x"), &[]).unwrap();
        assert_eq!(name, "tm-trusty-tools-01");
    }

    /// Why (#1955): the ticket's worked example — a project checked out at
    /// `/Users/masa/Projects/trusty-tools` with no repo_url known — must derive
    /// its leaf from the directory basename, giving `tm-trusty-tools-01`.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_dir_fallback() {
        let name =
            build_managed_session_name(None, Path::new("/Users/masa/Projects/trusty-tools"), &[])
                .unwrap();
        assert_eq!(name, "tm-trusty-tools-01");
    }

    /// Why: when neither a project name nor a usable directory basename is
    /// available (e.g. cwd is `/`), the name must fall back to `tm-local-NN`.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_local_fallback() {
        let name = build_managed_session_name(None, Path::new("/"), &[]).unwrap();
        assert_eq!(name, "tm-local-01");
    }

    /// Why: repo names from GitHub may contain dots (`.`), colons (`:`),
    /// uppercase letters, or other chars tmux session names cannot contain;
    /// `build_managed_session_name` must sanitize them via `slug_project`.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_sanitized_repo() {
        let name =
            build_managed_session_name(Some("My.Repo:v2"), Path::new("/tmp/x"), &[]).unwrap();
        assert_eq!(name, "tm-my-repo-v2-01");
    }

    /// Why (#1955 worked example): the SAME project spawning three concurrent
    /// sessions must get `-01`, `-02`, `-03` in order, each aware of the ones
    /// allocated before it.
    /// Test: itself.
    #[test]
    fn build_managed_session_name_concurrent_sessions_increment() {
        let mut existing: Vec<String> = vec![];
        let n1 = build_managed_session_name(Some("trusty-tools"), Path::new("/tmp/x"), &existing)
            .unwrap();
        assert_eq!(n1, "tm-trusty-tools-01");
        existing.push(n1);
        let n2 = build_managed_session_name(Some("trusty-tools"), Path::new("/tmp/x"), &existing)
            .unwrap();
        assert_eq!(n2, "tm-trusty-tools-02");
        existing.push(n2);
        let n3 = build_managed_session_name(Some("trusty-tools"), Path::new("/tmp/x"), &existing)
            .unwrap();
        assert_eq!(n3, "tm-trusty-tools-03");
    }
}
