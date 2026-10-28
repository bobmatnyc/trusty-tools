//! A server for consumer tests that judges every caller a person (#10386).
//!
//! Why: `secrets.set` and `secrets.delete` refuse a caller with a Claude Code
//! ancestor (#10386 Q2), judged on the host's process table. A consumer's
//! test suite that stores a key through a real server is then green in CI and
//! red when an agent session runs it. Compiled only under the `test-support`
//! feature and this crate's own tests, as `MemoryBackend` is, so no
//! production build can serve with it.
//! What: [`serve_without_agent_callers`] is [`super::serve`] whose process
//! table reads parents, start times and executables from the OS but answers
//! "not an agent" for every pid. Grants, peer-pid and descendant checks still
//! run on the real tree.
//! Test: `serve_without_agent_callers_stores_a_key_for_any_caller`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use super::ancestry::{OsProcessTable, ProcessError, ProcessTable, StartTime};
use super::grant::{DEFAULT_MAX_TTL, GrantRegistry, SystemClock};
use super::router::{BackendFactory, ServeError, ServeExit, StartEnv, State, serve_state};
use super::settings::ServerSettings;
use crate::store::platform;

/// The host's process table, with no process an agent.
#[derive(Debug, Clone, Copy, Default)]
struct NoAgentTable;

impl ProcessTable for NoAgentTable {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        OsProcessTable.parent(pid)
    }

    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        OsProcessTable.start_time(pid)
    }

    fn is_agent(&self, _pid: u32) -> Result<bool, ProcessError> {
        Ok(false)
    }

    fn executable(&self, pid: u32) -> Result<PathBuf, ProcessError> {
        OsProcessTable.executable(pid)
    }
}

/// [`super::serve`], judging every caller as having no Claude Code ancestor.
///
/// Why: see the module docs. Test support only.
/// What: the production state, with the grant registry's process table
/// replaced by one whose `is_agent` is always `false`.
/// Test: `serve_without_agent_callers_stores_a_key_for_any_caller`.
pub async fn serve_without_agent_callers(
    settings: ServerSettings,
    backends: BackendFactory,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ServeExit, ServeError> {
    let account_home = platform::account_home_dir().ok();
    let mut state = State::with_start(
        settings,
        backends,
        StartEnv::default(),
        account_home.as_deref(),
    );
    state.grants = Arc::new(GrantRegistry::new(
        Arc::new(NoAgentTable),
        Arc::new(SystemClock),
        DEFAULT_MAX_TTL,
    ));
    serve_state(state, shutdown).await
}
