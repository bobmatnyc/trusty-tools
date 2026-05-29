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
pub mod embed_pool;
pub mod embedder_supervisor;
pub mod fs_discovery;
pub mod grep;
pub mod indexed_files;
pub mod mcp_descriptor;
pub mod metrics;
pub mod persistence;
pub mod persistence_loader;
pub mod reindex;
pub mod roots_registry;
pub mod server;
pub mod ui;
pub mod walker;
pub mod watch_loop;
pub mod watcher;

pub use mcp_descriptor::SearchMcpService;

pub use config::{load_user_config, LoadedUserConfig};
pub use constants::DEFAULT_PORT;
pub use daemon::{
    daemon_env_path, daemon_lock_path, daemon_port_path, http_addr_path, is_already_running,
    load_daemon_env, run_daemon, running_daemon_pid, save_daemon_env, DaemonError, DaemonHandle,
    PERSISTED_ENV_VARS,
};
pub use indexed_files::IndexedFiles;
pub use server::SearchAppState;
pub use watch_loop::{spawn_watch_loop, WatcherTask};
pub use watcher::{FileWatcher, WatchEvent};
