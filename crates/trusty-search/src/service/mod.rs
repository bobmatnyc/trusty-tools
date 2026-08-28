pub mod boot_markers;
pub mod call_chain;
#[cfg(feature = "candle")]
pub mod candle_embedder;
pub mod client;
pub mod colocated_storage;
pub mod concurrency;
pub mod config;
pub mod constants;
pub mod context_inference;
pub mod daemon;
pub(crate) mod data_dir;
pub mod embed_pool;
pub mod embedder_supervisor;
pub mod fs_discovery;
pub mod grep;
pub mod index_budget;
pub mod indexed_files;
pub mod lazy_loader;
pub(crate) mod lazy_restore;
pub mod mcp_descriptor;
pub mod metrics;
pub mod network_fs;
pub mod orphan_reaper;
// #6371: the read-only census the console's cleanup action lists before it
// deletes anything. Sits beside the reaper because it routes the reap decision
// through `orphan_reaper::is_reapable_orphan`.
pub mod orphan_report;
pub mod persistence;
pub mod persistence_loader;
pub mod persistence_timestamps;
pub mod query_timeout;
pub mod reconcile;
pub mod reindex;
pub mod roots_registry;
pub mod server;
pub mod shutdown_budget;
pub mod shutdown_flush;
// #6285 (ADR-0032): the hardened UDS listener the daemon serves alongside its
// HTTP listener while the route families migrate.
/// The methods `socket` serves, one module per route family (#6285).
pub mod rpc;
pub mod socket;
pub mod stall_tracker;
pub mod timeout_recovery;
pub mod ui;
pub mod walker;
pub mod warm_boot;
pub mod watch_loop;
pub mod watch_rescan;
#[cfg(test)]
pub(crate) mod watch_test_support;
pub mod watcher;
pub mod watcher_manager;

pub use mcp_descriptor::SearchMcpService;

pub use config::{load_user_config, LoadedUserConfig};
pub use constants::DEFAULT_PORT;
pub use daemon::{
    bootstrap_process_env, daemon_env_path, daemon_lock_path, daemon_port_path, http_addr_path,
    is_already_running, load_daemon_env, load_daemon_env_early, load_daemon_env_early_for,
    parse_daemon_env, run_daemon, running_daemon_pid, save_daemon_env, write_http_addr_file,
    DaemonEnvPair, DaemonEnvReject, DaemonError, DaemonHandle, PERSISTED_ENV_VARS,
};
pub use indexed_files::IndexedFiles;
pub use server::SearchAppState;
pub use watch_loop::{spawn_watch_loop, WatcherTask};
pub use watcher::{FileWatcher, WatchEvent};
pub use watcher_manager::WatcherManager;
