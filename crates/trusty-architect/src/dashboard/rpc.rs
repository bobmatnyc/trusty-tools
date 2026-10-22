//! Request and response types of the `mpm.architect.dashboard.*` methods.
//!
//! Why: the daemon method, the MCP tool and `tm architect dashboard` must
//! share one schema, limits and exit codes (DOC-82 LDG-3.3); defining the
//! types here lets all three import them (#10063 PR 3).
//! What: [`METHODS`] (the router's `mpm.` prefix, owner ruling D34), the
//! store selection [`StoreSel`] every request carries, one request type per
//! verb (unknown keys refused, as `mpm.dbatg.*`), the replies that are not
//! already a store type, and [`DashError::code`], the error class that fixes
//! the exit code: a stale `if_version` is 1 (D34), a refused request 2, an
//! unreadable or locked store 3.
//! Test: `architect_dashboard_methods_round_trip_over_the_daemon_socket`,
//! `dashboard_rpc_classes_fix_the_exit_codes`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-03~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-03~draft)

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::DashError;
use super::store::Format;
use crate::ledger::rpc::RpcErrorCode;

/// Every `mpm.architect.dashboard.*` method, in DOC-82's order.
pub const METHODS: [&str; 12] = [
    "mpm.architect.dashboard.get",
    "mpm.architect.dashboard.list",
    "mpm.architect.dashboard.set",
    "mpm.architect.dashboard.update",
    "mpm.architect.dashboard.delete",
    "mpm.architect.dashboard.batch",
    "mpm.architect.dashboard.pruned",
    "mpm.architect.dashboard.validate",
    "mpm.architect.dashboard.fsck",
    "mpm.architect.dashboard.tally",
    "mpm.architect.dashboard.footer",
    "mpm.architect.dashboard.ids",
];

/// The loopback dashboard URL the prototype's footer names.
pub const LOCAL_URL: &str = "http://127.0.0.1:7890/";

fn dir_format() -> Format {
    Format::Dir
}

/// Which store a request reads or writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSel {
    /// The store root, absolute.
    pub root: PathBuf,
    /// `dir` (the prototype's default) or `jsonl`.
    #[serde(default = "dir_format")]
    pub format: Format,
    /// The `by` of change and prune lines (default `architect`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The prune log (default `<root>/../owner-queue-archive/pruned.jsonl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_log: Option<PathBuf>,
}

macro_rules! request {
    ($(#[$doc:meta])* $name:ident { $($(#[$f:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(missing_docs)] // fields are the verb's arguments
        pub struct $name { pub store: StoreSel, $($(#[$f])* pub $field: $ty),* }
    };
}

request!(/// `get` one document.
    GetRequest { collection: String, id: String });
request!(/// `list` a collection: `status` filter, `order_by` key, `desc`, `limit`.
    ListRequest { collection: String,
        #[serde(default)] status: Option<String>, #[serde(default)] order_by: Option<String>,
        #[serde(default)] desc: bool, #[serde(default)] limit: Option<usize> });
request!(/// `set` or `update` one document; `if_version` 0 means "must not exist".
    WriteRequest { collection: String, id: String, data: Value,
        #[serde(default)] if_version: Option<u64> });
request!(/// `delete` one document.
    DeleteRequest { collection: String, id: String, #[serde(default)] if_version: Option<u64> });
request!(/// `batch`: the prototype's ops `{op, collection, id, data, if_version}`, all or none.
    BatchRequest { ops: Vec<Value> });
request!(/// `pruned`: prune-log lines matching `id` and `collection`, oldest first.
    PrunedRequest { #[serde(default)] id: Option<String>, #[serde(default)] collection: Option<String> });
request!(/// `validate` every document against the schema directory `schemas`.
    ValidateRequest { schemas: PathBuf });
request!(/// `fsck` or `tally`: the store alone.
    StoreRequest {});
request!(/// `footer`: the reply footer; `local` defaults to [`LOCAL_URL`].
    FooterRequest { #[serde(default)] local: Option<String>, tailnet: String });
request!(/// `ids`: the next free id of `prefix` (one capital letter), the free count and retire candidates.
    IdsRequest { prefix: String });

/// `pruned` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedResponse {
    /// Raw prune-log lines, oldest first.
    pub lines: Vec<String>,
}

/// `fsck` reply; empty `problems` means consistent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsckResponse {
    /// One line per problem.
    pub problems: Vec<String>,
}

/// `ids` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdsResponse {
    /// The prefix asked for.
    pub prefix: String,
    /// The next free id; `None` when all 256 are used.
    pub next: Option<String>,
    /// Ids of the prefix still free.
    pub free: usize,
    /// The oldest `done` ids to retire so 64 stay free (LDG-2.2.4).
    pub retire: Vec<String>,
}

impl DashError {
    /// The method's error class, which fixes the CLI exit code.
    pub fn code(&self) -> RpcErrorCode {
        match self {
            Self::Conflict(_) => RpcErrorCode::Conflict,
            Self::Unavailable(_)
            | Self::Io { .. }
            | Self::Corrupt { .. }
            | Self::Unlogged { .. }
            | Self::RollbackIncomplete { .. } => RpcErrorCode::Unavailable,
            _ => RpcErrorCode::Invalid,
        }
    }
}
