//! Process-wide provider lock: refuse any route that leaves Bedrock (#10978).
//!
//! Why: a Bedrock-pinned setup could still reach Anthropic, OpenRouter,
//! Ollama or the `claude` CLI with no error — a bare-slug agent routed by
//! ambient credential, the `FALLBACK_MODEL` tier, the local-inference fast
//! path, a session `/provider` override, and `runner = "claude-code"` agents
//! (parity map section 2c, `docs/briefs/2026-11-10-tagent-beta-parity-map.md`).
//! A per-agent pin (#3765) or a default provider (#3766) cannot close those,
//! because each only governs agents that opt in. The lock is one switch that
//! every one of those paths consults.
//! What: [`active`] reads `TAGENT_PROVIDER_LOCK` and `[providers] lock` in
//! `~/.trusty-agents/config.toml` into a [`LockState`]. Only `bedrock` is a
//! supported value. The state FAILS CLOSED: an unsupported value, or a config
//! file that carries a `lock` key but does not parse, is an `Err` that
//! [`is_locked`] reads as locked and [`check_agent`] returns as the refusal.
//! Test: the `tests` module below; `agents::tests::provider_lock` for the
//! loader, credential and model-resolution consumers.

use std::path::Path;

/// Environment variable naming the lock (`TAGENT_PROVIDER_LOCK=bedrock`).
pub const LOCK_ENV: &str = "TAGENT_PROVIDER_LOCK";

/// Key inside the `[providers]` table of `~/.trusty-agents/config.toml`.
pub const CONFIG_KEY: &str = "lock";

/// The provider a lock confines every LLM call to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLock {
    /// AWS Bedrock: every agent slug must start `bedrock/`.
    Bedrock,
}

impl ProviderLock {
    /// The lock's configured spelling, used in every refusal message.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderLock::Bedrock => "bedrock",
        }
    }
}

/// The lock as read from its sources: unset, set, or set but unusable.
///
/// An `Err` is never "unset": every consumer treats it as locked.
pub type LockState = Result<Option<ProviderLock>, ProviderLockError>;

/// Why a provider lock refused a route.
///
/// Why: #10978 requires a failure that names the agent and the slug, and a
/// lock that cannot be read must stop work rather than degrade to "unlocked".
/// What: two variants for an unusable lock source, three for an agent the lock
/// refuses. Every variant aborts the caller; none is a warning.
/// Test: `unsupported_env_value_fails_closed`,
/// `unparseable_config_with_a_lock_key_fails_closed`,
/// `bare_slug_is_refused_naming_agent_and_slug`,
/// `claude_code_runner_is_refused_even_on_a_bedrock_slug`,
/// `empty_model_is_refused_under_the_lock`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderLockError {
    /// The lock names a provider this build cannot lock to.
    #[error(
        "provider lock {source_name} = '{value}' is not supported (supported: bedrock); \
         every LLM route is refused until it is fixed"
    )]
    Unsupported {
        source_name: &'static str,
        value: String,
    },
    /// `config.toml` mentions a lock but does not parse, so its value is unknown.
    #[error(
        "{path} sets [providers] {CONFIG_KEY} but does not parse ({detail}); \
         every LLM route is refused until it is fixed"
    )]
    UnreadableConfig { path: String, detail: String },
    /// The agent's final model slug is not on the locked provider.
    #[error(
        "agent '{agent}': model '{slug}' is refused because provider lock = {lock} \
         requires a '{lock}/' slug; no other provider is tried"
    )]
    OffProviderSlug {
        agent: String,
        slug: String,
        lock: &'static str,
    },
    /// The agent runs through the `claude` CLI, which never reaches Bedrock.
    #[error(
        "agent '{agent}': runner = \"claude-code\" (model '{slug}') is refused because \
         provider lock = {lock} and the claude CLI does not use it; no other provider is tried"
    )]
    ClaudeCodeRunner {
        agent: String,
        slug: String,
        lock: &'static str,
    },
    /// No model is configured, and the lock forbids the built-in fallback.
    #[error(
        "agent '{agent}' declares no model and provider lock = {lock} forbids the \
         built-in fallback; set a '{lock}/' model for it"
    )]
    NoModel { agent: String, lock: &'static str },
}

/// Classify one source's raw value; blank is unset.
fn classify(source_name: &'static str, raw: &str) -> LockState {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.eq_ignore_ascii_case("bedrock") {
        return Ok(Some(ProviderLock::Bedrock));
    }
    Err(ProviderLockError::Unsupported {
        source_name,
        value: value.to_string(),
    })
}

/// Read `[providers] lock` from a `config.toml` body.
///
/// Why: the lock must not fail open on a half-edited file, yet a malformed
/// file with no lock in it must not refuse every route either.
/// What: parses the body as a plain TOML table, so a wrong-typed value is
/// reported rather than hidden by a whole-schema parse failure. A body that
/// does not parse is an `Err` only when one of its lines assigns `lock`.
/// Test: `config_lock_is_read`, `unparseable_config_with_a_lock_key_fails_closed`,
/// `unparseable_config_without_a_lock_key_is_unlocked`.
fn from_config(path: &str, body: &str) -> LockState {
    let table = match body.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            let mentions_lock = body.lines().any(|line| {
                line.trim_start()
                    .strip_prefix(CONFIG_KEY)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            });
            return if mentions_lock {
                Err(ProviderLockError::UnreadableConfig {
                    path: path.to_string(),
                    detail: e.message().to_string(),
                })
            } else {
                Ok(None)
            };
        }
    };
    match table
        .get(crate::llm::provider_policy::CONFIG_TABLE)
        .and_then(|p| p.get(CONFIG_KEY))
    {
        None => Ok(None),
        Some(toml::Value::String(s)) => classify("[providers] lock", s),
        Some(other) => Err(ProviderLockError::Unsupported {
            source_name: "[providers] lock",
            value: other.to_string(),
        }),
    }
}

/// Combine the env value and the config body into one [`LockState`].
///
/// Why: the pure half of [`active`], testable without touching the process
/// environment or `$HOME`.
/// What: any unusable source is an `Err` (fail closed); otherwise either
/// source naming `bedrock` locks; both unset is `Ok(None)`.
/// Test: `unset_sources_are_unlocked`, `env_lock_is_read`, `config_lock_is_read`,
/// `unsupported_env_value_fails_closed`.
pub fn from_sources(env: Option<&str>, config: Option<(&str, &str)>) -> LockState {
    let from_env = env.map_or(Ok(None), |v| classify(LOCK_ENV, v));
    let from_file = config.map_or(Ok(None), |(path, body)| from_config(path, body));
    Ok(from_env?.or(from_file?))
}

/// [`from_sources`] over the env value and `<home>/.trusty-agents/config.toml`.
///
/// Why: takes `home` so a test can point it at a tempdir.
/// What: a missing config file contributes nothing; the env value is taken
/// as given.
/// Test: `read_from_home_finds_the_config_lock`.
pub fn read_from_home(env: Option<&str>, home: Option<&Path>) -> LockState {
    let path = home.map(|h| h.join(".trusty-agents").join("config.toml"));
    let body = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let shown = path.as_ref().map(|p| p.display().to_string());
    from_sources(env, shown.as_deref().zip(body.as_deref()))
}

/// The lock in effect for this process.
///
/// Why: the one ambient entry point every consumer calls, kept a thin wrapper
/// so the logic lives in the hermetically testable [`read_from_home`].
/// Test: [`read_from_home`]'s tests cover the body.
pub fn active() -> LockState {
    #[cfg(test)]
    if let Some(state) = TEST_OVERRIDE.with(|o| o.borrow().clone()) {
        return state;
    }
    let env = std::env::var(LOCK_ENV).ok();
    read_from_home(env.as_deref(), dirs::home_dir().as_deref())
}

#[cfg(test)]
thread_local! {
    /// Test-only: the state [`active`] returns on this thread while a
    /// [`LockOverride`] is alive. Thread-local so a locked test cannot leak
    /// the lock into a concurrent test, which `TAGENT_PROVIDER_LOCK` would.
    static TEST_OVERRIDE: std::cell::RefCell<Option<LockState>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only guard: [`active`] returns the given state on this thread until
/// the guard drops.
#[cfg(test)]
pub(crate) struct LockOverride(());

#[cfg(test)]
impl Drop for LockOverride {
    fn drop(&mut self) {
        TEST_OVERRIDE.with(|o| o.borrow_mut().take());
    }
}

/// Test-only: make [`active`] return `state` on this thread.
#[cfg(test)]
pub(crate) fn override_on_this_thread(state: LockState) -> LockOverride {
    TEST_OVERRIDE.with(|o| *o.borrow_mut() = Some(state));
    LockOverride(())
}

/// Whether `state` confines routing. An unreadable lock counts as locked.
pub fn is_locked(state: &LockState) -> bool {
    !matches!(state, Ok(None))
}

/// Refuse an agent whose final route leaves the locked provider.
///
/// Why: the loader, a model override, and the `.md` / trusty-mpm agent
/// projections all end in a model slug; checking the slug they end in is
/// what makes the lock hold for every one of them.
/// What: unlocked → `Ok`. An unusable lock → that error. Locked to Bedrock →
/// refuses `runner = "claude-code"`, an empty slug, and any slug that does not
/// start `bedrock/` with a non-empty model id after it.
/// Test: `bare_slug_is_refused_naming_agent_and_slug`,
/// `claude_code_runner_is_refused_even_on_a_bedrock_slug`,
/// `bedrock_slug_passes_the_lock`, `unlocked_state_admits_any_agent`.
pub fn check_agent(
    state: &LockState,
    agent: &str,
    slug: &str,
    runner_is_claude_code: bool,
) -> Result<(), ProviderLockError> {
    let lock = match state {
        Ok(None) => return Ok(()),
        Ok(Some(lock)) => lock.as_str(),
        Err(e) => return Err(e.clone()),
    };
    if runner_is_claude_code {
        return Err(ProviderLockError::ClaudeCodeRunner {
            agent: agent.to_string(),
            slug: slug.to_string(),
            lock,
        });
    }
    if slug.trim().is_empty() {
        return Err(ProviderLockError::NoModel {
            agent: agent.to_string(),
            lock,
        });
    }
    let on_bedrock = slug
        .strip_prefix("bedrock/")
        .is_some_and(|id| !id.trim().is_empty());
    if !on_bedrock {
        return Err(ProviderLockError::OffProviderSlug {
            agent: agent.to_string(),
            slug: slug.to_string(),
            lock,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCKED: LockState = Ok(Some(ProviderLock::Bedrock));

    /// Why: no lock must mean today's behaviour.
    /// Test: itself.
    #[test]
    fn unset_sources_are_unlocked() {
        assert_eq!(from_sources(None, None), Ok(None));
        assert_eq!(from_sources(Some("  "), Some(("c", "[mcp]\n"))), Ok(None));
        assert!(!is_locked(&from_sources(None, None)));
    }

    /// Why: the env spelling the issue names.
    /// Test: itself.
    #[test]
    fn env_lock_is_read() {
        assert_eq!(from_sources(Some("bedrock"), None), LOCKED);
        assert_eq!(from_sources(Some(" Bedrock "), None), LOCKED);
    }

    /// Why: the config spelling the issue names.
    /// Test: itself.
    #[test]
    fn config_lock_is_read() {
        let body = "[providers]\nlock = \"bedrock\"\n";
        assert_eq!(from_sources(None, Some(("c", body))), LOCKED);
    }

    /// Why: a typo such as `bedrok` must not read as "no lock".
    /// Test: itself.
    #[test]
    fn unsupported_env_value_fails_closed() {
        let state = from_sources(Some("bedrok"), None);
        assert!(is_locked(&state));
        let err = check_agent(&state, "pm", "bedrock/x", false).expect_err("closed");
        assert!(err.to_string().contains("bedrok"), "{err}");
        let wrong_type = from_sources(None, Some(("c", "[providers]\nlock = 7\n")));
        assert!(matches!(
            wrong_type,
            Err(ProviderLockError::Unsupported { .. })
        ));
    }

    /// Why: a half-edited file holding the lock must not unlock routing.
    /// Test: itself.
    #[test]
    fn unparseable_config_with_a_lock_key_fails_closed() {
        let state = from_sources(None, Some(("cfg.toml", "[providers\nlock = \"bedrock\"\n")));
        assert!(matches!(
            state,
            Err(ProviderLockError::UnreadableConfig { .. })
        ));
        assert!(is_locked(&state));
    }

    /// Why: a malformed file with no lock in it keeps today's behaviour.
    /// Test: itself.
    #[test]
    fn unparseable_config_without_a_lock_key_is_unlocked() {
        let state = from_sources(None, Some(("c", "[mcp\nlocked_out = 1\n")));
        assert_eq!(state, Ok(None));
    }

    /// Why: proves the documented file path is the one read.
    /// Test: itself.
    #[test]
    fn read_from_home_finds_the_config_lock() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_from_home(None, Some(home.path())), Ok(None));
        let dir = home.path().join(".trusty-agents");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("config.toml"), "[providers]\nlock = \"bedrock\"\n")
            .expect("write");
        assert_eq!(read_from_home(None, Some(home.path())), LOCKED);
    }

    /// Why: the issue's core case — a bare slug under the lock is an error
    /// naming both the agent and the slug.
    /// Test: itself.
    #[test]
    fn bare_slug_is_refused_naming_agent_and_slug() {
        for slug in [
            "claude-sonnet-4-6",
            "anthropic/claude-sonnet-4-6",
            "ollama/qwen3:30b",
            "bedrock/",
        ] {
            let err = check_agent(&LOCKED, "research-agent", slug, false).expect_err(slug);
            let msg = err.to_string();
            assert!(
                msg.contains("research-agent") && msg.contains(slug),
                "{msg}"
            );
        }
    }

    /// Why: the claude CLI never reaches Bedrock, whatever the slug says.
    /// Test: itself.
    #[test]
    fn claude_code_runner_is_refused_even_on_a_bedrock_slug() {
        let err = check_agent(&LOCKED, "claude-code-engineer", "bedrock/x", true)
            .expect_err("claude-code under the lock");
        assert!(matches!(err, ProviderLockError::ClaudeCodeRunner { .. }));
        assert!(err.to_string().contains("claude-code-engineer"), "{err}");
    }

    /// Why: an empty slug would otherwise reach the fallback model.
    /// Test: itself.
    #[test]
    fn empty_model_is_refused_under_the_lock() {
        let err = check_agent(&LOCKED, "pm", "", false).expect_err("empty");
        assert!(matches!(err, ProviderLockError::NoModel { .. }));
    }

    /// Why: the lock must admit the routes it exists to keep.
    /// Test: itself.
    #[test]
    fn bedrock_slug_passes_the_lock() {
        check_agent(&LOCKED, "pm", "bedrock/us.anthropic.claude-opus-4-1", false)
            .expect("bedrock slug passes");
    }

    /// Why: no lock → no refusal, including for claude-code agents.
    /// Test: itself.
    #[test]
    fn unlocked_state_admits_any_agent() {
        check_agent(&Ok(None), "a", "claude-sonnet-4-6", true).expect("unlocked");
    }
}
