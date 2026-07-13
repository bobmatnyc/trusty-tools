//! Injectable `config keys` operations (issue #2404) — the testable core.
//!
//! Why: the clap layer ([`super::keys`]) is a thin shell around these functions
//! so the whole `config keys` surface is exercisable non-interactively (a merge
//! gate): tests inject a [`crate::inference::credentials::MemoryKeyStore`], a
//! [`Configurator`] pointed at a mock server, and an in-memory output buffer,
//! then assert behaviour AND that no key value ever appears. Every function here
//! takes its store / configurator / output sink as parameters — none reaches for
//! process globals or a real network except where a caller wires them.
//! What: [`set`], [`list`], [`unset`], and the async [`probe`] (+ [`report_probe`]),
//! plus the [`KeyTier`] tier classifier and [`ProbeOutcome`] result type. Values
//! are only ever surfaced through [`redact_secret`]; the raw key is written to
//! the store and (for the probe) placed on the wire by the adapter, nowhere else.
//! Test: inline `tests` (tier classification, line parsing) + the full flow and
//! mock-server probe in `crates/trusty-common/tests/config_keys_cli.rs`.

use std::io::{BufRead, Write};

use crate::inference::configurator::Configurator;
use crate::inference::credentials::{KeyStore, env_local_value, redact_secret, resolve_key_with};
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, all, capabilities_for};
use crate::inference::types::{ChatMessage, ChatRequest};

/// Which resolution tier currently supplies a provider's key.
///
/// Why: `config keys list` must report WHERE a key comes from (so an operator
/// knows whether it is pinned in the shell, committed to `.env.local`, or held
/// in the secure store) — using the same precedence the resolver applies. A
/// mounting binary that pre-loads `.env.local` at startup (as trusty-search and
/// trusty-agents already do — see [`Self::EnvOrEnvLocal`]) folds the file's
/// values into the process env BEFORE `config` ever runs, so a bare "process env
/// var is set" check cannot tell "independently exported in the shell" apart
/// from "loaded from `.env.local` by the binary itself". Rather than assert a
/// precise-but-possibly-wrong tier in that case, [`detect_tier`] reports the
/// honest ambiguity.
/// What: [`Self::Env`] and [`Self::EnvLocal`] are the unambiguous single-source
/// tiers; [`Self::EnvOrEnvLocal`] covers the double-signal case; [`Self::Store`]
/// is the secure-store fallback. Rendered via [`Self::label`]; never carries a
/// value.
/// Test: `classify_tier_follows_precedence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTier {
    /// A process environment variable, with no matching `.env.local` entry
    /// found — unambiguously an independently-set env var.
    Env,
    /// A repo-root `.env.local` entry, with no value in the process env.
    EnvLocal,
    /// The key is present BOTH in the process env AND in the `.env.local` file.
    /// Ambiguous: it may be an independently-exported shell var, or it may be
    /// the `.env.local` value the mounting binary already folded into its own
    /// env at startup (trusty-search/trusty-agents both do this today). See the
    /// [`list`] docs for the long-term fix landing with #2405.
    EnvOrEnvLocal,
    /// The secure key store (File-0600 / OS keyring) — lowest precedence.
    Store,
}

impl KeyTier {
    /// Human-readable tier name for the `list` output.
    ///
    /// Why: the list is for humans; the tier needs a clear label, not an enum
    /// name.
    /// What: a short phrase per tier; [`Self::EnvOrEnvLocal`] spells out the
    /// ambiguity rather than picking one guess.
    /// Test: `classify_tier_follows_precedence` (values are asserted via callers).
    pub fn label(self) -> &'static str {
        match self {
            Self::Env => "environment variable",
            Self::EnvLocal => ".env.local",
            Self::EnvOrEnvLocal => "environment variable or .env.local (ambiguous)",
            Self::Store => "secure store",
        }
    }
}

/// Classify the resolution tier from the three source signals, in precedence.
///
/// Why: the pure decision at the heart of `list`'s tier reporting — separated so
/// it is unit-testable without touching the environment, a file, or a store.
/// What: when both `env` and `env_local` are true, the tier is the ambiguous
/// [`KeyTier::EnvOrEnvLocal`] (a mounting binary may have folded the file into
/// its env at startup — see [`KeyTier`] docs); otherwise env alone beats
/// `.env.local` alone beats store (matching
/// [`resolve_key_with`]/`resolve_key`'s actual precedence); `None` when no tier
/// supplies the key.
/// Test: `classify_tier_follows_precedence`.
pub fn classify_tier(env: bool, env_local: bool, store: bool) -> Option<KeyTier> {
    if env && env_local {
        Some(KeyTier::EnvOrEnvLocal)
    } else if env {
        Some(KeyTier::Env)
    } else if env_local {
        Some(KeyTier::EnvLocal)
    } else if store {
        Some(KeyTier::Store)
    } else {
        None
    }
}

/// The outcome of a `config keys test` live auth probe.
///
/// Why: a structured result so both the human-facing report and the process exit
/// code derive from one classification, and so tests can assert the outcome
/// directly without scraping stdout.
/// What: `Ok` (accepted), `Unauthorized` (401/403), `Unconfigured` (no key
/// resolved — the clean-degrade case), `Unsupported` (no probeable adapter, e.g.
/// Bedrock's AWS chain or a not-yet-wired provider), and `Failed` (any other
/// error — never contains the key, since [`InferenceError`] never does).
/// Test: `crates/trusty-common/tests/config_keys_cli.rs` (OK / 401 / unconfigured).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The provider accepted the credential.
    Ok,
    /// The provider rejected the credential (HTTP 401/403).
    Unauthorized,
    /// No key resolved for the provider; nothing to probe.
    Unconfigured,
    /// The provider cannot be probed by this verb (reason attached).
    Unsupported(String),
    /// The probe failed for another reason (redacted message attached).
    Failed(String),
}

impl ProbeOutcome {
    /// Human-readable, value-free status line for `report_probe`.
    ///
    /// Why: each outcome needs an actionable one-liner (and the `Unconfigured`
    /// case must point the operator at `set`).
    /// What: a short phrase; the `Unsupported`/`Failed` variants embed their
    /// (already key-free) reason.
    /// Test: asserted in `config_keys_cli.rs` probe tests.
    pub fn label(&self) -> String {
        match self {
            Self::Ok => "OK — credentials accepted".to_string(),
            Self::Unauthorized => "UNAUTHORIZED — provider rejected the key (401/403)".to_string(),
            Self::Unconfigured => {
                "UNCONFIGURED — no key resolved; set one with `config keys set <provider>`"
                    .to_string()
            }
            Self::Unsupported(reason) => format!("SKIPPED — {reason}"),
            Self::Failed(reason) => format!("ERROR — {reason}"),
        }
    }

    /// Map the outcome to a process-exit result.
    ///
    /// Why: scripts driving `config keys test` need a non-zero exit on a genuine
    /// failure (a rejected or broken key) while a clean "nothing to test" or
    /// "can't test this provider" degrades to success.
    /// What: `Ok`, `Unconfigured`, and `Unsupported` return `Ok(())`;
    /// `Unauthorized` and `Failed` return an `Err` whose message is the label.
    /// Test: covered structurally; the outcome itself is the asserted surface.
    pub fn into_result(self) -> anyhow::Result<()> {
        match self {
            Self::Ok | Self::Unconfigured | Self::Unsupported(_) => Ok(()),
            other => Err(anyhow::anyhow!("{}", other.label())),
        }
    }
}

/// Store `value` as `provider`'s key in the secure store.
///
/// Why: the `set` verb — the only sanctioned way a key enters the store. The raw
/// value is written to the store and never printed; the confirmation shows only
/// the [`redact_secret`] preview so a human can confirm *which* key landed
/// without seeing it.
/// What: validates `provider` is a known, keyed provider (rejecting unknowns and
/// the keyless Bedrock chain), refuses an empty value, writes it under the
/// canonical provider name, and prints a redacted confirmation to `out`.
/// Test: `config_keys_cli.rs::set_then_list_reports_store_tier_without_value`.
pub fn set(
    store: &dyn KeyStore,
    provider: &str,
    value: &str,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let caps = known_keyed_provider(provider)?;
    let name = caps.id.as_str();
    if value.is_empty() {
        anyhow::bail!("refusing to store an empty key for {name}");
    }
    store
        .set(name, value)
        .map_err(|e| anyhow::anyhow!("failed to store key for {name}: {e}"))?;
    writeln!(
        out,
        "Stored key for {name} in the secure store [{}].",
        redact_secret(value)
    )?;
    Ok(())
}

/// Remove `provider`'s key from the secure store (store tier only).
///
/// Why: the `unset` verb. It must touch ONLY the secure store — never the
/// process env or `.env.local` (those are not ours to mutate) — and report
/// honestly whether anything was actually removed.
/// What: validates the provider, checks prior presence in the store, removes it,
/// and prints whether a key was removed or none was set.
/// Test: `config_keys_cli.rs::unset_removes_key_and_reports_absence`.
pub fn unset(store: &dyn KeyStore, provider: &str, out: &mut dyn Write) -> anyhow::Result<()> {
    let caps = known_keyed_provider(provider)?;
    let name = caps.id.as_str();
    let was_present = store.get(name).is_some();
    store
        .unset(name)
        .map_err(|e| anyhow::anyhow!("failed to remove key for {name}: {e}"))?;
    if was_present {
        writeln!(out, "Removed key for {name} from the secure store.")?;
    } else {
        writeln!(
            out,
            "No key for {name} was set in the secure store; nothing to remove."
        )?;
    }
    Ok(())
}

/// List every known provider's key status: configured tier or "not configured".
///
/// Why: the `list` verb. It answers "which providers can I use, and where does
/// each key come from" WITHOUT ever revealing a value — names and tiers only.
///
/// NOTE for mounting binaries (#2405): if your `main()` pre-loads `.env.local`
/// into the process env before dispatching to `config` — as trusty-search
/// (`main.rs`) and trusty-agents (`runtime/startup.rs`) already do via their own
/// ad hoc `dotenvy` calls — this verb cannot tell "you exported this in your
/// shell" apart from "your binary loaded it from `.env.local` at startup" for a
/// key present in both places; it reports [`KeyTier::EnvOrEnvLocal`] rather than
/// guessing. The clean long-term fix is for every binary to route its startup
/// dotenv load through the shared `credentials::load_env_local_once` (so this
/// module owns the one loader and can observe load order); that migration lands
/// alongside #2405.
/// What: walks the capability registry ([`all`]); for keyed providers reports the
/// resolution tier via [`detect_tier`] (env-only > `.env.local`-only > store,
/// with the ambiguous both-present case reported honestly — see [`KeyTier`]) or
/// "not configured"; for the keyless Bedrock chain reports that it uses AWS
/// credentials. Writes to `out`.
/// Test: `config_keys_cli.rs::set_then_list_reports_store_tier_without_value`,
/// `list_reports_env_tier`,
/// `list_reports_ambiguous_tier_when_env_and_env_local_both_present`.
pub fn list(store: &dyn KeyStore, out: &mut dyn Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "Provider key status (names and tiers only — values are never shown):"
    )?;
    for caps in all() {
        let name = caps.id.as_str();
        match caps.credential_env {
            None => writeln!(out, "  {name:<12} AWS credential chain (no API key)")?,
            Some(env_var) => match detect_tier(env_var, name, store) {
                Some(tier) => writeln!(out, "  {name:<12} configured via {}", tier.label())?,
                None => writeln!(out, "  {name:<12} not configured")?,
            },
        }
    }
    Ok(())
}

/// Cheap live auth probe for `provider` using the resolved key.
///
/// Why: the `test` verb — the "api-testable-locally" check. It confirms a key is
/// actually accepted (or rejected) by the provider without leaking it, and
/// degrades cleanly when no key is configured.
/// What: validates the provider; the keyless Bedrock chain is
/// [`ProbeOutcome::Unsupported`]. Resolves the key (env > `.env.local` via the
/// caller's prior load > `store`); a miss is [`ProbeOutcome::Unconfigured`]. Then
/// forces this exact provider by prefixing its slug (a resolvable key means the
/// two-stage resolver will not fall back), builds the adapter from `cfg`, and
/// issues a minimal 1-token chat as the auth probe. A 401/403 is
/// [`ProbeOutcome::Unauthorized`]; a not-yet-wired provider is
/// [`ProbeOutcome::Unsupported`]; any other error is [`ProbeOutcome::Failed`],
/// its message run through [`scrub_key`] first — a provider's non-2xx response
/// BODY is attacker/provider-controlled and could echo the credential back
/// (some providers include the offending header value in a 401/400 body); the
/// scrub guarantees the resolved key never reaches `Failed`'s message even in
/// that case. Returns `Err` only on an unknown provider.
/// Test: `config_keys_cli.rs` OK / 401 / unconfigured probe cases,
/// `probe_error_body_never_leaks_the_resolved_key`.
pub async fn probe(
    store: &dyn KeyStore,
    cfg: &Configurator,
    provider: &str,
) -> anyhow::Result<ProbeOutcome> {
    let caps = capabilities_for(provider).ok_or_else(|| {
        anyhow::anyhow!("unknown provider {provider:?}; known: {}", known_names())
    })?;
    let name = caps.id.as_str();

    // Bedrock (and any future keyless provider): no API key to probe.
    if caps.credential_env.is_none() {
        return Ok(ProbeOutcome::Unsupported(format!(
            "{name} authenticates via the AWS credential chain, not an API key"
        )));
    }

    // Clean degrade when nothing resolves. Keep the resolved value so any error
    // text surfaced below can be scrubbed of it (see `scrub_key`).
    let Some(resolved_key) = resolve_key_with(name, store) else {
        return Ok(ProbeOutcome::Unconfigured);
    };

    // Prefix the slug so the resolver picks THIS provider (its key resolves, so
    // stage 1 wins and there is no OpenRouter fallback).
    let slug = format!("{name}/{}", caps.default_model);
    let adapter = match cfg.build(&slug, store) {
        Ok(adapter) => adapter,
        Err(InferenceError::NoAdapterRegistered { .. }) => {
            return Ok(ProbeOutcome::Unsupported(format!(
                "no inference adapter is wired for {name} yet"
            )));
        }
        Err(InferenceError::MissingCredential { .. }) => return Ok(ProbeOutcome::Unconfigured),
        Err(err) => {
            return Ok(ProbeOutcome::Failed(scrub_key(
                &err.to_string(),
                &resolved_key,
            )));
        }
    };

    // Minimal, cheap 1-token request as the auth probe.
    let mut req = ChatRequest::new(caps.default_model, vec![ChatMessage::user("ping")]);
    req.max_tokens = Some(1);
    req.temperature = Some(0.0);
    match adapter.chat(&req).await {
        Ok(_) => Ok(ProbeOutcome::Ok),
        Err(InferenceError::Api {
            status: 401 | 403, ..
        }) => Ok(ProbeOutcome::Unauthorized),
        Err(err) => Ok(ProbeOutcome::Failed(scrub_key(
            &err.to_string(),
            &resolved_key,
        ))),
    }
}

/// Remove any occurrence of `key` from `message`, defensively.
///
/// Why: [`InferenceError::Api`]'s `Display` embeds the provider's raw,
/// non-2xx response BODY verbatim — provider-controlled content that could
/// (accidentally or via a misbehaving/malicious endpoint) echo the credential
/// back, e.g. in a "your key `sk-…` is invalid" message. `Failed`'s message
/// reaches stdout unredacted via [`report_probe`], so this is the one place in
/// the credential-management CLI that must never let that happen. Generic
/// substring removal (rather than a provider-specific parser) keeps the guard
/// correct for every current and future adapter.
/// What: returns `message` with every occurrence of `key` replaced by
/// `[REDACTED]`; a no-op when `key` is empty (an empty needle would match
/// everywhere) or does not occur in `message`.
/// Test: `probe_error_body_never_leaks_the_resolved_key`,
/// `scrub_key_is_noop_for_empty_key`.
fn scrub_key(message: &str, key: &str) -> String {
    if key.is_empty() {
        return message.to_string();
    }
    message.replace(key, "[REDACTED]")
}

/// Write a probe outcome as a value-free status line.
///
/// Why: keeps the human report in one place so the `test` runner is a one-liner.
/// What: writes `test <provider>: <label>` to `out`.
/// Test: asserted in `config_keys_cli.rs` probe cases.
pub fn report_probe(
    provider: &str,
    outcome: &ProbeOutcome,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    writeln!(out, "test {provider}: {}", outcome.label())?;
    Ok(())
}

/// Read one key value from a `BufRead` (the scriptable stdin-pipe path).
///
/// Why: `set` with no VALUE arg on a non-TTY reads the key from a pipe; factoring
/// the line read out of the interactive machinery makes the scriptable path
/// unit-testable with an in-memory reader.
/// What: reads a single line and strips the trailing newline / carriage return;
/// the value is returned, never printed.
/// Test: `config_keys_cli.rs::read_key_line_trims_piped_value`.
pub fn read_key_line(reader: &mut dyn BufRead) -> anyhow::Result<String> {
    let mut buf = String::new();
    reader.read_line(&mut buf)?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Resolve `provider` to its capabilities, requiring it be known AND keyed.
///
/// Why: `set`/`unset` act on a key, so an unknown provider or the keyless Bedrock
/// chain is a user error with an actionable message (rather than silently
/// storing an orphan entry).
/// What: looks the provider up case-insensitively in the registry; errors on an
/// unknown name (listing the known keyed ones) or on a provider with no
/// `credential_env`.
/// Test: covered via `set`/`unset` error paths in `config_keys_cli.rs`.
fn known_keyed_provider(provider: &str) -> anyhow::Result<&'static ProviderCapabilities> {
    let caps = capabilities_for(provider).ok_or_else(|| {
        anyhow::anyhow!("unknown provider {provider:?}; known: {}", known_names())
    })?;
    if caps.credential_env.is_none() {
        anyhow::bail!(
            "{} authenticates via the AWS credential chain, not an API key",
            caps.id.as_str()
        );
    }
    Ok(caps)
}

/// Gather the three tier signals for `provider` and classify them.
///
/// Why: the production tier lookup behind `list`. It reads the process env and
/// the `.env.local` file INDEPENDENTLY (rather than after loading the file into
/// the env) so it CAN tell the two tiers apart when only one of them supplies
/// the key. It cannot, however, un-fold a value a mounting binary already
/// merged into its own env at startup — when both signals are true, the result
/// is [`KeyTier::EnvOrEnvLocal`], not a guess (see [`KeyTier`]/[`list`] docs).
/// What: checks a non-empty process env var, a non-empty `.env.local` entry (via
/// [`env_local_value`], non-mutating), and store presence, then defers to
/// [`classify_tier`].
/// Test: the pure classifier is unit-tested; the env, `.env.local`, ambiguous,
/// and store tiers are covered by `config_keys_cli.rs`.
fn detect_tier(env_var: &str, provider: &str, store: &dyn KeyStore) -> Option<KeyTier> {
    let env = std::env::var(env_var)
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();
    let env_local = env_local_value(env_var).is_some();
    let stored = store.get(provider).is_some();
    classify_tier(env, env_local, stored)
}

/// Comma-separated list of known keyed provider names (for error messages).
///
/// Why: an unknown-provider error should tell the user what IS valid.
/// What: the canonical names of registry providers that use an API key.
/// Test: surfaced via `set`/`probe` unknown-provider errors.
fn known_names() -> String {
    all()
        .iter()
        .filter(|c| c.credential_env.is_some())
        .map(|c| c.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `list`'s tier report must follow the resolver's precedence exactly
    /// for single-signal cases — env-only over `.env.local`-only over store —
    /// report the honest ambiguity when BOTH env and `.env.local` supply the
    /// key (a mounting binary may have folded the file into its env at
    /// startup), and report `None` when absent everywhere.
    /// Test: itself.
    #[test]
    fn classify_tier_follows_precedence() {
        assert_eq!(
            classify_tier(true, true, true),
            Some(KeyTier::EnvOrEnvLocal)
        );
        assert_eq!(classify_tier(true, false, true), Some(KeyTier::Env));
        assert_eq!(classify_tier(false, true, true), Some(KeyTier::EnvLocal));
        assert_eq!(classify_tier(false, false, true), Some(KeyTier::Store));
        assert_eq!(classify_tier(false, false, false), None);
    }

    /// Why: the scriptable stdin path must return the piped value with the
    /// trailing newline stripped, and nothing else.
    /// Test: itself.
    #[test]
    fn read_key_line_strips_newline() {
        let mut reader = std::io::Cursor::new(b"sk-piped-value\n".to_vec()); // pragma: allowlist secret
        assert_eq!(read_key_line(&mut reader).unwrap(), "sk-piped-value");
    }

    /// Why: a probe-error message that echoes the resolved key verbatim (e.g. a
    /// provider that includes the offending credential in its error body) must
    /// have every occurrence of the key removed, never partially redacted.
    /// Test: itself.
    #[test]
    fn scrub_key_removes_every_occurrence() {
        let key = "sk-or-verysecret1234"; // pragma: allowlist secret
        let msg = format!("inference API error 400: bad key {key}, retry without {key}");
        let scrubbed = scrub_key(&msg, key);
        assert!(!scrubbed.contains(key), "leaked: {scrubbed}");
        assert_eq!(scrubbed.matches("[REDACTED]").count(), 2);
    }

    /// Why: an empty key must never be treated as a wildcard needle — that would
    /// corrupt every message it touches.
    /// Test: itself.
    #[test]
    fn scrub_key_is_noop_for_empty_key() {
        assert_eq!(scrub_key("some message", ""), "some message");
    }
}
