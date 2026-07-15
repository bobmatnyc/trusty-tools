//! Shared OAuth error-body parsing and actionable re-auth hints.
//!
//! Why: Both the interactive consent flow (`flow.rs`) and the background token
//! refresh (`manager.rs`) can receive an error response from Google's token
//! endpoint. They must surface it the *same* way — bounded, sanitized, and (for
//! the one recoverable failure mode, a dead refresh token) with an actionable
//! hint naming the exact `setup` command to run. Previously only the consent
//! path sanitized its error body and the refresh path echoed the raw body with
//! no re-auth guidance; centralising the logic here keeps the two in lockstep.
//! What: `sanitize_oauth_error` parses Google's RFC 6749 §5.2 error body and
//! returns a bounded, safe string; `is_invalid_grant` detects the
//! expired/revoked-refresh-token case; `refresh_failure_message` composes the
//! full message the refresh path returns, appending the exact re-auth command
//! (with profile name) when the failure is an `invalid_grant`.
//! Test: `sanitizes_oauth_error_json`, `sanitizes_oauth_error_json_without_description`,
//! `truncates_unparseable_body`, `detects_invalid_grant`, `ignores_non_invalid_grant`,
//! `refresh_failure_message_names_profile_and_setup_command`,
//! `refresh_failure_message_non_invalid_grant_has_no_hint`.

use serde::Deserialize;

/// Google's RFC 6749 §5.2 error-response shape (`error` + optional
/// `error_description`).
///
/// Why: Google returns machine-readable OAuth failures as
/// `{"error": "...", "error_description": "..."}`; parsing that lets us surface
/// only the structured fields instead of an arbitrary response body.
/// What: A minimal serde view over the two fields we care about.
/// Test: Exercised via `sanitizes_oauth_error_json` and `detects_invalid_grant`.
#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Maximum characters of an unparseable error body to surface.
const MAX_ERROR_BODY_CHARS: usize = 200;

/// Extract a safe, bounded error message from a Google OAuth error response.
///
/// Why: The raw response body can be arbitrary third-party text (an HTML
/// error page, a verbose diagnostic dump) that ends up inside an `anyhow`
/// error chain and may be printed to the terminal or captured in crash
/// reports. Surfacing only the structured `error`/`error_description` (or a
/// short truncated fallback) keeps that surface bounded and predictable.
/// What: Parses `body` as `{"error": "...", "error_description": "..."}`;
/// falls back to a message truncated to [`MAX_ERROR_BODY_CHARS`] characters
/// when the body isn't that shape.
/// Test: `sanitizes_oauth_error_json`, `truncates_unparseable_body`.
pub(crate) fn sanitize_oauth_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<OAuthErrorBody>(body) {
        return match parsed.error_description {
            Some(desc) => format!("{status} {}: {desc}", parsed.error),
            None => format!("{status} {}", parsed.error),
        };
    }
    let char_count = body.chars().count();
    let truncated: String = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if char_count > MAX_ERROR_BODY_CHARS {
        format!("{status} (unparseable error body, truncated): {truncated}...")
    } else {
        format!("{status} (unparseable error body): {truncated}")
    }
}

/// True when `body` is a structured OAuth error whose `error` is `invalid_grant`.
///
/// Why: `invalid_grant` is Google's signal that a *refresh token* is expired or
/// revoked — the one refresh failure a user can fix themselves by
/// re-authenticating. Distinguishing it from transient/other failures lets us
/// attach the re-auth hint only when it actually applies (avoiding
/// misattribution on, say, a 500 or a rate-limit).
/// What: Parses `body` as an [`OAuthErrorBody`] and compares its `error` field.
/// Test: `detects_invalid_grant`, `ignores_non_invalid_grant`.
pub(crate) fn is_invalid_grant(body: &str) -> bool {
    serde_json::from_str::<OAuthErrorBody>(body)
        .map(|b| b.error == "invalid_grant")
        .unwrap_or(false)
}

/// Build the error message for a failed refresh-token exchange.
///
/// Why: When a background refresh fails with `invalid_grant` the user's only
/// recovery is to re-run consent; the message must name the EXACT command
/// (including the profile) so the fix is copy-pasteable, while still routing the
/// raw failure through [`sanitize_oauth_error`] for a bounded diagnostic. For
/// any other failure we return a sanitized error without the re-auth hint, so a
/// transient/server error is never misattributed to a dead token.
/// What: For `invalid_grant`, returns
/// `Google refresh token for profile '<name>' is expired or revoked —
/// re-authenticate with: gworkspace-mcp setup --profile <name> (<sanitized>)`;
/// otherwise returns `token refresh failed: <sanitized>`.
/// Test: `refresh_failure_message_names_profile_and_setup_command`,
/// `refresh_failure_message_non_invalid_grant_has_no_hint`.
pub(crate) fn refresh_failure_message(
    status: reqwest::StatusCode,
    body: &str,
    profile: &str,
) -> String {
    let sanitized = sanitize_oauth_error(status, body);
    if is_invalid_grant(body) {
        format!(
            "Google refresh token for profile '{profile}' is expired or revoked — \
             re-authenticate with: gworkspace-mcp setup --profile {profile} ({sanitized})"
        )
    } else {
        format!("token refresh failed: {sanitized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_oauth_error_json() {
        let msg = sanitize_oauth_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"Bad Request"}"#,
        );
        assert!(msg.contains("invalid_grant"));
        assert!(msg.contains("Bad Request"));
    }

    #[test]
    fn sanitizes_oauth_error_json_without_description() {
        let msg = sanitize_oauth_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant"}"#,
        );
        assert!(msg.contains("invalid_grant"));
    }

    #[test]
    fn truncates_unparseable_body() {
        let long_body = "x".repeat(500);
        let msg = sanitize_oauth_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &long_body);
        assert!(
            msg.len() < long_body.len(),
            "message must be shorter than the raw body"
        );
        assert!(msg.contains("truncated"));
    }

    #[test]
    fn detects_invalid_grant() {
        assert!(is_invalid_grant(
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
        ));
    }

    #[test]
    fn ignores_non_invalid_grant() {
        assert!(!is_invalid_grant(r#"{"error":"invalid_client"}"#));
        assert!(!is_invalid_grant("not json at all"));
        assert!(!is_invalid_grant("<html>500</html>"));
    }

    #[test]
    fn refresh_failure_message_names_profile_and_setup_command() {
        let msg = refresh_failure_message(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
            "work",
        );
        // Actionable hint must name the profile AND the exact setup command.
        assert!(
            msg.contains("profile 'work'"),
            "message must name the profile: {msg}"
        );
        assert!(
            msg.contains("gworkspace-mcp setup --profile work"),
            "message must name the exact re-auth command: {msg}"
        );
        assert!(
            msg.contains("expired or revoked"),
            "message must explain the cause: {msg}"
        );
        // Sanitized diagnostic is still routed through.
        assert!(
            msg.contains("invalid_grant"),
            "sanitized body retained: {msg}"
        );
    }

    #[test]
    fn refresh_failure_message_non_invalid_grant_has_no_hint() {
        let msg = refresh_failure_message(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"internal_failure"}"#,
            "work",
        );
        assert!(
            !msg.contains("setup --profile"),
            "non-invalid_grant failure must NOT suggest re-auth: {msg}"
        );
        assert!(
            msg.contains("token refresh failed"),
            "must stay a sanitized error: {msg}"
        );
        assert!(msg.contains("internal_failure"));
    }
}
