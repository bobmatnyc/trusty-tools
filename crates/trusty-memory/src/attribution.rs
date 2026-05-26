//! Drawer creator-attribution tag helpers.
//!
//! Why: prior to this module, drawers carried content, room, importance,
//! and free-form tags but no first-class metadata describing the writer.
//! Operators who saw noise drawers in a palace had no way to trace which
//! client wrote them — was it the trusty-memory MCP, a curl from a shell
//! script, claude-mpm's Python hook, the dashboard form? This module
//! defines a reserved `creator:*` tag namespace that every write path
//! (HTTP, MCP, CLI, hook) attaches automatically. With `creator:client=…`
//! present on every drawer, "where did this come from?" becomes
//! grep-able. The namespace approach (vs. a `Drawer` schema change)
//! piggy-backs on the existing `msg:` tag pattern from #99 so no
//! migration is required.
//!
//! What:
//!   - `CREATOR_*_PREFIX` constants — the four reserved tag prefixes.
//!   - [`CreatorInfo`] — small value type carrying client name, version,
//!     source, and optional cwd. `into_tags()` renders the four tag
//!     strings (or three, when cwd is absent) in a stable order.
//!   - [`is_creator_tag`] — predicate used by UI render code that wants
//!     to hide the namespace from the main tag chips (mirroring how
//!     `msg:*` is filtered today).
//!
//! Test: see the `tests` module at the bottom — covers tag composition,
//! prefix detection, and round-trip via `is_creator_tag`.

use crate::ActivitySource;

/// Tag prefix carrying the writing client's short name
/// (e.g. `creator:client=trusty-memory-mcp`).
///
/// Why: the dominant question "who wrote this drawer?" reduces to a
/// single substring search against this prefix. Stable string so curl
/// and grep workflows keep working over time.
/// Test: `creator_info_renders_all_fields`.
pub const CREATOR_CLIENT_PREFIX: &str = "creator:client=";

/// Tag prefix carrying the writing client's version string
/// (e.g. `creator:version=0.5.1`).
///
/// Why: lets operators distinguish "old buggy client wrote this" from
/// "current client wrote this" without rummaging through logs.
/// Test: `creator_info_renders_all_fields`.
pub const CREATOR_VERSION_PREFIX: &str = "creator:version=";

/// Tag prefix carrying the originating subsystem (`http`/`mcp`/`hook`/`cli`).
///
/// Why: same labels as [`ActivitySource`] for HTTP / MCP / hook; CLI is
/// a fourth value we accept here because drawers written from the
/// `trusty-memory send-message` CLI never travel through the activity
/// log emit path but still need attribution.
/// What: lowercase string after the prefix.
/// Test: `creator_info_renders_all_fields`.
pub const CREATOR_SOURCE_PREFIX: &str = "creator:source=";

/// Tag prefix carrying the writing process' cwd at write time
/// (e.g. `creator:cwd=/Users/alice/projects/foo`).
///
/// Why: cwd is the single most useful clue when chasing noise — if a
/// drawer carries `creator:cwd=/Users/alice/projects/claude-mpm`, the
/// operator knows the write came from that working directory and can
/// look at *what* was running there. Absent when the writer could not
/// resolve a cwd (e.g. a remote HTTP client that did not send the
/// optional header).
/// Test: `creator_info_omits_cwd_when_absent`.
pub const CREATOR_CWD_PREFIX: &str = "creator:cwd=";

/// Tag prefix carrying the short session id of the writer (issue #202).
///
/// Why: when a session UUID is already attached as a bare tag, the TUI
/// activity panel cannot easily pick it out of the tag list. Emitting a
/// dedicated `creator:session=<first-8>` tag puts the session shorthand
/// in the same reserved namespace as the rest of the attribution data so
/// the dashboard / TUI can render it without bespoke parsing.
/// What: prefix string; the suffix is the first 8 hex characters of the
/// originating UUID.
/// Test: `session_tag_from_tags_returns_first_uuid_short`.
pub const CREATOR_SESSION_PREFIX: &str = "creator:session=";

/// HTTP request header carrying the writing client's short name.
///
/// Why: lets remote HTTP callers self-identify so the recipient daemon
/// can populate `creator:client=` without guessing. The dashboard /
/// claude-mpm / future trusty-* clients all set this when they make
/// writes; clients that don't get the conservative fallback below.
/// Test: `drawer_creator_attribution_http_default`,
/// `drawer_creator_attribution_http_header`.
pub const X_TRUSTY_CLIENT_NAME: &str = "x-trusty-client-name";

/// HTTP request header carrying the writing client's cwd.
///
/// Why: trusts the caller's self-reported cwd because the daemon has
/// no other way to know it (the HTTP request originates from a remote
/// process whose cwd is opaque). Absent header → `creator:cwd=` is
/// omitted from the drawer tags rather than synthesised from the
/// daemon's own cwd, which would be wrong.
/// Test: `drawer_creator_attribution_http_default`.
pub const X_TRUSTY_CLIENT_CWD: &str = "x-trusty-client-cwd";

/// Default client name used when an HTTP caller omits the
/// `X-Trusty-Client-Name` header.
///
/// Why: every drawer must carry a `creator:client=` tag so the
/// dashboard renders a consistent "client" column; a missing header
/// must not yield a missing tag. The fallback is verbose on purpose so
/// operators can tell "the caller forgot to identify itself" apart from
/// "the caller is a known trusty-* binary".
/// Test: `drawer_creator_attribution_http_default`.
pub const HTTP_DEFAULT_CLIENT: &str = "unknown-http-client";

/// Client name attached to drawers written by the MCP tool surface.
pub const MCP_CLIENT_NAME: &str = "trusty-memory-mcp";

/// Client name attached to drawers written by the `trusty-memory` CLI.
pub const CLI_CLIENT_NAME: &str = "trusty-memory-cli";

/// Client name attached to drawers written by hook-driven code paths.
///
/// Why: hooks currently only read; the constant is reserved here so a
/// future hook that *does* write a drawer (e.g. an inbox auto-archive)
/// would tag itself consistently with the rest of the namespace.
/// Test: `creator_info_renders_all_fields`.
pub const HOOK_CLIENT_NAME: &str = "trusty-memory-hook";

/// Originating-subsystem labels emitted into `creator:source=`.
///
/// Why: matches [`ActivitySource`] for HTTP/MCP/hook plus a fourth `cli`
/// label that has no analogue on the activity-feed source enum (CLI
/// writes go through the HTTP API, but the *origin* of the request was a
/// CLI process; the user wants to see that distinction).
/// What: stable lower-case strings.
/// Test: `creator_info_renders_all_fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorSource {
    Http,
    Mcp,
    Hook,
    Cli,
}

impl CreatorSource {
    /// Stable lower-case string used in the `creator:source=` tag.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Mcp => "mcp",
            Self::Hook => "hook",
            Self::Cli => "cli",
        }
    }
}

impl From<ActivitySource> for CreatorSource {
    fn from(s: ActivitySource) -> Self {
        match s {
            ActivitySource::Http => Self::Http,
            ActivitySource::Mcp => Self::Mcp,
            ActivitySource::Hook => Self::Hook,
        }
    }
}

/// Value type describing the writer of a drawer.
///
/// Why: each write path builds one of these and merges the rendered tags
/// into the caller-supplied tag list before persisting. Keeping the
/// rendering centralised guarantees every write produces tags in the
/// same order with the same prefixes, so curl + grep workflows stay
/// stable.
/// What: holds an owned client name, an owned version string, the source
/// enum, and an optional cwd. `into_tags()` consumes the value and
/// returns the rendered tag list.
/// Test: `creator_info_renders_all_fields`,
/// `creator_info_omits_cwd_when_absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatorInfo {
    pub client: String,
    pub version: String,
    pub source: CreatorSource,
    pub cwd: Option<String>,
}

impl CreatorInfo {
    /// Build a `CreatorInfo` with the supplied client + source, defaulting
    /// the version to this crate's `CARGO_PKG_VERSION` and the cwd to
    /// whatever the writing process has at construction time.
    ///
    /// Why: most call sites want a one-liner; explicit overrides remain
    /// available by mutating the returned value.
    /// What: `client.into()` + `env!("CARGO_PKG_VERSION").into()` +
    /// `std::env::current_dir().ok().map(...)`.
    /// Test: `creator_info_self_populates_version_and_cwd`.
    pub fn new_self(client: impl Into<String>, source: CreatorSource) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        Self {
            client: client.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            source,
            cwd,
        }
    }

    /// Render the rendered tag strings in stable order.
    ///
    /// Why: stable order keeps tests deterministic and gives operators a
    /// predictable layout when they grep through palaces with `jq`.
    /// What: `[client, version, source, cwd?]`. `cwd` is omitted when
    /// absent rather than rendered as an empty string so downstream
    /// consumers can distinguish "writer didn't share a cwd" from
    /// "writer's cwd was literally empty".
    /// Test: `creator_info_renders_all_fields`,
    /// `creator_info_omits_cwd_when_absent`.
    pub fn into_tags(self) -> Vec<String> {
        let mut out = Vec::with_capacity(4);
        out.push(format!("{CREATOR_CLIENT_PREFIX}{}", self.client));
        out.push(format!("{CREATOR_VERSION_PREFIX}{}", self.version));
        out.push(format!("{CREATOR_SOURCE_PREFIX}{}", self.source.as_str()));
        if let Some(cwd) = self.cwd.filter(|c| !c.is_empty()) {
            out.push(format!("{CREATOR_CWD_PREFIX}{cwd}"));
        }
        out
    }

    /// Render the tags and append them to an existing tag list.
    ///
    /// Why: write-path call sites already hold a `Vec<String>` of
    /// user-supplied tags; merging in place avoids an allocation and
    /// preserves the caller's ordering.
    /// What: pushes each rendered tag onto `dst`. Does not deduplicate —
    /// caller is expected to pass a freshly-built or de-duplicated list.
    /// Test: `merge_into_appends_creator_tags`.
    pub fn merge_into(self, dst: &mut Vec<String>) {
        for tag in self.into_tags() {
            dst.push(tag);
        }
    }
}

/// Return `true` when a tag belongs to the `creator:*` reserved namespace.
///
/// Why: render paths (TUI, dashboard) want to hide attribution tags from
/// the main tag chips so they don't clutter the UI alongside meaningful
/// user-supplied tags (same pattern as `msg:*` hiding from #99). A single
/// predicate keeps every renderer in lock-step.
/// What: returns `tag.starts_with("creator:")`.
/// Test: `is_creator_tag_detects_namespace`.
pub fn is_creator_tag(tag: &str) -> bool {
    tag.starts_with("creator:")
}

/// Build a `creator:session=<first-8-chars>` tag from the first bare UUID
/// found in `tags`, if any (issue #202).
///
/// Why: MCP writers (claude-mpm hooks, in particular) already pass the
/// session UUID as a free-form tag in the `tags` array. Turning that into
/// an explicit `creator:session=...` tag puts the session id alongside
/// the rest of the attribution data so the dashboard / TUI can surface
/// it without inspecting every tag for UUID-shaped strings.
/// What: scans the slice in order, parses each entry with
/// `uuid::Uuid::parse_str`, and on the first success returns
/// `Some("creator:session=<first-8-hex>")`. Returns `None` when no entry
/// parses as a UUID, or when the matching tag is itself already a
/// `creator:*` tag (so dashboard-supplied creator tags don't get
/// re-projected).
/// Test: `session_tag_from_tags_returns_first_uuid_short`,
/// `session_tag_from_tags_skips_non_uuid_entries`.
pub fn session_tag_from_tags(tags: &[String]) -> Option<String> {
    for tag in tags {
        // Skip the reserved-namespace tags so a stray
        // `creator:cwd=<uuid-shaped-path>` can never be misinterpreted
        // as a session id. We only consider free-form bare tags.
        if is_creator_tag(tag) {
            continue;
        }
        if let Ok(uuid) = uuid::Uuid::parse_str(tag) {
            // `uuid.simple()` renders as 32 lowercase hex chars; the
            // first 8 are the same characters that appear before the
            // first dash in the hyphenated form. Both forms parse to the
            // same `Uuid`, so we render canonically here for stability.
            let simple = uuid.simple().to_string();
            let short: String = simple.chars().take(8).collect();
            return Some(format!("{CREATOR_SESSION_PREFIX}{short}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: every render path must emit the four tags in stable order
    /// (`client`, `version`, `source`, `cwd`) so dashboards can rely on
    /// the layout. A regression that swapped two would silently change
    /// every downstream consumer's parsing.
    /// What: constructs a `CreatorInfo` with all fields populated and
    /// asserts the rendered list.
    /// Test: itself.
    #[test]
    fn creator_info_renders_all_fields() {
        let info = CreatorInfo {
            client: "qa-curl".into(),
            version: "0.1.2".into(),
            source: CreatorSource::Http,
            cwd: Some("/tmp/proj".into()),
        };
        let tags = info.into_tags();
        assert_eq!(
            tags,
            vec![
                "creator:client=qa-curl".to_string(),
                "creator:version=0.1.2".to_string(),
                "creator:source=http".to_string(),
                "creator:cwd=/tmp/proj".to_string(),
            ]
        );
    }

    /// Why: absent cwd must produce three tags, not four with an empty
    /// `cwd=` — that would force every parser to special-case the empty
    /// suffix. Same for an empty-string cwd.
    /// What: omits cwd and renders; then sets it to "" and renders.
    /// Test: itself.
    #[test]
    fn creator_info_omits_cwd_when_absent() {
        let info = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: None,
        };
        assert_eq!(info.into_tags().len(), 3);

        let info_empty = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: Some(String::new()),
        };
        assert_eq!(info_empty.into_tags().len(), 3);
    }

    /// Why: `new_self` is the one-line convenience entry point most call
    /// sites use; it must populate the version from the crate version and
    /// the cwd from the running process so tests don't have to wire it up
    /// by hand.
    /// What: constructs and asserts version + cwd are non-empty.
    /// Test: itself.
    #[test]
    fn creator_info_self_populates_version_and_cwd() {
        let info = CreatorInfo::new_self("client", CreatorSource::Cli);
        assert!(!info.version.is_empty(), "version must be populated");
        assert!(info.cwd.is_some(), "cwd should resolve in tests");
    }

    /// Why: the merge helper exists so call sites with an existing tag
    /// vec don't have to allocate; the contract is "appends in stable
    /// order".
    /// What: starts with one caller-supplied tag, merges, asserts the
    /// trailing tags are the creator tags in order.
    /// Test: itself.
    #[test]
    fn merge_into_appends_creator_tags() {
        let mut tags = vec!["user-supplied".to_string()];
        CreatorInfo {
            client: "x".into(),
            version: "1".into(),
            source: CreatorSource::Cli,
            cwd: None,
        }
        .merge_into(&mut tags);
        assert_eq!(
            tags,
            vec![
                "user-supplied".to_string(),
                "creator:client=x".to_string(),
                "creator:version=1".to_string(),
                "creator:source=cli".to_string(),
            ]
        );
    }

    /// Why: dashboards / TUI renderers must hide `creator:*` tags from
    /// the main tag chips so the user-supplied tags remain prominent.
    /// What: tests true / false cases against the predicate.
    /// Test: itself.
    #[test]
    fn is_creator_tag_detects_namespace() {
        assert!(is_creator_tag("creator:client=foo"));
        assert!(is_creator_tag("creator:cwd=/tmp"));
        assert!(is_creator_tag(CREATOR_VERSION_PREFIX));
        assert!(!is_creator_tag("user-tag"));
        assert!(!is_creator_tag("msg:v1"));
        assert!(!is_creator_tag("creatorx"));
    }

    /// Why: issue #202 — MCP writers (claude-mpm hooks) commonly pass
    /// the session UUID as a bare tag in the `tags` array. The helper
    /// must pick out the first parseable UUID and emit the short form
    /// in the reserved `creator:session=` namespace so the TUI activity
    /// panel renders it without bespoke parsing.
    /// What: feeds a mixed tag list and asserts the first 8 hex chars
    /// of the UUID round-trip into the returned tag.
    /// Test: itself.
    #[test]
    fn session_tag_from_tags_returns_first_uuid_short() {
        let tags = vec![
            "user-tag".to_string(),
            "01919e90-8a2e-7c1d-9f8b-1234567890ab".to_string(),
            "ignored-second-uuid:11111111-2222-3333-4444-555555555555".to_string(),
        ];
        let session = session_tag_from_tags(&tags).expect("session tag");
        assert_eq!(session, "creator:session=01919e90");
    }

    /// Why: non-UUID entries (free-form tags, scoped tags like `idx:0`)
    /// must not be misinterpreted as session ids — the helper has to
    /// return `None` when no entry parses as a UUID.
    /// What: feeds a tag list with no UUIDs and asserts `None`.
    /// Test: itself.
    #[test]
    fn session_tag_from_tags_skips_non_uuid_entries() {
        let tags = vec![
            "user-tag".to_string(),
            "idx:0".to_string(),
            "session-prefix-not-a-uuid".to_string(),
        ];
        assert!(session_tag_from_tags(&tags).is_none());

        // Empty list returns `None`.
        assert!(session_tag_from_tags(&[]).is_none());
    }

    /// Why: a tag in the reserved `creator:*` namespace must never be
    /// re-projected as a session id, even if its value parses as a UUID.
    /// `creator:cwd=` carrying a UUID-shaped temporary path is the
    /// motivating example.
    /// What: feeds a `creator:` tag whose value parses as a UUID and a
    /// real bare UUID later in the list, then asserts the real one wins.
    /// Test: itself.
    #[test]
    fn session_tag_from_tags_skips_reserved_namespace() {
        let tags = vec![
            // Reserved namespace tag with a UUID-shaped value — must be skipped.
            "creator:cwd=11111111-1111-1111-1111-111111111111".to_string(),
            // The real session tag — must win.
            "22222222-2222-2222-2222-222222222222".to_string(),
        ];
        let session = session_tag_from_tags(&tags).expect("session tag");
        assert_eq!(session, "creator:session=22222222");
    }

    /// Why: the `From<ActivitySource>` impl lets the HTTP path build a
    /// `CreatorSource` from the existing `ActivitySource::Http` without
    /// a manual match; the mapping must be identity for the three shared
    /// variants.
    /// What: round-trips each variant.
    /// Test: itself.
    #[test]
    fn creator_source_from_activity_source() {
        assert_eq!(
            CreatorSource::from(ActivitySource::Http),
            CreatorSource::Http
        );
        assert_eq!(CreatorSource::from(ActivitySource::Mcp), CreatorSource::Mcp);
        assert_eq!(
            CreatorSource::from(ActivitySource::Hook),
            CreatorSource::Hook
        );
    }
}
