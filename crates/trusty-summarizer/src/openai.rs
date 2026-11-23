//! An inference backend for any OpenAI-compatible chat-completions endpoint.
//!
//! Why: plan section 2. One client covers Ollama (`/v1`), OpenRouter and the
//! Atlas route; each endpoint is a `[backends.<name>]` table with
//! `type = "openai"`, so a new endpoint is configuration, not code.
//!
//! What: [`OpenAiBackend::complete`] posts one `chat/completions` request
//! (system = instruction, user = excerpt), reads the first choice's message
//! and the `usage` block: `prompt_tokens`, `completion_tokens`, the
//! reasoning and cached-prompt detail counts, and `cost` when the provider
//! reports one (else the cost is computed from configured prices, else it is
//! unknown). An empty answer is a failure that names its cause. The API key
//! is read, per call, from the environment variable named by `api_key_env`,
//! or from the keys a warm-server client passed with the request (see
//! [`crate::backend::Credentials`]); its value never enters the config, a
//! log line, an error message, a record or the cache key.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::backend::{
    Cost, InferenceBackend, InferenceRequest, InferenceResponse, KeySource, Usage,
};
use crate::error::{Error, Result};

/// The `type` value that selects this backend.
pub const OPENAI_TYPE: &str = "openai";

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const SNIPPET_CHARS: usize = 300;
const REDACTED: &str = "[redacted]";

/// Looks up an environment variable; replaceable in tests.
pub(crate) type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// USD per million tokens, input and output.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Prices {
    input: f64,
    output: f64,
}

/// One configured OpenAI-compatible endpoint.
///
/// Why: Ollama now and OpenRouter later differ only in `base_url`, `model`
/// and `api_key_env`.
/// What: built from its options table by [`OpenAiBackend::from_options`];
/// one blocking request per [`InferenceBackend::complete`] call, bounded by
/// `timeout_ms`. Every failure is an [`Error::Backend`] that names the cause:
/// unreachable endpoint, timeout, HTTP status, malformed response or a
/// missing key variable. Thread-safe; the HTTP agent pools connections.
/// Test: `posts_chat_completion_and_reads_usage`, `failures_name_their_cause`,
/// `key_value_never_appears_in_errors`
pub struct OpenAiBackend {
    name: String,
    endpoint: String,
    model: String,
    api_key_env: Option<String>,
    temperature: Option<f64>,
    extra: Map<String, Value>,
    max_output_tokens: Option<u32>,
    prices: Option<Prices>,
    max_response_bytes: u64,
    timeout: Duration,
    agent: ureq::Agent,
    env: EnvLookup,
}

impl std::fmt::Debug for OpenAiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiBackend")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key_env", &self.api_key_env)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl OpenAiBackend {
    /// Builds the backend configured as `[backends.<name>]`.
    ///
    /// Options: `type = "openai"`, `base_url` (required, e.g.
    /// `http://localhost:11434/v1`), `model` (required), `api_key_env`
    /// (optional: the NAME of the variable holding the key), `timeout_ms`
    /// (default 120000), `temperature` (optional), `max_response_bytes`
    /// (default 4 MiB), `request` (optional table of extra JSON fields
    /// merged into the request body, e.g. `reasoning_effort = "none"`),
    /// `max_output_tokens` (optional; replaces the caller's output budget)
    /// and `price_in_per_mtok` with `price_out_per_mtok` (optional, both or
    /// neither: USD per million prompt and completion tokens, used when the
    /// provider reports no `usage.cost`).
    /// Errors: [`Error::Config`] for a missing or invalid option.
    pub fn from_options(name: &str, options: &toml::Table) -> Result<Self> {
        Self::with_env_lookup(name, options, Arc::new(|k| std::env::var(k).ok()))
    }

    /// Builds the backend with an injected variable lookup, so tests never
    /// touch the process environment. #9123: not named `with_env`, which the
    /// credential-test scan reads as an environment mutation.
    pub(crate) fn with_env_lookup(
        name: &str,
        options: &toml::Table,
        env: EnvLookup,
    ) -> Result<Self> {
        let bad = |m: String| Error::Config(format!("backends.{name}: {m}"));
        let mut base_url = None;
        let mut model = None;
        let mut api_key_env = None;
        let mut temperature = None;
        let mut extra = Map::new();
        let mut timeout_ms = DEFAULT_TIMEOUT_MS;
        let mut max_response_bytes = DEFAULT_MAX_RESPONSE_BYTES;
        let mut max_output_tokens = None;
        let (mut price_in, mut price_out) = (None, None);
        let price = |key: &str, v: &toml::Value| -> Result<f64> {
            let x = match v {
                toml::Value::Float(f) => *f,
                toml::Value::Integer(i) => *i as f64,
                _ => return Err(bad(format!("{key} must be a number"))),
            };
            if !x.is_finite() || x < 0.0 {
                return Err(bad(format!("{key} must be a finite number at least 0")));
            }
            Ok(x)
        };
        for (key, value) in options {
            match (key.as_str(), value) {
                ("type", toml::Value::String(_)) => {}
                ("base_url", toml::Value::String(s)) if !s.trim().is_empty() => {
                    base_url = Some(s.trim().trim_end_matches('/').to_string());
                }
                ("model", toml::Value::String(s)) if !s.trim().is_empty() => {
                    model = Some(s.clone());
                }
                ("api_key_env", toml::Value::String(s)) => {
                    // Never echo the value: a pasted key would land in a log.
                    if !is_env_name(s) {
                        return Err(bad(
                            "api_key_env must be the NAME of an environment variable \
                             (A-Z, 0-9, _), never the key itself"
                                .into(),
                        ));
                    }
                    api_key_env = Some(s.clone());
                }
                ("timeout_ms", toml::Value::Integer(n)) if *n >= 1 => timeout_ms = *n as u64,
                ("max_response_bytes", toml::Value::Integer(n)) if *n >= 1 => {
                    max_response_bytes = *n as u64;
                }
                ("max_output_tokens", toml::Value::Integer(n)) if *n >= 1 => {
                    max_output_tokens = Some(u32::try_from(*n).unwrap_or(u32::MAX));
                }
                ("price_in_per_mtok", v) => price_in = Some(price(key, v)?),
                ("price_out_per_mtok", v) => price_out = Some(price(key, v)?),
                ("temperature", toml::Value::Float(t)) => temperature = Some(*t),
                ("temperature", toml::Value::Integer(t)) => temperature = Some(*t as f64),
                ("request", toml::Value::Table(t)) => {
                    for (k, v) in t {
                        if matches!(k.as_str(), "model" | "messages" | "stream") {
                            return Err(bad(format!("request.{k} cannot be overridden")));
                        }
                        let json = serde_json::to_value(v)
                            .map_err(|e| bad(format!("request.{k}: {e}")))?;
                        extra.insert(k.clone(), json);
                    }
                }
                ("api_key" | "key" | "token" | "authorization" | "api_token", _) => {
                    return Err(bad(format!(
                        "`{key}` is not accepted: put the key in an environment variable \
                         and name it with api_key_env"
                    )));
                }
                _ => return Err(bad(format!("unsupported option `{key}`"))),
            }
        }
        let base_url = base_url.ok_or_else(|| bad("base_url is required".into()))?;
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(bad("base_url must start with http:// or https://".into()));
        }
        let model = model.ok_or_else(|| bad("model is required".into()))?;
        let prices = match (price_in, price_out) {
            (Some(input), Some(output)) => Some(Prices { input, output }),
            (None, None) => None,
            _ => {
                return Err(bad(
                    "set both price_in_per_mtok and price_out_per_mtok, or neither".into(),
                ));
            }
        };
        let timeout = Duration::from_millis(timeout_ms);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .build(),
            )
            .build()
            .into();
        Ok(OpenAiBackend {
            name: name.to_string(),
            endpoint: format!("{base_url}/chat/completions"),
            model,
            api_key_env,
            temperature,
            extra,
            max_output_tokens,
            prices,
            max_response_bytes,
            timeout,
            agent,
            env,
        })
    }

    fn fail(&self, message: String) -> Error {
        Error::Backend {
            backend: self.name.clone(),
            message,
            usage: None,
        }
    }

    /// A failure that carries the usage the response reported.
    fn fail_billed(&self, message: String, usage: Option<Usage>) -> Error {
        Error::Backend {
            backend: self.name.clone(),
            message,
            usage,
        }
    }

    /// Applies configured prices when the provider sent no cost.
    fn priced(&self, mut usage: Usage) -> Usage {
        if usage.cost == Cost::Unknown
            && let Some(p) = self.prices
        {
            usage.cost = Cost::ConfiguredPrices(
                (usage.tokens_in as f64 * p.input + usage.tokens_out as f64 * p.output) / 1e6,
            );
        }
        usage
    }

    /// The priced usage of a response body that has a `usage` object.
    fn billed(&self, body: &str) -> Option<Usage> {
        let v: Value = serde_json::from_str(body).ok()?;
        let u = v.get("usage").filter(|u| u.is_object())?;
        Some(self.priced(usage_of(Some(u))))
    }

    fn body(&self, request: &InferenceRequest) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.model.clone()));
        body.insert(
            "messages".into(),
            serde_json::json!([
                {"role": "system", "content": request.instruction},
                {"role": "user", "content": request.input},
            ]),
        );
        let budget = self.max_output_tokens.unwrap_or(request.max_output_tokens);
        body.insert("max_tokens".into(), budget.into());
        body.insert("stream".into(), Value::Bool(false));
        if let Some(t) = self.temperature {
            body.insert("temperature".into(), t.into());
        }
        for (k, v) in &self.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }
}

impl InferenceBackend for OpenAiBackend {
    fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        let key = match &self.api_key_env {
            None => None,
            Some(var) => {
                let found = match request.credentials.source(var) {
                    KeySource::ProcessEnv => (self.env)(var),
                    KeySource::Given(k) => Some(k.to_string()),
                    KeySource::NotGiven => None,
                };
                match found {
                    Some(k) if !k.trim().is_empty() => Some(k),
                    _ => {
                        return Err(self.fail(format!(
                            "missing API key: environment variable {var} is not set"
                        )));
                    }
                }
            }
        };
        let redact = |s: String| match &key {
            Some(k) if !k.is_empty() => s.replace(k.as_str(), REDACTED),
            _ => s,
        };
        let body = serde_json::to_vec(&self.body(request))
            .map_err(|e| self.fail(format!("cannot encode request: {e}")))?;

        let started = Instant::now();
        let mut call = self
            .agent
            .post(&self.endpoint)
            .header("Content-Type", "application/json");
        if let Some(k) = &key {
            call = call.header("Authorization", format!("Bearer {k}"));
        }
        let response = call.send(&body[..]).map_err(|e| {
            let cause = match &e {
                ureq::Error::Timeout(_) => format!(
                    "request to {} timed out after {} ms",
                    self.endpoint,
                    self.timeout.as_millis()
                ),
                _ => format!("cannot reach {}: {e}", self.endpoint),
            };
            self.fail(redact(cause))
        })?;
        let status = response.status().as_u16();
        let text = response
            .into_body()
            .with_config()
            .limit(self.max_response_bytes)
            .read_to_string()
            .map_err(|e| {
                self.fail(redact(format!(
                    "cannot read response from {}: {e}",
                    self.endpoint
                )))
            })?;
        let latency = started.elapsed();
        if !(200..300).contains(&status) {
            let usage = self.billed(&text);
            return Err(self.fail_billed(
                redact(format!(
                    "HTTP {status} from {}: {}",
                    self.endpoint,
                    snippet(&text)
                )),
                usage,
            ));
        }
        let (text, usage) = match parse_completion(&text) {
            Ok((answer, usage)) => (answer, self.priced(usage)),
            Err(m) => {
                // A failed answer (e.g. budget spent on reasoning)
                // is still billed; the error carries the reported usage.
                let usage = self.billed(&text);
                return Err(self.fail_billed(
                    redact(format!("malformed response from {}: {m}", self.endpoint)),
                    usage,
                ));
            }
        };
        Ok(InferenceResponse {
            text,
            usage,
            latency,
        })
    }
}

/// Reads the first choice's text and the usage block.
///
/// The cost is the provider's `usage.cost` when present, else
/// [`Cost::Unknown`]; the caller applies configured prices.
/// Errors (as a message): not JSON, a provider error object, no
/// `choices[0].message`, or an empty or whitespace-only answer (null
/// content included). An empty answer names its cause; when
/// `finish_reason` is `length` and the response shows reasoning, the cause
/// is "output budget spent on reasoning".
/// Test: `empty_and_reasoning_exhausted_answers_fail_with_cause`
pub(crate) fn parse_completion(body: &str) -> std::result::Result<(String, Usage), String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("not JSON ({e}): {}", snippet(body)))?;
    if let Some(err) = v.get("error") {
        return Err(format!("provider error: {}", snippet(&err.to_string())));
    }
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| "no choices[0]".to_string())?;
    let message = choice
        .get("message")
        .ok_or_else(|| "no choices[0].message".to_string())?;
    let content = match message.get("content") {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return Err("choices[0].message.content is not a string".into()),
    };
    let usage = usage_of(v.get("usage"));
    let text = strip_think(content).trim().to_string();
    if text.is_empty() {
        let reason = choice.get("finish_reason").and_then(Value::as_str);
        let reasoning_text = ["reasoning_content", "reasoning"].iter().any(|k| {
            message
                .get(*k)
                .and_then(Value::as_str)
                .is_some_and(|r| !r.trim().is_empty())
        }) || content.trim_start().starts_with("<think>");
        let reasoned = usage.reasoning_tokens.is_some_and(|n| n > 0) || reasoning_text;
        let counts = format!(
            "{} completion tokens{}",
            usage.tokens_out,
            usage
                .reasoning_tokens
                .map(|n| format!(", {n} reasoning tokens"))
                .unwrap_or_default()
        );
        return Err(match reason {
            Some("length") if reasoned => format!(
                "empty answer: output budget spent on reasoning (finish_reason `length`, \
                 {counts}); raise max_output_tokens or turn reasoning down in the \
                 backend's [request] table"
            ),
            Some(r) => format!("empty answer (finish_reason `{r}`, {counts})"),
            None => format!("empty answer (no finish_reason, {counts})"),
        });
    }
    Ok((text, usage))
}

/// Token counts and the provider cost from a `usage` object.
fn usage_of(usage: Option<&Value>) -> Usage {
    let at = |path: &[&str]| {
        path.iter()
            .try_fold(usage?, |v, k| v.get(*k))
            .and_then(Value::as_u64)
    };
    Usage {
        tokens_in: at(&["prompt_tokens"]).unwrap_or(0),
        tokens_out: at(&["completion_tokens"]).unwrap_or(0),
        reasoning_tokens: at(&["completion_tokens_details", "reasoning_tokens"]),
        cached_tokens_in: at(&["prompt_tokens_details", "cached_tokens"]),
        cost: match usage.and_then(|u| u.get("cost")).and_then(Value::as_f64) {
            Some(c) if c.is_finite() => Cost::Provider(c),
            _ => Cost::Unknown,
        },
    }
}

/// Drops a leading `<think>…</think>` block some reasoning models emit.
fn strip_think(s: &str) -> &str {
    let t = s.trim_start();
    if let Some(rest) = t.strip_prefix("<think>") {
        return match rest.find("</think>") {
            Some(end) => &rest[end + "</think>".len()..],
            None => "",
        };
    }
    s
}

fn snippet(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(SNIPPET_CHARS).collect();
    if s.chars().count() > SNIPPET_CHARS {
        out.push('…');
    }
    out
}

fn is_env_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;
