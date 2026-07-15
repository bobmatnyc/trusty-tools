//! Google Workspace API endpoint constants.
//!
//! Why: Centralise base URLs so service modules don't drift on path roots
//! and we can swap to enterprise endpoints in one place if needed.
//! What: `const &str` for each API root. No string interpolation here —
//! callers append paths.
//! Test: Smoke test asserts URLs parse as valid `reqwest::Url` values.

// Each constant below is a Google API root path; the module-level
// Why/What/Test pattern (above) covers them as a group: they exist so
// service modules don't drift on URL roots, they're plain `const &str`,
// and they're validated by the smoke test in this module.

/// Gmail REST API v1 root.
pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
/// Google Calendar REST API v3 root.
pub const CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";
/// Google Drive REST API v3 root.
pub const DRIVE_API_BASE: &str = "https://www.googleapis.com/drive/v3";
/// Google Drive **upload** endpoint root (multipart/simple/resumable uploads).
pub const DRIVE_UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3/files";
/// Google Docs REST API v1 root.
pub const DOCS_API_BASE: &str = "https://docs.googleapis.com/v1";
/// Google Sheets REST API v4 root.
pub const SHEETS_API_BASE: &str = "https://sheets.googleapis.com/v4";
/// Google Slides REST API v1 root.
pub const SLIDES_API_BASE: &str = "https://slides.googleapis.com/v1";
/// Google Tasks REST API v1 root.
pub const TASKS_API_BASE: &str = "https://tasks.googleapis.com/tasks/v1";
/// OAuth 2.0 token endpoint for refresh requests.
pub const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// OAuth 2.0 userinfo endpoint (email, profile id).
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
/// OAuth 2.0 authorization endpoint (interactive consent — start of PKCE flow).
///
/// Why: The `setup` subcommand must send the user's browser here to grant
/// consent before Google will issue an authorization code.
/// What: Google's v2 auth endpoint; query params (client_id, scope, PKCE
/// challenge, state, redirect_uri) are appended by the consent flow.
/// Test: Compile-time constant; smoke test asserts it parses as a URL.
pub const OAUTH_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Full OAuth scope set requested by the interactive consent flow.
///
/// Why: This MUST stay identical to the Python CLI's scope set so that the
/// tokens minted here are wire-compatible with the existing
/// `~/.gworkspace-mcp/tokens.json` (Google keys refresh tokens by the exact
/// granted scope set; a mismatch would force re-consent and could invalidate
/// a token shared with the Python implementation).
/// What: `openid` plus nine Google API scopes covering userinfo, Calendar,
/// Gmail (modify), Drive, Docs, Tasks, Sheets, and Slides.
/// Test: `oauth::flow::assemble_scope_string` unit test asserts order/content.
pub const OAUTH_SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/drive",
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/tasks",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/presentations",
];

/// Default profile name for token storage — matches Python implementation.
/// Why: Single canonical profile name shared with the Python CLI.
/// What: String constant `"gworkspace-mcp"`.
/// Test: Compile-time constant; no runtime test needed.
pub const DEFAULT_PROFILE: &str = "gworkspace-mcp";
