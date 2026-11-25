//! `tm presence wait`: block until this session holds a valid grant (#8878
//! PR2b-1b).
//!
//! Why: plan-pr2-devices.md §3 step 7 — after `tm presence request`, the
//! Architect waits for a device approval without retrying the guarded call.
//! What: [`wait`] polls `presence::check` (the hook's own test, so a forged,
//! idle or other-pid grant never counts) until it passes (exit 0), the named
//! request is gone or expired with no grant (exit 1), or the timeout passes
//! (exit 75, `EXIT_PENDING`: re-issue the command).
//! Test: `wait_returns_zero_once_granted_8878`,
//! `wait_never_exits_zero_on_a_present_but_invalid_grant_8878`.

use std::path::Path;
use std::time::Duration;

use trusty_mpm::core::presence;
use trusty_mpm::core::presence_key::GrantKey;
use trusty_mpm::core::presence_request;
use trusty_mpm::core::twin_identity::ClaudeProcess;

use crate::commands::wait::EXIT_PENDING;

/// How often the grant is re-read.
pub(crate) const POLL: Duration = Duration::from_secs(1);

/// What [`wait`] reads from its surroundings.
pub(crate) struct WaitEnv<'a> {
    /// `~/.trusty-mpm`.
    pub(crate) root: &'a Path,
    /// The `claude` whose grant counts.
    pub(crate) claude: ClaudeProcess,
    /// The presence key, read per poll.
    pub(crate) key: &'a dyn Fn() -> Option<GrantKey>,
    /// Now, Unix seconds.
    pub(crate) now: &'a dyn Fn() -> u64,
    /// Sleep between polls.
    pub(crate) sleep: &'a dyn Fn(Duration),
}

/// Parse `300`, `300s` or `5m`.
pub(crate) fn parse_timeout(text: &str) -> anyhow::Result<Duration> {
    let (digits, scale) = match text.strip_suffix('m') {
        Some(d) => (d, 60),
        None => (text.strip_suffix('s').unwrap_or(text), 1),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| anyhow::anyhow!("bad --timeout `{text}`: use 300, 300s or 5m"))?;
    Ok(Duration::from_secs(n.saturating_mul(scale)))
}

/// Wait up to `timeout` for a grant; returns the exit code.
///
/// Why/What: see the module doc. Nothing here writes.
/// Test: `wait_returns_zero_once_granted_8878`,
/// `wait_never_exits_zero_on_a_present_but_invalid_grant_8878`.
pub(crate) fn wait(env: &WaitEnv<'_>, request: Option<&str>, timeout: Duration) -> i32 {
    let deadline = (env.now)().saturating_add(timeout.as_secs());
    loop {
        let now = (env.now)();
        if presence::check(env.root, &env.claude, env.key, now).is_ok() {
            println!("Presence granted.");
            return 0;
        }
        if let Some(id) = request {
            let gone = match (env.key)() {
                Some(key) => presence_request::load(env.root, &key, id, now).is_err(),
                None => false,
            };
            if gone {
                // A verify claims the request just before it mints: look once more.
                (env.sleep)(POLL);
                if presence::check(env.root, &env.claude, env.key, (env.now)()).is_ok() {
                    println!("Presence granted.");
                    return 0;
                }
                println!("Presence request {id} expired or is gone, and no grant was made.");
                return 1;
            }
        }
        if now >= deadline {
            println!(
                "Presence still pending; re-issue `tm presence wait{}`.",
                request
                    .map(|id| format!(" --request {id}"))
                    .unwrap_or_default()
            );
            return EXIT_PENDING;
        }
        (env.sleep)(POLL);
    }
}
