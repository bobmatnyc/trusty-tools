//! The default `~/.trusty-agents/config.toml` asset.
//!
//! Why: Stored as a single verbatim asset so the on-disk shape is identical to
//! what callers see when they load the file, and so `default_config()` can
//! parse it to materialize the documented in-memory defaults. The text lives
//! under `assets/config/` and is embedded at compile time via `include_str!`
//! (per the repo convention, see `trusty-mpm/src/core/bundle.rs`), keeping this
//! source file lean while the emitted bytes stay identical to the old literal.
//! What: `DEFAULT_CONFIG_TOML` — the full commented default config.
//! Test: `default_config_is_valid_toml` and friends in `config::tests`.

/// Default config TOML written when `~/.trusty-agents/config.toml` is absent.
///
/// Why: Embedded verbatim so the on-disk shape is identical to what callers
/// see when they load the file; comments document intent for human editors.
/// What: `include_str!` of `assets/config/default-config.toml`.
/// Test: `default_config_is_valid_toml` and friends in `config::tests`.
pub(super) const DEFAULT_CONFIG_TOML: &str =
    include_str!("../../../assets/config/default-config.toml");
