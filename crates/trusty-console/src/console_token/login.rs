//! `GET`/`POST /login` for the console token gate (#11074, TOK-5).
//!
//! Why: a browser cannot send `Authorization` on a page load, so it trades the
//! token once for a session cookie.
//! What: [`login_form`] serves a static form. [`login_submit`] checks the
//! posted token in constant time; on a match it mints a session and answers
//! `303 /` with a `Set-Cookie` that is `HttpOnly; Secure; SameSite=Strict`,
//! holds only the session id, and expires with the session. A wrong or
//! malformed submission gets the form again with a fixed `401`.
//!
//! `Secure` is always set. The sled path is `tailscale serve` over HTTPS. On
//! plain-HTTP loopback, browsers that treat `127.0.0.1`/`localhost` as a
//! secure context (Chromium, Firefox) accept the cookie; one that does not
//! keeps redirecting to the login page, and the caller uses the `Bearer`
//! header instead.
//! Test: `login_cookie_opens_the_console_and_holds_no_token_11074`,
//! `wrong_login_sets_no_cookie_11074`, `login_form_is_served_unauthenticated_11074`.

use std::sync::Arc;

use axum::Form;
use axum::extract::State;
use axum::extract::rejection::FormRejection;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use super::{SESSION_COOKIE, SESSION_TTL, TokenGate};

/// The login page; `{notice}` is replaced by a fixed message or nothing.
const LOGIN_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Trusty Console login</title>
<meta name="viewport" content="width=device-width, initial-scale=1"></head>
<body style="font-family: system-ui, sans-serif; max-width: 28rem; margin: 4rem auto;">
<h1>Trusty Console</h1>
<p>Paste the console token to sign in.</p>
{notice}<form method="post" action="/login">
<label for="token">Console token</label><br>
<input id="token" name="token" type="password" autocomplete="current-password" required autofocus style="width: 100%;">
<p><button type="submit">Sign in</button></p>
</form></body></html>
"#;

/// The notice a refused submission shows; it never says why.
const REJECTED_NOTICE: &str = "<p role=\"alert\">The token was not accepted.</p>\n";

/// The posted login form.
#[derive(Deserialize)]
pub(super) struct LoginForm {
    token: String,
}

/// `GET /login`: the form.
pub(super) async fn login_form() -> Response {
    page(StatusCode::OK, "")
}

/// `POST /login`: trade the token for a session cookie.
///
/// Test: `login_cookie_opens_the_console_and_holds_no_token_11074`,
/// `wrong_login_sets_no_cookie_11074`, `malformed_login_sets_no_cookie_11074`.
pub(super) async fn login_submit(
    State(gate): State<Arc<TokenGate>>,
    form: Result<Form<LoginForm>, FormRejection>,
) -> Response {
    let accepted = form.is_ok_and(|Form(f)| gate.token.matches(f.token.trim()));
    if !accepted {
        tracing::warn!("console login refused a submission");
        return page(StatusCode::UNAUTHORIZED, REJECTED_NOTICE);
    }
    match gate.sessions.mint() {
        Ok(id) => redirect_to("/", Some(&session_cookie(&id))),
        Err(_) => {
            tracing::error!("console login could not mint a session: OS random source failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "session could not be created",
            )
                .into_response()
        }
    }
}

/// The `Set-Cookie` value for session `id` (TOK-5).
///
/// Test: `login_cookie_opens_the_console_and_holds_no_token_11074`.
pub(super) fn session_cookie(id: &str) -> String {
    format!(
        "{SESSION_COOKIE}={id}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

/// `303 See Other` to `location`, optionally setting a cookie.
pub(super) fn redirect_to(location: &'static str, cookie: Option<&str>) -> Response {
    let mut resp = (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
    if let Some(cookie) = cookie
        && let Ok(mut value) = HeaderValue::from_str(cookie)
    {
        value.set_sensitive(true);
        resp.headers_mut().insert(header::SET_COOKIE, value);
    }
    resp
}

/// The login page with `notice`, at `status`, never cached.
fn page(status: StatusCode, notice: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Html(LOGIN_PAGE.replace("{notice}", notice)),
    )
        .into_response()
}
