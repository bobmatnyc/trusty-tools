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
//!
//! # Spec References
//!
//! - [`SPEC-WSCLAIM-03~draft`](docs/specs/DOC-53-workstream-claim-drawer-convention.md#SPEC-WSCLAIM-03~draft) (§4 workstream-attributed memory)

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

/// Tag prefix carrying the originating tm workstream's name (DOC-53).
///
/// Why: mirrors the rest of the `creator:*` namespace, but for the *workstream*
/// (tm PM session) that wrote the drawer rather than the writing *client*
/// binary. Lets operators (and the claim-drawer convention, DOC-53 §3) answer
/// "which workstream wrote this?" the same grep-able way `creator:client=`
/// answers "which binary wrote this?".
/// What: rendered only when [`resolve_workstream_name`] returns `Some` — never
/// a placeholder value.
/// Test: `creator_info_renders_workstream_tags_when_resolvable`.
pub const CREATOR_WORKSTREAM_PREFIX: &str = "creator:workstream=";

/// Bare, non-namespaced tag carrying the same workstream name as
/// [`CREATOR_WORKSTREAM_PREFIX`] (DOC-53 §4.2).
///
/// Why: `creator:*` tags are hidden from the primary tag chips
/// ([`is_creator_tag`]) and from `memory_list`'s exact-tag filter unless the
/// caller already knows the reserved-prefix form. A first-class `ws:<name>`
/// tag lets `memory_list(tag: "ws:<name>")` find every drawer a workstream
/// touched — both auto-stamped writes (this module) and hand-written claim
/// drawers (DOC-53 §3.1), which use the identical `ws:<name>` tag by
/// convention.
/// What: always rendered together with `creator:workstream=`, never alone.
/// Test: `creator_info_renders_workstream_tags_when_resolvable`.
pub const WORKSTREAM_TAG_PREFIX: &str = "ws:";

/// Environment variable carrying an explicit, human-readable workstream name
/// (DOC-53 §4.3).
///
/// Why: `tm` does not currently export this — the only session-identity
/// environment variable a managed session inherits today is
/// `TM_MANAGED_SESSION_ID` (a UUID). This constant exists so resolution is
/// forward-compatible: the day `tm` starts exporting a human-readable
/// workstream name under this key, [`resolve_workstream_name`] picks it up
/// with no code change here. Until then, resolution falls back to the
/// cwd-derived heuristic (see that function).
/// Test: `resolve_workstream_name_prefers_env_var`.
pub const WORKSTREAM_NAME_ENV: &str = "TM_WORKSTREAM_NAME";

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
    pub workstream: Option<String>,
}

impl CreatorInfo {
    /// Build a `CreatorInfo` with the supplied client + source, defaulting
    /// the version to this crate's `CARGO_PKG_VERSION` and the cwd to
    /// whatever the writing process has at construction time.
    ///
    /// Why: most call sites want a one-liner; explicit overrides remain
    /// available by mutating the returned value.
    /// What: `client.into()` + `env!("CARGO_PKG_VERSION").into()` +
    /// `std::env::current_dir().ok().map(...)` + [`resolve_workstream_name`].
    /// Test: `creator_info_self_populates_version_and_cwd`.
    pub fn new_self(client: impl Into<String>, source: CreatorSource) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let workstream = resolve_own_workstream_name(cwd.as_deref());
        Self {
            client: client.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            source,
            cwd,
            workstream,
        }
    }

    /// Render the rendered tag strings in stable order.
    ///
    /// Why: stable order keeps tests deterministic and gives operators a
    /// predictable layout when they grep through palaces with `jq`.
    /// What: `[client, version, source, cwd?, creator:workstream?, ws?]`.
    /// `cwd` and the workstream pair are each omitted when absent rather
    /// than rendered with an empty/placeholder value, so downstream
    /// consumers can distinguish "writer didn't share this" from "writer's
    /// value was literally empty" (DOC-53 §4.1 — no placeholder ever).
    /// Test: `creator_info_renders_all_fields`,
    /// `creator_info_omits_cwd_when_absent`,
    /// `creator_info_renders_workstream_tags_when_resolvable`,
    /// `creator_info_omits_workstream_tags_when_absent`.
    pub fn into_tags(self) -> Vec<String> {
        let mut out = Vec::with_capacity(6);
        out.push(format!("{CREATOR_CLIENT_PREFIX}{}", self.client));
        out.push(format!("{CREATOR_VERSION_PREFIX}{}", self.version));
        out.push(format!("{CREATOR_SOURCE_PREFIX}{}", self.source.as_str()));
        if let Some(cwd) = self.cwd.filter(|c| !c.is_empty()) {
            out.push(format!("{CREATOR_CWD_PREFIX}{cwd}"));
        }
        if let Some(ws) = self.workstream.filter(|w| is_valid_workstream_name(w)) {
            out.push(format!("{CREATOR_WORKSTREAM_PREFIX}{ws}"));
            out.push(format!("{WORKSTREAM_TAG_PREFIX}{ws}"));
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

/// Resolve a workstream name from a cwd, if any (DOC-53 §4.3).
///
/// Why: the sole heuristic available for a cwd whose owner is not
/// necessarily this process — a `.worktrees/<name>` path segment is already
/// implicit in the cwd used for `creator:cwd=` (self-reported by an MCP/CLI
/// call, or client-reported over HTTP via `X-Trusty-Client-Cwd`), so no new
/// input is required to try it. Deliberately does **not** consult
/// [`WORKSTREAM_NAME_ENV`] — that variable describes *this process'*
/// environment, which is meaningless for a remote HTTP caller's cwd; only
/// [`resolve_own_workstream_name`] (this process' own identity) reads it.
/// What: the path segment immediately following a `.worktrees` component in
/// `cwd`, gated by [`is_valid_workstream_name`] — an invalid candidate
/// (empty, UUID-shaped, unsafe characters, too long) resolves to `None`,
/// never a sanitized substitute.
/// Test: `resolve_workstream_name_falls_back_to_worktrees_cwd_segment`,
/// `resolve_workstream_name_rejects_uuid_shaped_cwd_segment`,
/// `resolve_workstream_name_none_when_unresolvable`.
pub fn resolve_workstream_name(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    let mut components = cwd.split('/');
    while let Some(part) = components.next() {
        if part == ".worktrees" {
            let candidate = components.next()?;
            return is_valid_workstream_name(candidate).then(|| candidate.to_string());
        }
    }
    None
}

/// Resolve *this process'* workstream name (DOC-53 §4.3).
///
/// Why: `tm` does not currently export a human-readable workstream-name
/// environment variable to a managed session (only `TM_MANAGED_SESSION_ID`,
/// a UUID) — see DOC-53 §4.3 for the investigation. Rather than block the
/// feature on a session-launch change (a surface with several in-flight PRs
/// at spec-authoring time), resolution uses only context this process
/// already has: [`WORKSTREAM_NAME_ENV`] first (forward-compatible — the key
/// `tm` would use if it starts exporting one), else the
/// [`resolve_workstream_name`] cwd fallback. An env var that is *set but
/// invalid* is treated as an explicit-but-bad signal and resolves to `None`
/// rather than silently falling through to the cwd the caller never asked
/// for.
/// What: only called from [`CreatorInfo::new_self`] — the MCP/CLI/hook
/// write paths, which run as this process. The HTTP write path
/// (`web::rpc::creator_info_from_http`) calls [`resolve_workstream_name`]
/// directly against the *caller's* reported cwd instead, since this
/// process' own environment says nothing about a remote caller's identity.
/// Test: `resolve_workstream_name_prefers_env_var`,
/// `resolve_workstream_name_invalid_env_var_returns_none`.
fn resolve_own_workstream_name(cwd: Option<&str>) -> Option<String> {
    if let Ok(name) = std::env::var(WORKSTREAM_NAME_ENV) {
        return is_valid_workstream_name(&name).then_some(name);
    }
    resolve_workstream_name(cwd)
}

/// Validate a workstream-name candidate before it is ever rendered into a
/// tag (DOC-53 §4.3).
///
/// Why: two independent hazards must both be rejected — an empty/overlong/
/// unsafe-character candidate would corrupt the tag grammar, and a
/// UUID-shaped candidate (the naming convention for ephemeral/anonymous
/// scratch worktrees, as opposed to named workstream worktrees) would stamp
/// noise indistinguishable from a real workstream name. Rejecting both means
/// the caller omits the tag cleanly rather than sanitizing-and-including.
/// What: non-empty, at most 64 bytes, matches
/// `^[A-Za-z0-9][A-Za-z0-9_.-]*$`, and does not parse as a [`uuid::Uuid`].
/// Test: `is_valid_workstream_name_accepts_slugs`,
/// `is_valid_workstream_name_rejects_uuid_and_unsafe_names`.
pub fn is_valid_workstream_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if uuid::Uuid::parse_str(name).is_ok() {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
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
            workstream: None,
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
            workstream: None,
        };
        assert_eq!(info.into_tags().len(), 3);

        let info_empty = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: Some(String::new()),
            workstream: None,
        };
        assert_eq!(info_empty.into_tags().len(), 3);
    }

    /// Why: `new_self` is the one-line convenience entry point most call
    /// sites use; it must populate the version from the crate version and
    /// the cwd from the running process so tests don't have to wire it up
    /// by hand.
    /// What: constructs and asserts version + cwd are non-empty. Serialises
    /// on [`crate::commands::env_test_lock`] and clears
    /// [`WORKSTREAM_NAME_ENV`] first since `new_self` now also resolves a
    /// workstream name from the environment.
    /// Test: itself.
    #[tokio::test]
    async fn creator_info_self_populates_version_and_cwd() {
        let _guard = crate::commands::env_test_lock().lock().await;
        // SAFETY: serialised by env_test_lock; only this var is touched.
        unsafe {
            std::env::remove_var(WORKSTREAM_NAME_ENV);
        }
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
            workstream: None,
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

    /// Why: `into_tags` must render both the reserved `creator:workstream=`
    /// tag and the ergonomic bare `ws:` tag, in that order, immediately
    /// after `cwd` — DOC-53 §4.1/§4.2.
    /// What: constructs a `CreatorInfo` with a resolvable workstream and
    /// asserts both trailing tags.
    /// Test: itself.
    #[test]
    fn creator_info_renders_workstream_tags_when_resolvable() {
        let info = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: Some("/tmp/proj".into()),
            workstream: Some("feat-ws-memory-claims".into()),
        };
        let tags = info.into_tags();
        assert_eq!(
            tags,
            vec![
                "creator:client=mcp".to_string(),
                "creator:version=0.1.0".to_string(),
                "creator:source=mcp".to_string(),
                "creator:cwd=/tmp/proj".to_string(),
                "creator:workstream=feat-ws-memory-claims".to_string(),
                "ws:feat-ws-memory-claims".to_string(),
            ]
        );
    }

    /// Why: DOC-53 §4.1's "no placeholder, ever" rule must hold both when
    /// `workstream` is `None` (the common case) AND when a caller manually
    /// sets an invalid value — `into_tags` must re-validate rather than
    /// trust its input, since [`CreatorInfo`] is a public struct any caller
    /// can construct directly.
    /// What: `None` renders no trailing tags; an unsafe/UUID-shaped value
    /// also renders none, rather than being sanitized and included.
    /// Test: itself.
    #[test]
    fn creator_info_omits_workstream_tags_when_absent_or_invalid() {
        let absent = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: None,
            workstream: None,
        };
        assert_eq!(absent.into_tags().len(), 3);

        let invalid = CreatorInfo {
            client: "mcp".into(),
            version: "0.1.0".into(),
            source: CreatorSource::Mcp,
            cwd: None,
            workstream: Some("11111111-1111-1111-1111-111111111111".into()),
        };
        assert_eq!(invalid.into_tags().len(), 3);
    }

    /// Why: [`WORKSTREAM_NAME_ENV`] is the forward-compatible primary
    /// source (DOC-53 §4.3) and must win over the cwd fallback whenever
    /// set. Serialises on `env_test_lock` since it mutates process-wide
    /// state.
    /// What: sets the env var, passes an unrelated (even a `.worktrees`-
    /// bearing) cwd, and asserts the env value wins.
    /// Test: itself.
    #[tokio::test]
    async fn resolve_workstream_name_prefers_env_var() {
        let _guard = crate::commands::env_test_lock().lock().await;
        // SAFETY: serialised by env_test_lock; only this var is touched,
        // and it is always cleared before returning.
        unsafe {
            std::env::set_var(WORKSTREAM_NAME_ENV, "explicit-ws");
        }
        let resolved = resolve_own_workstream_name(Some("/x/.worktrees/other-name"));
        unsafe {
            std::env::remove_var(WORKSTREAM_NAME_ENV);
        }
        assert_eq!(resolved, Some("explicit-ws".to_string()));
    }

    /// Why: an env var that is SET but fails validation (DOC-53 §4.3) is an
    /// explicit-but-bad signal — it must resolve to `None`, not silently
    /// fall through to the cwd heuristic the writer never asked for.
    /// What: sets an unsafe env value with a plausible cwd fallback present
    /// and asserts `None`.
    /// Test: itself.
    #[tokio::test]
    async fn resolve_workstream_name_invalid_env_var_returns_none() {
        let _guard = crate::commands::env_test_lock().lock().await;
        // SAFETY: serialised by env_test_lock.
        unsafe {
            std::env::set_var(WORKSTREAM_NAME_ENV, "not a valid name!");
        }
        let resolved = resolve_own_workstream_name(Some("/x/.worktrees/other-name"));
        unsafe {
            std::env::remove_var(WORKSTREAM_NAME_ENV);
        }
        assert_eq!(resolved, None);
    }

    /// Why: the common case — a `tm`-managed PM session running directly in
    /// its own `.worktrees/<name>` checkout — must resolve without any `tm`
    /// changes (DOC-53 §4.3). [`resolve_workstream_name`] is pure cwd-based
    /// (no env var involved, see its doc comment), so no locking is needed.
    /// What: feeds a realistic worktree cwd, asserts the segment
    /// immediately after `.worktrees` is returned.
    /// Test: itself.
    #[test]
    fn resolve_workstream_name_falls_back_to_worktrees_cwd_segment() {
        let resolved = resolve_workstream_name(Some(
            "/Users/bob/trusty-tools/.base/.worktrees/feat-ws-memory-claims",
        ));
        assert_eq!(resolved, Some("feat-ws-memory-claims".to_string()));
    }

    /// Why: ephemeral/anonymous scratch worktrees are named by UUID, not by
    /// workstream (DOC-53 §4.3) — stamping one would produce noise
    /// indistinguishable from a real name, so it must resolve to `None`.
    /// What: feeds a UUID-named `.worktrees/<uuid>` cwd, asserts `None`.
    /// Test: itself.
    #[test]
    fn resolve_workstream_name_rejects_uuid_shaped_cwd_segment() {
        let resolved = resolve_workstream_name(Some(
            "/Users/bob/trusty-tools/.base/.worktrees/2eb72dca-de08-481b-8dfa-22ab7f81b1f9",
        ));
        assert_eq!(resolved, None);
    }

    /// Why: a cwd with no `.worktrees` component (or no cwd at all) is the
    /// honest "can't resolve this" case — DOC-53 §4.1's omit-cleanly rule,
    /// exercised at the resolver level.
    /// What: cwd `None`; then a cwd with no `.worktrees` segment. Both
    /// resolve to `None`.
    /// Test: itself.
    #[test]
    fn resolve_workstream_name_none_when_unresolvable() {
        assert_eq!(resolve_workstream_name(None), None);
        assert_eq!(
            resolve_workstream_name(Some("/Users/bob/some/other/project")),
            None
        );
    }

    /// Why: the validator is the single gate standing between untrusted
    /// input (env var or cwd segment) and a rendered tag — it must accept
    /// ordinary slugs.
    /// What: table of accepted names.
    /// Test: itself.
    #[test]
    fn is_valid_workstream_name_accepts_slugs() {
        for name in [
            "feat-ws-memory-claims",
            "tm_search_eviction_01",
            "a",
            "release.0.20.0",
        ] {
            assert!(is_valid_workstream_name(name), "expected valid: {name}");
        }
    }

    /// Why: the validator must reject both structurally-unsafe candidates
    /// (empty, overlong, unsafe characters, non-alnum leading char) and
    /// UUID-shaped candidates (DOC-53 §4.3's anonymous-worktree exclusion),
    /// since a caller (env var or cwd) is untrusted input.
    /// What: table of rejected names.
    /// Test: itself.
    #[test]
    fn is_valid_workstream_name_rejects_uuid_and_unsafe_names() {
        for name in [
            "",
            "2eb72dca-de08-481b-8dfa-22ab7f81b1f9",
            "-leading-dash",
            "has space",
            "has/slash",
            "has;semicolon",
            &"x".repeat(65),
        ] {
            assert!(!is_valid_workstream_name(name), "expected invalid: {name}");
        }
    }
}
