//! Google OAuth scope URL -> dotted RBAC scope mapping (#3577).
//!
//! Why: `trusty-gworkspace`'s `rpc.discover` advertises each tool's
//! required permissions as `x-google-scopes`: an array of full Google
//! OAuth 2.0 scope URLs (`https://www.googleapis.com/auth/gmail.modify`).
//! Everything downstream of discovery — `ScopePattern` matching in
//! `scope.rs`, the operator-declared `[[endpoints]].scopes` allowlist, and
//! the agent-level `[tools].scopes` enforcement fed by PR #3576 — operates
//! on `DiscoveredTool.scope`, a single dotted string like
//! `google.gmail.write`. This module is the single place that bridges the
//! two vocabularies, so the mapping is auditable and testable in isolation
//! from the wire-parsing code in `discovery.rs`.
//!
//! What: A fixed, literal table of every OAuth scope URL
//! `trusty-gworkspace` actually requests (`crates/trusty-gworkspace/src/
//! openrpc.rs::scopes` — a closed, small set), each mapped to a
//! `(family, access)` pair. `dotted_scope_for_google_scopes` resolves a
//! tool's full scope-URL list down to ONE dotted string (`DiscoveredTool`
//! carries a single `scope`, not a set) by walking the table in priority
//! order and returning the first match — see "Priority" below.
//!
//! Read vs. write: every `trusty-gworkspace` OAuth scope except
//! `userinfo.profile` is the FULL-ACCESS variant of its Google API (never
//! a `.readonly` scope) — see the literal constants in
//! `trusty-gworkspace/src/openrpc.rs::scopes`. Classifying by tool
//! behavior instead (e.g. `search_gmail_messages` "feels" read-only) would
//! understate what the underlying OAuth grant actually permits: the same
//! `gmail.modify` token that a "read" tool holds can also delete mail.
//! Scope is a trust boundary computed once at discovery time from the
//! wire payload — it must reflect what the credential CAN do, not what
//! today's call happens to do, or a future behavior change silently
//! escapes a `google.*.read`-gated policy. `userinfo.profile` is the one
//! scope that is genuinely read-only by Google's own definition (no write
//! capability exists for it), so it is the only `Read` entry.
//!
//! Priority: `create_document_from_template` and friends request BOTH a
//! resource-specific scope (`documents`) and the generic `drive` scope
//! (Docs/Sheets/Slides need Drive-level access for file operations). The
//! table lists narrow, resource-specific scopes before the generic `drive`
//! catch-all, so `dotted_scope_for_google_scopes` prefers `google.docs.*`
//! over `google.drive.*` for a Docs tool — the operator-facing scope
//! should name the resource the tool actually operates on, not the
//! incidental broader grant it also needs.
//!
//! Test: `family_and_access_examples`, `priority_prefers_specific_over_drive`,
//! `unknown_scope_returns_none`, `empty_scopes_returns_none`.

/// Read vs. write classification, mirrored into the dotted scope's last
/// segment (`google.gmail.read` / `google.gmail.write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeAccess {
    Read,
    Write,
}

impl ScopeAccess {
    fn as_str(self) -> &'static str {
        match self {
            ScopeAccess::Read => "read",
            ScopeAccess::Write => "write",
        }
    }
}

/// `(OAuth scope URL, dotted family, access)`, ordered by priority: entries
/// earlier in the list win when a tool advertises multiple scopes. See the
/// module-level "Priority" note for why resource-specific scopes precede
/// the generic `drive` catch-all.
const FAMILY_TABLE: &[(&str, &str, ScopeAccess)] = &[
    (
        "https://www.googleapis.com/auth/documents",
        "docs",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/spreadsheets",
        "sheets",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/presentations",
        "slides",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/calendar.events",
        "calendar",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/calendar",
        "calendar",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/gmail.send",
        "gmail",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/gmail.modify",
        "gmail",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/gmail.labels",
        "gmail",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/gmail.settings.basic",
        "gmail",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/drive",
        "drive",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/tasks",
        "tasks",
        ScopeAccess::Write,
    ),
    (
        "https://www.googleapis.com/auth/userinfo.profile",
        "accounts",
        ScopeAccess::Read,
    ),
];

/// Every dotted scope this build can EVER emit for the `google` namespace.
///
/// Why (#3987): `dead_scope::dead_scope_patterns` needs to know the closed
/// vocabulary an agent's `[tools].scopes` patterns are matched against, and
/// for Google that vocabulary is a compile-time property of
/// [`FAMILY_TABLE`] — NOT of whether a `gworkspace` endpoint happens to be
/// reachable right now. Deriving it here rather than only from a live
/// `rpc.discover` is what makes the diagnostic sound: `google.read` is dead
/// because no table entry can ever produce it, a fact that holds with the
/// daemon down, and a pattern must never be reported dead merely because an
/// endpoint is offline.
/// What: the de-duplicated `google.<family>.<access>` set produced by
/// walking [`FAMILY_TABLE`], in table (priority) order with duplicates
/// dropped — several OAuth URLs collapse to the same dotted scope (the
/// four Gmail scopes all yield `google.gmail.write`).
/// Test: `known_dotted_scopes_covers_every_family`,
/// `known_dotted_scopes_never_contains_the_dead_google_read_pattern`.
pub fn known_dotted_scopes() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, family, access) in FAMILY_TABLE {
        let dotted = format!("google.{family}.{}", access.as_str());
        if !out.contains(&dotted) {
            out.push(dotted);
        }
    }
    out
}

/// Resolve a tool's `x-google-scopes` list to one dotted scope string.
///
/// Why: `DiscoveredTool.scope` is a single `String` (the shape every
/// downstream consumer — `ScopePattern`, endpoint allowlists, agent-level
/// enforcement — already matches against); a tool's OAuth grant can list
/// several scope URLs, so this collapses that set deterministically.
/// What: Walks `FAMILY_TABLE` in priority order and returns
/// `Some("google.<family>.<access>")` for the first entry whose URL is
/// present in `scopes`. Returns `None` when `scopes` is empty or contains
/// no URL this table recognizes — callers must treat `None` as "no scope
/// could be derived" and surface that loudly rather than silently
/// defaulting to an empty string that would vacuously fail every
/// `ScopePattern` match.
/// Test: `family_and_access_examples`, `priority_prefers_specific_over_drive`.
pub fn dotted_scope_for_google_scopes(scopes: &[String]) -> Option<String> {
    if scopes.is_empty() {
        return None;
    }
    FAMILY_TABLE
        .iter()
        .find(|(url, _, _)| scopes.iter().any(|s| s == url))
        .map(|(_, family, access)| format!("google.{family}.{}", access.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn family_and_access_examples() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&["https://www.googleapis.com/auth/gmail.modify"])),
            Some("google.gmail.write".to_string())
        );
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&["https://www.googleapis.com/auth/calendar"])),
            Some("google.calendar.write".to_string())
        );
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&["https://www.googleapis.com/auth/tasks"])),
            Some("google.tasks.write".to_string())
        );
    }

    /// `userinfo.profile` is Google's one genuinely read-only scope in
    /// this table — no write capability exists for it, unlike every other
    /// entry which is the full-access (never `.readonly`) variant.
    #[test]
    fn userinfo_profile_is_read() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&[
                "https://www.googleapis.com/auth/userinfo.profile"
            ])),
            Some("google.accounts.read".to_string())
        );
    }

    /// `compose_email` requests both `gmail.send` and `gmail.modify`
    /// (`trusty-gworkspace/src/openrpc.rs::scopes_for_tool`); both map to
    /// the same family/access, so the result is stable regardless of
    /// table order within the gmail cluster.
    #[test]
    fn multiple_scopes_same_family_collapse_to_one() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&[
                "https://www.googleapis.com/auth/gmail.send",
                "https://www.googleapis.com/auth/gmail.modify",
            ])),
            Some("google.gmail.write".to_string())
        );
    }

    /// Docs/Sheets/Slides tools request BOTH their resource-specific scope
    /// AND the generic `drive` scope. The resource-specific scope must
    /// win so the RBAC-facing family names the resource, not the
    /// incidental Drive-level grant.
    #[test]
    fn priority_prefers_specific_over_drive() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&[
                "https://www.googleapis.com/auth/documents",
                "https://www.googleapis.com/auth/drive",
            ])),
            Some("google.docs.write".to_string())
        );
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&[
                "https://www.googleapis.com/auth/spreadsheets",
                "https://www.googleapis.com/auth/drive",
            ])),
            Some("google.sheets.write".to_string())
        );
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&[
                "https://www.googleapis.com/auth/presentations",
                "https://www.googleapis.com/auth/drive",
            ])),
            Some("google.slides.write".to_string())
        );
    }

    #[test]
    fn drive_only_tool_maps_to_drive() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&["https://www.googleapis.com/auth/drive"])),
            Some("google.drive.write".to_string())
        );
    }

    #[test]
    fn empty_scopes_returns_none() {
        assert_eq!(dotted_scope_for_google_scopes(&[]), None);
    }

    #[test]
    fn unknown_scope_returns_none() {
        assert_eq!(
            dotted_scope_for_google_scopes(&s(&["https://www.googleapis.com/auth/unknown.thing"])),
            None
        );
    }

    /// #3987: the vocabulary must name every family the table maps, and must
    /// collapse the several OAuth URLs that share a family/access pair.
    #[test]
    fn known_dotted_scopes_covers_every_family() {
        let vocab = known_dotted_scopes();
        for expected in [
            "google.docs.write",
            "google.sheets.write",
            "google.slides.write",
            "google.calendar.write",
            "google.gmail.write",
            "google.drive.write",
            "google.tasks.write",
            "google.accounts.read",
        ] {
            assert!(
                vocab.iter().any(|s| s == expected),
                "vocabulary is missing {expected}: {vocab:?}"
            );
        }
        let mut deduped = vocab.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            vocab.len(),
            "vocabulary must be deduplicated"
        );
    }

    /// #3987 the point of the whole exercise: `google.read` — the base
    /// assistant's declared pattern — is not in the vocabulary and cannot be,
    /// because `FAMILY_TABLE` only ever emits three-segment scopes.
    #[test]
    fn known_dotted_scopes_never_contains_the_dead_google_read_pattern() {
        assert!(!known_dotted_scopes().iter().any(|s| s == "google.read"));
    }
}
