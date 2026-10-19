//! LSP capability configuration: the default-off flag, the server command and
//! the timeouts (#9078).
//!
//! Why: DOC-72 §2 puts the capability behind a flag that defaults off (D10), and
//! §12 ships `rust-analyzer` only (D2) with a 2-minute idle window, because the
//! 1.9.0 slice enforces no RSS cap.
//! What: [`LspConfig::from_env`] reads `ANALYZE_LSP`, `ANALYZE_LSP_IDLE_SECS` and
//! `ANALYZE_LSP_RUST_ANALYZER` once at state construction. An unparseable
//! value keeps the safe default and logs a warning.
//! Test: `config_is_off_by_default_and_parses_its_env`.

use std::time::Duration;

/// `1`/`true`/`on`/`yes` enables the capability; anything else leaves it off.
pub const ENV_LSP: &str = "ANALYZE_LSP";
/// Idle window in whole seconds before an unused server is killed.
pub const ENV_LSP_IDLE_SECS: &str = "ANALYZE_LSP_IDLE_SECS";
/// Program to run as the Rust language server (default `rust-analyzer`).
pub const ENV_LSP_RUST_ANALYZER: &str = "ANALYZE_LSP_RUST_ANALYZER";

/// The languages with a configured server. 1.9.0 ships Rust only (DOC-72 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LspLanguage {
    /// `rust-analyzer`.
    Rust,
}

impl LspLanguage {
    /// The LSP `languageId` for documents of this language.
    pub fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
        }
    }
}

/// A server command line: program plus arguments, spawned with stdio piped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCommand {
    /// A bare name resolved on `PATH`, or a path.
    pub program: String,
    /// Arguments passed verbatim.
    pub args: Vec<String>,
}

/// Configuration for [`super::LspManager`].
///
/// Test: `config_is_off_by_default_and_parses_its_env`.
#[derive(Debug, Clone)]
pub struct LspConfig {
    /// The default-off flag; with it off nothing is ever spawned.
    pub enabled: bool,
    /// The Rust server command.
    pub rust_analyzer: LspCommand,
    /// An unused server is killed after this long (default 2 minutes).
    pub idle_timeout: Duration,
    /// Deadline for one request (default 30 s).
    pub request_timeout: Duration,
    /// Deadline for the `initialize` handshake (default 60 s).
    pub initialize_timeout: Duration,
    /// How long `shutdown`/`exit` may take before the group is killed.
    pub shutdown_grace: Duration,
    /// How long a crashed or hung server's error is replayed before a respawn.
    pub failure_cooloff: Duration,
    /// Consecutive request timeouts after which a server counts as hung.
    pub max_consecutive_timeouts: u32,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rust_analyzer: LspCommand {
                program: "rust-analyzer".into(),
                args: Vec::new(),
            },
            idle_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(30),
            initialize_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(2),
            failure_cooloff: Duration::from_secs(30),
            max_consecutive_timeouts: 3,
        }
    }
}

impl LspConfig {
    /// Read the configuration from the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read the configuration through `get`, so tests need no process env.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut cfg = Self::default();
        if let Some(raw) = get(ENV_LSP) {
            cfg.enabled = match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "on" | "yes" => true,
                "" | "0" | "false" | "off" | "no" => false,
                other => {
                    tracing::warn!(
                        value = other,
                        "{ENV_LSP}: unrecognised value; LSP stays off"
                    );
                    false
                }
            };
        }
        if let Some(raw) = get(ENV_LSP_IDLE_SECS) {
            match raw.trim().parse::<u64>() {
                Ok(secs) if secs > 0 => cfg.idle_timeout = Duration::from_secs(secs),
                _ => {
                    tracing::warn!(value = %raw, "{ENV_LSP_IDLE_SECS}: not a positive integer; using the default")
                }
            }
        }
        if let Some(program) = get(ENV_LSP_RUST_ANALYZER).filter(|p| !p.trim().is_empty()) {
            cfg.rust_analyzer.program = program;
        }
        cfg
    }

    /// The command that serves `language`.
    pub fn command_for(&self, language: LspLanguage) -> &LspCommand {
        match language {
            LspLanguage::Rust => &self.rust_analyzer,
        }
    }
}
