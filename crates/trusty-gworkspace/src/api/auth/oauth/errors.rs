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

/// JSON key fragments whose values may be bearer credentials and must never be
/// echoed into an error string.
///
/// Why: A *token* response body (from the `authorization_code` / `refresh_token`
/// exchanges) carries `access_token`, `refresh_token`, and `id_token`. If such a
/// body ever fails to deserialize into the typed struct it would otherwise be
/// embedded verbatim in a `parse token response: {body}` error — leaking the
/// live credential into logs or crash reports. Any key containing one of these
/// fragments (case-insensitive) has its value replaced before surfacing.
/// What: Substrings matched against lower-cased JSON object keys.
/// Test: `redacts_token_values_in_json_body`.
const SENSITIVE_KEY_FRAGMENTS: [&str; 4] = ["token", "secret", "password", "assertion"];

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut.
///
/// Why: Both the unparseable-body fallback and a well-formed but arbitrarily
/// long `error_description` must be length-bounded so a single error cannot
/// balloon a log line unbounded.
/// What: Returns `s` unchanged when within `max` chars, otherwise the first
/// `max` chars followed by `...`.
/// Test: `caps_long_error_description`, `truncates_unparseable_body`.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}...")
    } else {
        s.to_string()
    }
}

/// Recursively replace the value of any credential-named key with `<redacted>`.
///
/// Why: A token-response body is JSON whose credential fields must not survive
/// into an error string; walking the whole tree covers nested objects/arrays.
/// What: For every object entry whose (lower-cased) key contains a
/// [`SENSITIVE_KEY_FRAGMENTS`] substring, replaces its value with the string
/// `<redacted>`; otherwise recurses into the value.
/// Test: `redacts_token_values_in_json_body`.
fn redact_sensitive_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let lower = key.to_ascii_lowercase();
                if SENSITIVE_KEY_FRAGMENTS
                    .iter()
                    .any(|frag| lower.contains(frag))
                {
                    *val = serde_json::Value::String("<redacted>".to_string());
                } else {
                    redact_sensitive_values(val);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_sensitive_values(item);
            }
        }
        _ => {}
    }
}

/// Produce a safe, bounded diagnostic for a *token-response* body that failed to
/// parse, guaranteeing no bearer credential is embedded verbatim.
///
/// Why: On the 2xx success path the token endpoint returns a body containing
/// `access_token`/`refresh_token`. Deserializing it into the typed struct is
/// practically always successful, but if it ever fails the raw body must NOT be
/// echoed into the `parse token response` error — that would surface a live
/// token. This routes every such body through key-based redaction (for JSON) or
/// full suppression (for non-JSON, which could itself be a raw token).
/// What: Parses `body` as JSON and scrubs credential-named values via
/// [`redact_sensitive_values`], returning the bounded scrubbed shape; when the
/// body is not JSON, returns only its byte length (never its content).
/// Test: `redacts_token_values_in_json_body`, `redacts_non_json_token_body`.
pub(crate) fn redact_token_response(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut value) => {
            redact_sensitive_values(&mut value);
            truncate_chars(&value.to_string(), MAX_ERROR_BODY_CHARS)
        }
        Err(_) => format!("<non-JSON body redacted, {} bytes>", body.len()),
    }
}

/// Extract a safe, bounded error message from a Google OAuth error response.
///
/// Why: The raw response body can be arbitrary third-party text (an HTML
/// error page, a verbose diagnostic dump) that ends up inside an `anyhow`
/// error chain and may be printed to the terminal or captured in crash
/// reports. Surfacing only the structured `error`/`error_description` (or a
/// short truncated fallback) keeps that surface bounded and predictable.
/// What: Parses `body` as `{"error": "...", "error_description": "..."}` and
/// caps `error_description` at [`MAX_ERROR_BODY_CHARS`] characters (a well-formed
/// but arbitrarily long description must not surface in full); falls back to a
/// message truncated to the same bound when the body isn't that shape.
/// Test: `sanitizes_oauth_error_json`, `caps_long_error_description`,
/// `truncates_unparseable_body`.
pub(crate) fn sanitize_oauth_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<OAuthErrorBody>(body) {
        return match parsed.error_description {
            Some(desc) => format!(
                "{status} {}: {}",
                parsed.error,
                truncate_chars(&desc, MAX_ERROR_BODY_CHARS)
            ),
            None => format!("{status} {}", parsed.error),
        };
    }
    if body.chars().count() > MAX_ERROR_BODY_CHARS {
        format!(
            "{status} (unparseable error body, truncated): {}",
            truncate_chars(body, MAX_ERROR_BODY_CHARS)
        )
    } else {
        format!("{status} (unparseable error body): {body}")
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
    fn caps_long_error_description() {
        let long_desc = "e".repeat(500);
        let body = format!(r#"{{"error":"invalid_request","error_description":"{long_desc}"}}"#);
        let msg = sanitize_oauth_error(reqwest::StatusCode::BAD_REQUEST, &body);
        assert!(msg.contains("invalid_request"));
        assert!(
            msg.chars().count() < long_desc.len(),
            "error_description must be bounded, got {} chars",
            msg.chars().count()
        );
        assert!(
            msg.contains("..."),
            "truncated description must be marked: {msg}"
        );
    }

    #[test]
    fn redacts_token_values_in_json_body() {
        // A synthetic 2xx token body that fails typed parsing (expires_in is a
        // string, not a number) yet carries live-looking credentials.
        let body = r#"{"access_token":"ya29.SUPER_SECRET_ACCESS","refresh_token":"1//SUPER_SECRET_REFRESH","id_token":"eyJ.SECRET_ID","token_type":"Bearer","expires_in":"not-a-number"}"#;
        let redacted = redact_token_response(body);
        assert!(
            !redacted.contains("SUPER_SECRET_ACCESS"),
            "access_token value must not survive: {redacted}"
        );
        assert!(
            !redacted.contains("SUPER_SECRET_REFRESH"),
            "refresh_token value must not survive: {redacted}"
        );
        assert!(
            !redacted.contains("SECRET_ID"),
            "id_token value must not survive: {redacted}"
        );
        assert!(
            redacted.contains("<redacted>"),
            "credential values must be replaced with a redaction marker: {redacted}"
        );
        // Non-sensitive structure is retained for diagnostics.
        assert!(
            redacted.contains("expires_in"),
            "shape retained: {redacted}"
        );
    }

    #[test]
    fn redacts_non_json_token_body() {
        // A non-JSON body could itself be a raw token; never echo its content.
        let body = "ya29.RAW_TOKEN_STRING_NOT_JSON";
        let redacted = redact_token_response(body);
        assert!(
            !redacted.contains("RAW_TOKEN_STRING_NOT_JSON"),
            "non-JSON body content must not survive: {redacted}"
        );
        assert!(
            redacted.contains("bytes"),
            "reports only length: {redacted}"
        );
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
