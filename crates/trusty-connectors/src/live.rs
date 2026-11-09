//! The live lane's scope pre-check (SPEC-OKF-01 criterion 148).
//!
//! Why: the live lane imports a real account. Criterion 148 lets it hold
//! only the read-only preset (trusty-gworkspace `setup --scopes readonly`,
//! ok-T20), so a run with a write-capable token must stop before the first
//! import, and the stop must never become a warning. The check reads what
//! Google says the token carries (tokeninfo), not what the token store
//! recorded at consent, because Google enforces the former.
//! What: [`check_readonly`] is the pure verdict over tokeninfo's `scope`
//! field. [`precheck`] reads the profile's access token from the adapter's
//! own token store (the named profile only, never the default), posts it to
//! [`TOKENINFO_URL`] through the same `BaseClient` the adapters use (so the
//! `--api-base` loopback seam reaches it in tests), and applies the verdict.
//! Test: `check_readonly_accepts_only_the_preset` (below), and
//! `precheck_*` in `tests/live_lane.rs` against the loopback fake.
//!
//! # Spec References
//! - SPEC-OKF-01 §6.2 criterion 148.

use std::path::Path;

use serde_json::Value;
use trusty_gworkspace::api::constants::READONLY_SCOPES;

use crate::google::{Google, Options};

/// Google's tokeninfo endpoint. The token goes in a form body, never in the
/// URL, so it cannot reach a request log.
pub const TOKENINFO_URL: &str = "https://oauth2.googleapis.com/tokeninfo";

// #10746: ok-T21 S13 live lane, criterion 148 pre-check
/// The verdict on tokeninfo's space-separated `scope` field.
///
/// Why: a refusal here is the only thing between the live lane and an
/// import under a write-capable token; it must fail closed.
/// What: `Ok(scopes)` when the field is present, non-empty and every scope
/// is in [`READONLY_SCOPES`]. `Err` names every scope beyond the preset, or
/// says the field is missing or empty. A preset scope the token lacks is not
/// a refusal: it cannot widen what the lane may do.
/// Test: `check_readonly_accepts_only_the_preset`,
/// `precheck_refuses_every_write_scope`.
pub fn check_readonly(scope_field: Option<&str>) -> Result<Vec<String>, String> {
    let scopes: Vec<String> = scope_field
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if scopes.is_empty() {
        return Err(
            "tokeninfo reported no scope for the token; refusing the live lane".to_string(),
        );
    }
    let beyond: Vec<&str> = scopes
        .iter()
        .map(String::as_str)
        .filter(|s| !READONLY_SCOPES.contains(s))
        .collect();
    if beyond.is_empty() {
        Ok(scopes)
    } else {
        Err(format!(
            "the token carries scope(s) beyond the read-only preset: {}; \
             refusing the live lane, no import was run",
            beyond.join(" ")
        ))
    }
}

/// Read `profile`'s token scopes from tokeninfo and apply [`check_readonly`].
///
/// Why: criterion 148 — "before any import it reads the profile's token
/// with tokeninfo".
/// What: builds the adapter's client over the user-tier token store under
/// `home` (routed to `opts.api_base` when the loopback seam is on), takes
/// `profile`'s access token (refreshing it if expired; an absent profile is
/// an error, there is no default), posts it to [`TOKENINFO_URL`] and checks
/// the `scope` field. Any failure on the way is an `Err`.
/// Test: `precheck_passes_the_readonly_preset`,
/// `precheck_refuses_every_write_scope`,
/// `precheck_fails_closed_without_a_verdict`.
pub fn precheck(home: &Path, opts: &Options, profile: &str) -> Result<Vec<String>, String> {
    let google = Google::with_home(home, opts)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("no runtime: {e}"))?;
    let info = rt.block_on(async {
        let token = google
            .client
            .get_access_token(Some(profile))
            .await
            .map_err(|e| format!("profile {profile}: {e:#}"))?;
        let form = format!("access_token={}", form_encode(&token));
        google
            .client
            .post_raw(
                TOKENINFO_URL,
                "application/x-www-form-urlencoded",
                form.into_bytes(),
                Some(profile),
            )
            .await
            .map_err(|e| format!("tokeninfo: {e:#}"))
    })?;
    // BaseClient returns 403/404 as a value with an `error` key.
    if info.get("error").is_some() {
        return Err(format!("tokeninfo refused the token: {info}"));
    }
    check_readonly(info.get("scope").and_then(Value::as_str))
}

/// Percent-encode a form value (RFC 3986 unreserved characters pass).
fn form_encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITE_SCOPES: &[&str] = &[
        "https://www.googleapis.com/auth/gmail.modify",
        "https://www.googleapis.com/auth/gmail.send",
        "https://www.googleapis.com/auth/gmail.settings.basic",
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/drive",
        "https://www.googleapis.com/auth/drive.file",
        "https://www.googleapis.com/auth/documents",
        "https://www.googleapis.com/auth/calendar",
        "https://www.googleapis.com/auth/spreadsheets",
        "https://www.googleapis.com/auth/presentations",
        "https://www.googleapis.com/auth/tasks",
        "https://www.googleapis.com/auth/userinfo.profile",
    ];

    /// Why: the preset passes, every other scope is refused by name, and a
    /// missing field never passes.
    /// What: the exact preset, a subset, each write scope added to the
    /// preset, an empty field and an absent field.
    /// Test: this test.
    #[test]
    fn check_readonly_accepts_only_the_preset() {
        let preset = READONLY_SCOPES.join(" ");
        assert_eq!(check_readonly(Some(&preset)).unwrap(), READONLY_SCOPES);
        assert!(
            check_readonly(Some("openid")).is_ok(),
            "a subset widens nothing"
        );
        for w in WRITE_SCOPES {
            let e = check_readonly(Some(&format!("{preset} {w}"))).unwrap_err();
            assert!(e.contains(w) && e.contains("no import"), "{w}: {e}");
            assert!(
                !e.contains("gmail.readonly"),
                "only the extra is named: {e}"
            );
        }
        assert!(check_readonly(Some("  ")).is_err());
        assert!(check_readonly(None).is_err());
    }

    #[test]
    fn form_encode_escapes_reserved_bytes() {
        assert_eq!(form_encode("ya29.a-b_c~"), "ya29.a-b_c~");
        assert_eq!(form_encode("a+b/c="), "a%2Bb%2Fc%3D");
    }
}
