//! [`GchatChannel`]: one project's Google Chat channel — its routes, its
//! Chat client, and its state directory.
//!
//! Why: S2b's `gchat-mcp` binary needs one handle that sends only through
//! the route check, processes pulled batches, answers question lookups, and
//! reports per-route health for doctor rows (#9448).
//! What: [`GchatChannel::open`] loads `routes.toml` (keeping a refused load as
//! state, so every send is refused), builds the Chat client from the
//! connection's key file, takes the state directory's single-writer lock,
//! and opens the state files. Sending lives in
//! [`crate::gchat::egress`], inbound processing in [`crate::gchat::inbound`].
//! Test: `src/gchat/tests/egress.rs`, `src/gchat/tests/inbound.rs`,
//! `health_reports_each_route_and_the_load_status`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::gchat::api::client::{Endpoints, GchatClient};
use crate::gchat::error::{RouteError, SendError, StateError};
use crate::gchat::routes::{load_routes, RouteTable};
use crate::gchat::state::audit::{AuditEvent, AuditLog, AuditRecord};
use crate::gchat::state::ledger::{Ledger, Question};
use crate::gchat::state::spaces::SpaceBook;
use crate::gchat::state::{self, StateLock, AUDIT_FILE, QUESTIONS_FILE, SPACES_FILE};

/// Mutable state guarded by one lock: learned spaces and the ledger.
#[derive(Debug)]
pub(crate) struct Inner {
    pub(crate) spaces: SpaceBook,
    pub(crate) ledger: Ledger,
}

/// One project's Google Chat channel.
///
/// Why: the only public path to post a Chat message (ruling 7); the raw
/// `create_message` is crate-private.
/// What: holds the route-load outcome, the client (when the routes file
/// names a connection), the audit log, the locked [`Inner`] state, and the
/// state directory's single-writer lock, released when the channel drops.
/// Test: `unknown_recipient_is_refused_with_no_request_and_an_audit_line`,
/// `second_open_on_one_state_dir_is_refused_until_the_first_drops`.
#[derive(Debug)]
pub struct GchatChannel {
    pub(crate) project_dir: PathBuf,
    pub(crate) routes: Result<RouteTable, RouteError>,
    pub(crate) client: Option<Result<GchatClient, String>>,
    pub(crate) audit: AuditLog,
    pub(crate) inner: Mutex<Inner>,
    /// Declared last so it drops after every state handle.
    _lock: StateLock,
}

/// The routes-file load outcome, for doctor rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// No routes file: zero routes, every send refused.
    Missing,
    /// Loaded with this many routes.
    Loaded {
        /// Route count.
        routes: usize,
    },
    /// Refused at load: every send refused.
    Refused {
        /// The load error.
        reason: String,
    },
}

/// One route's health, for a doctor row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHealth {
    /// Route name.
    pub name: String,
    /// Recipient email.
    pub recipient: String,
    /// Allowed kinds, schema spelling.
    pub kinds: Vec<&'static str>,
    /// The learned DM space, or `None` until the recipient messages the app.
    pub space: Option<String>,
    /// Questions sent on this route and not yet answered.
    pub open_questions: usize,
}

/// The whole channel's health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHealth {
    /// The routes-file load outcome.
    pub load: LoadStatus,
    /// `Ok` when a client was built; `Err` names why not (no connection, or
    /// a key-file error). Never key material.
    pub client: Result<(), String>,
    /// One entry per route, in file order.
    pub routes: Vec<RouteHealth>,
}

impl GchatChannel {
    /// Open the channel for `project_dir` against Google's production hosts.
    ///
    /// Why: the `gchat-mcp` startup path.
    /// What: see [`GchatChannel::open_with`].
    /// Test: `refused_load_refuses_every_send`.
    pub fn open(project_dir: &Path) -> Result<Self, StateError> {
        Self::open_with(project_dir, Endpoints::default())
    }

    /// Open the channel with explicit API hosts.
    ///
    /// Why: tests point the client at a mock server.
    /// What: a refused routes load or a client that cannot be built is kept
    /// as state (sends are then refused), not returned, so doctor can still
    /// report it. Fails when another channel holds the state directory
    /// ([`StateError::Locked`]) or a state file cannot be read. A torn final
    /// ledger line is quarantined and audit-logged, not fatal. Makes no
    /// network call.
    /// Test: `refused_load_refuses_every_send`, `ledger_survives_restart`,
    /// `second_open_on_one_state_dir_is_refused_until_the_first_drops`,
    /// `torn_final_ledger_line_is_quarantined_and_the_channel_opens`.
    pub fn open_with(project_dir: &Path, endpoints: Endpoints) -> Result<Self, StateError> {
        let routes = load_routes(project_dir);
        if let Err(e) = &routes {
            tracing::warn!(error = %e, "gchat routes refused; every send is refused");
        }
        let client = routes
            .as_ref()
            .ok()
            .and_then(|t| t.connection.as_ref())
            .map(|c| {
                GchatClient::with_endpoints(&c.key_file, endpoints.clone())
                    .map_err(|e| e.to_string())
            });
        let dir = state::state_dir(project_dir);
        state::ensure_dir(&dir)?;
        // #9448 review: one writer per state dir, before any state is read.
        let lock = state::lock_dir(&dir)?;
        let inner = Inner {
            spaces: SpaceBook::open(&dir.join(SPACES_FILE))?,
            ledger: Ledger::open(&dir.join(QUESTIONS_FILE))?,
        };
        let quarantined = inner.ledger.quarantined_bytes();
        let channel = Self {
            project_dir: project_dir.to_path_buf(),
            routes,
            client,
            audit: AuditLog::new(&dir.join(AUDIT_FILE)),
            inner: Mutex::new(inner),
            _lock: lock,
        };
        if let Some(length) = quarantined {
            tracing::warn!(length, "gchat ledger: torn final line quarantined");
            let mut record = AuditRecord::new(AuditEvent::LedgerLineQuarantined, "torn_final_line");
            record.length = Some(length);
            channel.audit(&record);
        }
        Ok(channel)
    }

    /// The project directory this channel serves.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// The audit log path.
    pub fn audit_path(&self) -> &Path {
        self.audit.path()
    }

    /// The question with this id, as stored (open or answered).
    ///
    /// Why: the binary's "read a question's answer" call.
    /// What: a clone of the ledger entry; `answer` is `None` while open.
    /// Test: `reply_resolves_exactly_that_question`.
    pub fn question(&self, id: u64) -> Option<Question> {
        self.lock().ledger.get(id).cloned()
    }

    /// The Pub/Sub subscription resource name, when the routes file loaded
    /// and names a connection.
    pub fn subscription(&self) -> Option<String> {
        let table = self.routes.as_ref().ok()?;
        table.connection.as_ref().map(|c| c.subscription_name())
    }

    /// The routes-file load outcome.
    pub fn load_status(&self) -> LoadStatus {
        match &self.routes {
            Ok(t) if !t.file_present => LoadStatus::Missing,
            Ok(t) => LoadStatus::Loaded {
                routes: t.routes.len(),
            },
            Err(e) => LoadStatus::Refused {
                reason: e.to_string(),
            },
        }
    }

    /// Load status, client status and one entry per route.
    ///
    /// Why: `doctor` shows a row per route (#9448 acceptance).
    /// What: reads only local state; makes no network call.
    /// Test: `health_reports_each_route_and_the_load_status`.
    pub fn health(&self) -> ChannelHealth {
        let inner = self.lock();
        let routes = match &self.routes {
            Ok(t) => t
                .routes
                .iter()
                .map(|r| RouteHealth {
                    name: r.name.clone(),
                    recipient: r.recipient.clone(),
                    kinds: r.kinds.iter().map(|k| k.as_str()).collect(),
                    space: inner
                        .spaces
                        .space_for(&r.name, &r.recipient)
                        .map(str::to_string),
                    open_questions: inner
                        .ledger
                        .questions()
                        .filter(|q| q.route == r.name && q.is_open())
                        .count(),
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let client = match &self.client {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e.clone()),
            None => Err("routes file names no [gchat.connection]".to_string()),
        };
        ChannelHealth {
            load: self.load_status(),
            client,
            routes,
        }
    }

    /// The loaded route table, or the refusal every send reports.
    pub(crate) fn table(&self) -> Result<&RouteTable, SendError> {
        self.routes
            .as_ref()
            .map_err(|e| SendError::RoutesUnavailable {
                reason: e.to_string(),
            })
    }

    /// The Chat client, or why there is none.
    pub(crate) fn client(&self) -> Result<&GchatClient, String> {
        match &self.client {
            Some(Ok(c)) => Ok(c),
            Some(Err(e)) => Err(e.clone()),
            None => Err("routes file names no [gchat.connection]".to_string()),
        }
    }

    /// Append an audit line; a failed write is logged, never fatal to the
    /// refusal it records. Returns whether the line was written.
    pub(crate) fn audit(&self, record: &AuditRecord) -> bool {
        match self.audit.record(record) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(error = %e, "gchat audit write failed");
                false
            }
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves whole records on disk, so
        // the in-memory state is still usable.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
