//! The mpm chat page the console serves at `/tools/mpm/chat/` (#9979, D96).
//!
//! Why: owner ruling D96 moves the daemon's `/web` chat page to a console
//! route, so the page survives the daemon's TCP bind retiring (#6288).
//! What: one self-contained HTML file embedded with `include_str!` (no
//! bundle, no build step), its no-slash redirect, and [`mpm_chat_guard`], the
//! loopback-only, never-framed layer on the page and on its POST route.
//! Test: the `tests` module below.

use axum::{
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
};

/// The chat page, moved from `trusty-mpm/src/daemon/api/web_chat.html`.
pub const MPM_CHAT_HTML: &str = include_str!("tools_ui_mpm_chat.html");

/// Where the page is served.
pub const MPM_CHAT_PAGE_PATH: &str = "/tools/mpm/chat/";

/// The text a non-loopback caller gets in place of the page or a turn.
const OFF_LOOPBACK_TEXT: &str = "mpm chat is served only to a browser on this machine, \
over the console's loopback address.";

/// `GET /tools/mpm/chat` — redirect to the trailing-slash form, like the
/// other `/tools/<tool>` mounts.
///
/// Test: `bare_chat_path_redirects_to_the_slash_form`.
pub async fn mpm_chat_redirect() -> Redirect {
    Redirect::permanent(MPM_CHAT_PAGE_PATH)
}

/// `GET /tools/mpm/chat/` — the chat page.
///
/// Test: `chat_page_is_served_as_html_to_a_loopback_client`.
pub async fn mpm_chat_page() -> Html<&'static str> {
    Html(MPM_CHAT_HTML)
}

/// The layer on the chat page and on `POST /api/console/sessions/chat`.
///
/// Why: #9979 ruling 2 — the page drives action-capable chat that can spawn
/// and stop sessions, so it gets the Secrets page's posture: no caller off
/// this machine, and no framing by any site.
/// What: a request the tailnet listener tagged, or one whose peer address is
/// known and not loopback (`tools_ui::is_off_loopback`), gets `403` with a
/// text body and never reaches the handler. Every response carries
/// `Content-Security-Policy: frame-ancestors 'none'` and
/// `X-Frame-Options: DENY`.
/// Test: `a_non_loopback_peer_is_refused_the_page_and_the_turn`,
/// `a_tailnet_tagged_turn_is_refused`,
/// `chat_page_is_served_as_html_to_a_loopback_client`.
pub async fn mpm_chat_guard(request: Request, next: Next) -> Response {
    let mut response = if crate::tools_ui::is_off_loopback(&request) {
        tracing::warn!(
            route = request.uri().path(),
            "mpm chat refused a non-loopback client"
        );
        (StatusCode::FORBIDDEN, OFF_LOOPBACK_TEXT).into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

#[cfg(test)]
#[path = "tools_ui_mpm_tests.rs"]
mod tests;
