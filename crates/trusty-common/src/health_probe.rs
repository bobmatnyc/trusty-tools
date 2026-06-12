//! HTTP health-probe helper for the "is a daemon already running?" check.
//!
//! Why: every trusty-* daemon's "is one already running?" check follows the
//! same shape — probe the recorded address for `/health` with a tight timeout
//! so a dead daemon does not block the start command for the discovery timeout.

/// Issue a short-timeout `GET {base_url}{health_path}` and report whether it
/// returns a 2xx response.
///
/// Why: every trusty-* daemon's "is one already running?" check follows the
/// same shape — probe the recorded address for `/health` with a tight timeout
/// so a dead daemon does not block the start command for the discovery
/// timeout. Lifting the probe into one helper keeps the request/timeout
/// configuration identical across `check_already_running` (file-based) and the
/// trusty-mpm lock-file path (where the URL is derived from a TOML file).
/// What: builds a `reqwest::Client` with a 1 s request timeout, issues the GET,
/// returns `true` only when the response is HTTP 2xx. Any client-builder error
/// or transport failure returns `false`.
/// Test: covered indirectly via `check_already_running_*` tests in
/// `daemon_addr` module and the three daemon integration paths.
pub async fn probe_health(base_url: &str, health_path: &str) -> bool {
    let probe = format!("{base_url}{health_path}");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&probe).send().await, Ok(resp) if resp.status().is_success())
}
