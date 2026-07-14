//! Loopback OAuth redirect receiver for the PKCE consent flow.
//!
//! Why: Google's installed-app flow redirects the browser to
//! `http://127.0.0.1:<port>/...` after consent; we need a throwaway local
//! HTTP endpoint to capture the `code` (or `error`) and hand it back to the
//! flow. A full HTTP framework is overkill for one request, so this speaks
//! just enough HTTP/1.1 by hand over a raw `TcpStream`.
//! Why (security): the listener binds only to `127.0.0.1` (loopback), never a
//! routable interface, so no other host can reach the redirect endpoint.
//! What: `bind_loopback` grabs a free ephemeral port; `wait_for_code` blocks
//! for one request, parses the redirect query, validates `state`, and writes
//! a human-readable HTML response back to the browser.
//! Test: `mod tests` covers free-port binding and request-line parsing (the
//! pure part); the blocking accept loop is exercised by an end-to-end test
//! that connects a client socket to the bound listener.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result, anyhow};

use super::pkce::parse_callback_query;

/// Bind a fresh loopback TCP listener on an OS-chosen free port.
///
/// Why: The redirect URI must point at a port we actually own; asking the OS
/// for port 0 avoids racing against a hard-coded port already in use.
/// What: Binds `127.0.0.1:0` and returns the listener (its `local_addr` gives
/// the real port). Loopback-only by construction.
/// Test: `bind_returns_usable_port`.
pub fn bind_loopback() -> Result<TcpListener> {
    TcpListener::bind("127.0.0.1:0").context("bind loopback callback listener")
}

/// The redirect URI a listener is serving, e.g. `http://127.0.0.1:54123`.
///
/// Why: This exact string must be sent as `redirect_uri` in both the
/// authorization request and the token exchange — Google requires them to
/// match byte-for-byte.
/// What: Formats the listener's bound port into a loopback `http://` URI.
/// Test: `redirect_uri_reflects_bound_port`.
pub fn redirect_uri(listener: &TcpListener) -> Result<String> {
    let port = listener
        .local_addr()
        .context("read listener local_addr")?
        .port();
    Ok(format!("http://127.0.0.1:{port}"))
}

/// Extract the query string from an HTTP request line.
///
/// Why: The redirect arrives as `GET /?code=...&state=... HTTP/1.1`; we only
/// need the part after `?`.
/// What: Splits the request line into method/target/version and returns the
/// substring after the first `?` in the target (empty if none).
/// Test: `parses_query_from_request_line`.
pub fn query_from_request_line(line: &str) -> String {
    let target = line.split_whitespace().nth(1).unwrap_or("");
    match target.split_once('?') {
        Some((_, q)) => q.to_string(),
        None => String::new(),
    }
}

/// Block until the browser hits the redirect endpoint, returning the code.
///
/// Why: This is the synchronisation point of the whole flow — nothing can
/// proceed until the user finishes consent and Google redirects back.
/// What: Accepts a single connection, reads the request line, parses the
/// query, validates that `state` matches (CSRF guard), writes a success or
/// error HTML page to the browser, and returns the authorization `code`.
/// Runs synchronously; callers on an async runtime should wrap it in
/// `tokio::task::spawn_blocking`.
/// Test: `roundtrips_code_over_socket` connects a client and asserts the
/// returned code.
pub fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    let (stream, _peer) = listener.accept().context("accept callback connection")?;
    handle_connection(stream, expected_state)
}

/// Handle a single accepted connection (split out for testability).
///
/// Why: Keeping the socket logic in one function lets the test feed a real
/// `TcpStream` without re-implementing the accept loop.
/// What: Reads the first request line, validates `state`, responds with HTML,
/// and returns the `code` or a descriptive error.
/// Test: `roundtrips_code_over_socket`, `rejects_state_mismatch`.
fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Result<String> {
    let mut reader = BufReader::new(stream.try_clone().context("clone callback stream")?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("read callback request line")?;

    let query = query_from_request_line(request_line.trim_end());
    let params = parse_callback_query(&query);

    if let Some(err) = params.get("error") {
        write_response(
            &mut stream,
            "Authorization failed",
            &format!("Google returned an error: {err}. You can close this window."),
        );
        return Err(anyhow!("authorization denied: {err}"));
    }

    let state = params.get("state").map(String::as_str).unwrap_or("");
    if state != expected_state {
        write_response(
            &mut stream,
            "Authorization failed",
            "State mismatch — possible CSRF. You can close this window.",
        );
        return Err(anyhow!("state mismatch: possible CSRF, aborting"));
    }

    let code = params
        .get("code")
        .filter(|c| !c.is_empty())
        .cloned()
        .ok_or_else(|| anyhow!("callback missing authorization code"))?;

    write_response(
        &mut stream,
        "Authorization complete",
        "You have successfully authorized trusty-gworkspace. You can close this window and return to the terminal.",
    );
    Ok(code)
}

/// Write a minimal self-contained HTML page and close the connection.
///
/// Why: The user's browser is left on this page after consent; a friendly
/// message beats a raw connection-reset.
/// What: Sends an HTTP/1.1 200 response with a tiny inline HTML body. Write
/// errors are ignored — the browser tab is cosmetic and the flow's real
/// result is the returned code.
/// Test: Body content is asserted indirectly by `roundtrips_code_over_socket`.
fn write_response(stream: &mut TcpStream, title: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;text-align:center\">\
         <h1>{title}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn bind_returns_usable_port() {
        let listener = bind_loopback().expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(port > 0, "OS must assign a nonzero ephemeral port");
        assert!(
            listener.local_addr().unwrap().ip().is_loopback(),
            "listener must be loopback-only"
        );
    }

    #[test]
    fn redirect_uri_reflects_bound_port() {
        let listener = bind_loopback().expect("bind");
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            redirect_uri(&listener).unwrap(),
            format!("http://127.0.0.1:{port}")
        );
    }

    #[test]
    fn parses_query_from_request_line() {
        assert_eq!(
            query_from_request_line("GET /?code=abc&state=xyz HTTP/1.1"),
            "code=abc&state=xyz"
        );
        assert_eq!(query_from_request_line("GET / HTTP/1.1"), "");
    }

    #[test]
    fn roundtrips_code_over_socket() {
        let listener = bind_loopback().expect("bind");
        let addr = listener.local_addr().unwrap();

        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).expect("connect");
            s.write_all(b"GET /?code=THECODE&state=STATE123 HTTP/1.1\r\n\r\n")
                .expect("write");
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        });

        let code = wait_for_code(&listener, "STATE123").expect("code");
        assert_eq!(code, "THECODE");

        let response = client.join().expect("join");
        assert!(response.contains("200 OK"));
        assert!(response.contains("Authorization complete"));
    }

    #[test]
    fn rejects_state_mismatch() {
        let listener = bind_loopback().expect("bind");
        let addr = listener.local_addr().unwrap();

        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).expect("connect");
            let _ = s.write_all(b"GET /?code=X&state=WRONG HTTP/1.1\r\n\r\n");
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
        });

        let result = wait_for_code(&listener, "EXPECTED");
        assert!(result.is_err(), "state mismatch must be rejected");
        client.join().expect("join");
    }
}
