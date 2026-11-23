//! The inference and classifier backend interfaces, and name-based selection.
//!
//! Why: plan section 2. Callers name a backend in configuration; adding one
//! means registering a factory under a new name. No caller changes.
//!
//! What: [`InferenceBackend`] produces text from an instruction and an input;
//! [`ClassifierBackend`] scores texts against labels. [`BackendRegistry`]
//! maps names to factories that build a backend from its
//! `[backends.<name>]` options table.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Config, Label, NO_CLASSIFIER};
use crate::embed::{EMBEDDING_TYPE, EmbeddingClassifier};
use crate::error::{Error, Result};
use crate::extractive::ExtractiveBackend;
use crate::labeler::{LLM_TYPE, LlmClassifier};
use crate::openai::{OPENAI_TYPE, OpenAiBackend};

/// Model usage for one call. All zero, cost [`Cost::None`], for a backend
/// that runs no billable model.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Prompt tokens billed.
    pub tokens_in: u64,
    /// Completion tokens billed (reasoning included, as providers bill it).
    pub tokens_out: u64,
    /// Of the completion tokens, those spent on reasoning, when the
    /// response reports them (`completion_tokens_details.reasoning_tokens`).
    pub reasoning_tokens: Option<u64>,
    /// Of the prompt tokens, those served from the provider's prompt cache,
    /// when reported (`prompt_tokens_details.cached_tokens`).
    pub cached_tokens_in: Option<u64>,
    /// What the call cost, and where that number came from.
    pub cost: Cost,
}

impl Usage {
    /// Adds `other` to this usage: counts sum, an absent detail count stays
    /// absent only when both are absent, costs combine by [`Cost::plus`].
    pub fn add(&mut self, other: &Usage) {
        let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.reasoning_tokens = sum(self.reasoning_tokens, other.reasoning_tokens);
        self.cached_tokens_in = sum(self.cached_tokens_in, other.cached_tokens_in);
        self.cost = self.cost.plus(other.cost);
    }
}

/// The cost of a call in US dollars, with its source.
///
/// Why: a provider that reports no cost must not read as free; the record
/// says whether a number is the provider's, computed from configured
/// prices, or unknown.
/// What: serialized in a stage as `cost_usd` (a number, or null when
/// unknown) and `cost_source` (`none`, `provider`, `configured_prices`,
/// `unknown`).
/// Test: `cost_comes_from_provider_or_prices_or_is_unknown`
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Cost {
    /// No billable model ran (local backends, non-model stages): 0.
    #[default]
    None,
    /// The provider's own `usage.cost`.
    Provider(f64),
    /// Tokens times the prices configured for the backend.
    ConfiguredPrices(f64),
    /// A billable call ran, and neither a provider cost nor prices exist.
    Unknown,
}

impl Cost {
    /// The amount, `None` when unknown.
    pub fn usd(self) -> Option<f64> {
        match self {
            Cost::None => Some(0.0),
            Cost::Provider(x) | Cost::ConfiguredPrices(x) => Some(x),
            Cost::Unknown => None,
        }
    }

    /// The `cost_source` string.
    pub fn source(self) -> &'static str {
        match self {
            Cost::None => "none",
            Cost::Provider(_) => "provider",
            Cost::ConfiguredPrices(_) => "configured_prices",
            Cost::Unknown => "unknown",
        }
    }

    /// The sum of two costs. Unknown wins over any amount; a sum of a
    /// provider cost and a computed one is reported as computed.
    pub fn plus(self, other: Cost) -> Cost {
        match (self, other) {
            (Cost::None, x) | (x, Cost::None) => x,
            (Cost::Unknown, _) | (_, Cost::Unknown) => Cost::Unknown,
            (Cost::Provider(a), Cost::Provider(b)) => Cost::Provider(a + b),
            (a, b) => Cost::ConfiguredPrices(a.usd().unwrap_or(0.0) + b.usd().unwrap_or(0.0)),
        }
    }
}

/// API keys a client handed over for one batch, by environment variable
/// name.
///
/// Why: a warm server outlives the shell that started it, so it cannot read
/// a key injected into a later CLI call's environment; the CLI passes the
/// keys its config names with each request instead (plan section 4).
/// What: [`Credentials::process_env`] (the default) means "read the
/// process environment at call time", the one-shot path. A
/// [`Credentials::scoped`] set is the only source of keys for the requests
/// that carry it: a name absent from the set is a missing key, even when
/// the process environment has it. The values never appear in `Debug`
/// output, and their bytes are overwritten when the last holder drops them.
/// Test: `scoped_credentials_replace_the_environment`
#[derive(Clone, Default)]
pub struct Credentials(Option<Arc<KeySet>>);

struct KeySet(BTreeMap<String, String>);

impl Drop for KeySet {
    fn drop(&mut self) {
        for value in std::mem::take(&mut self.0).into_values() {
            let mut bytes = value.into_bytes();
            bytes.fill(0);
            std::hint::black_box(&bytes);
        }
    }
}

/// Where a backend gets the key named `var`.
#[derive(Debug, PartialEq, Eq)]
pub enum KeySource<'a> {
    /// Read the process environment.
    ProcessEnv,
    /// The client passed this value.
    Given(&'a str),
    /// The client passed keys, and not this one.
    NotGiven,
}

impl Credentials {
    /// Keys come from the process environment at call time.
    pub fn process_env() -> Self {
        Credentials(None)
    }

    /// Keys come only from `keys` (variable name to value).
    pub fn scoped(keys: BTreeMap<String, String>) -> Self {
        Credentials(Some(Arc::new(KeySet(keys))))
    }

    /// Where the key named `var` comes from.
    pub fn source(&self, var: &str) -> KeySource<'_> {
        match &self.0 {
            None => KeySource::ProcessEnv,
            Some(set) => match set.0.get(var) {
                Some(v) => KeySource::Given(v),
                None => KeySource::NotGiven,
            },
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => f.write_str("Credentials(process env)"),
            Some(set) => f
                .debug_tuple("Credentials")
                .field(&set.0.keys().collect::<Vec<_>>())
                .finish(),
        }
    }
}

impl PartialEq for Credentials {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => a.0 == b.0,
            _ => false,
        }
    }
}

impl Eq for Credentials {}

/// One inference call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceRequest {
    /// What to do with the input, e.g. "Summarize this document".
    pub instruction: String,
    /// The bounded document excerpt; never the whole of a large document.
    pub input: String,
    /// The output budget.
    pub max_output_tokens: u32,
    /// Where API keys come from for this call.
    pub credentials: Credentials,
}

/// The result of one inference call.
#[derive(Debug, Clone, PartialEq)]
pub struct InferenceResponse {
    /// The produced text.
    pub text: String,
    /// Tokens and cost for this call.
    pub usage: Usage,
    /// Time the backend itself spent, network included.
    pub latency: Duration,
}

/// Produces text from an instruction and an input.
///
/// Why: summaries come from a local model, a remote model, or no model at
/// all, behind one call.
/// What: `complete` returns the text plus the usage it cost. Implementations
/// are shared across the batch's worker threads, so they take `&self`; a
/// blocking call is expected (the pipeline already runs files in parallel).
/// A failure is an [`Error::Backend`] and becomes that file's error record.
/// Test: `test_backend_selected_by_name_without_caller_change`
pub trait InferenceBackend: Send + Sync {
    /// Runs one request.
    fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse>;
}

/// Scores for each text against each label.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyResponse {
    /// `scores[t][l]` is text `t`'s score for label `l`; higher is better.
    /// Must have one row per text and one column per label.
    pub scores: Vec<Vec<f32>>,
    /// Tokens and cost for this call.
    pub usage: Usage,
}

/// Scores texts against a fixed label set.
///
/// Why: classification is cheaper with an embedding model than with an LLM
/// (plan section 1, step 3), and either must be swappable by name.
/// What: `classify` returns one score row per text, one score per label, in
/// the order given. A response of the wrong shape is rejected by the caller
/// as [`Error::Backend`]. Shared across threads, like [`InferenceBackend`].
/// Test: `test_backend_selected_by_name_without_caller_change`
pub trait ClassifierBackend: Send + Sync {
    /// Scores `texts` against `labels`.
    fn classify(&self, texts: &[&str], labels: &[Label]) -> Result<ClassifyResponse>;

    /// [`ClassifierBackend::classify`] with the batch's [`Credentials`]; a
    /// classifier that calls a keyed endpoint overrides this.
    fn classify_with(
        &self,
        texts: &[&str],
        labels: &[Label],
        credentials: &Credentials,
    ) -> Result<ClassifyResponse> {
        let _ = credentials;
        self.classify(texts, labels)
    }
}

/// Builds an inference backend from its options table.
pub type InferenceFactory =
    Arc<dyn Fn(&toml::Table) -> Result<Arc<dyn InferenceBackend>> + Send + Sync>;
/// Builds a classifier backend from its options table.
pub type ClassifierFactory =
    Arc<dyn Fn(&toml::Table) -> Result<Arc<dyn ClassifierBackend>> + Send + Sync>;

/// Builds an inference backend of one `type` from its name and options.
pub type InferenceTypeFactory =
    Arc<dyn Fn(&str, &toml::Table) -> Result<Arc<dyn InferenceBackend>> + Send + Sync>;
/// Builds a classifier backend of one `type` from its name and options.
pub type ClassifierTypeFactory =
    Arc<dyn Fn(&str, &toml::Table) -> Result<Arc<dyn ClassifierBackend>> + Send + Sync>;

/// Backend name to factory, for both roles.
///
/// Why: the one selection path. The CLI uses [`BackendRegistry::with_builtins`];
/// tests register test-only backends the same way.
/// What: a name resolves to the factory registered under it; failing that,
/// to the factory registered for the `type` key of its `[backends.<name>]`
/// table. So one implementation serves many configured endpoints:
/// `[backends.ollama] type = "openai"` and `[backends.openrouter] type =
/// "openai"` are two backends and no code.
/// Test: `test_backend_selected_by_name_without_caller_change`,
/// `unknown_backend_is_a_config_error`, `endpoints_are_configured_by_type`
#[derive(Clone, Default)]
pub struct BackendRegistry {
    inference: BTreeMap<String, InferenceFactory>,
    classifiers: BTreeMap<String, ClassifierFactory>,
    inference_types: BTreeMap<String, InferenceTypeFactory>,
    classifier_types: BTreeMap<String, ClassifierTypeFactory>,
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendRegistry")
            .field("inference", &self.inference.keys().collect::<Vec<_>>())
            .field("classifiers", &self.classifiers.keys().collect::<Vec<_>>())
            .field(
                "inference_types",
                &self.inference_types.keys().collect::<Vec<_>>(),
            )
            .field(
                "classifier_types",
                &self.classifier_types.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl BackendRegistry {
    /// A registry with nothing registered.
    pub fn empty() -> Self {
        BackendRegistry::default()
    }

    /// The backends this build ships. Inference: `extractive` (embedding
    /// ranking, local model), `extractive-tf` (term frequency, no model),
    /// and `type = "openai"` endpoints. Classifiers: `local` (embedding),
    /// `type = "embedding"`, and `type = "llm"` (labels through an inference
    /// backend; see [`BackendRegistry::build_classifier`]). The classifier
    /// name `none` is always available and needs no registration.
    ///
    /// No factory loads a model or opens a connection: the local model loads
    /// on first use, and an endpoint is contacted per call.
    pub fn with_builtins() -> Self {
        let mut r = BackendRegistry::empty();
        r.register_inference("extractive", |opts| {
            Ok(Arc::new(ExtractiveBackend::embedding_from_options(opts)?)
                as Arc<dyn InferenceBackend>)
        })
        .expect("builtin name is valid");
        r.register_inference("extractive-tf", |opts| {
            Ok(Arc::new(ExtractiveBackend::from_options(opts)?) as Arc<dyn InferenceBackend>)
        })
        .expect("builtin name is valid");
        r.register_inference_type(OPENAI_TYPE, |name, opts| {
            Ok(Arc::new(OpenAiBackend::from_options(name, opts)?) as Arc<dyn InferenceBackend>)
        })
        .expect("builtin name is valid");
        r.register_classifier("local", |opts| {
            Ok(Arc::new(EmbeddingClassifier::from_options("local", opts)?)
                as Arc<dyn ClassifierBackend>)
        })
        .expect("builtin name is valid");
        r.register_classifier_type(EMBEDDING_TYPE, |name, opts| {
            Ok(Arc::new(EmbeddingClassifier::from_options(name, opts)?)
                as Arc<dyn ClassifierBackend>)
        })
        .expect("builtin name is valid");
        r
    }

    /// Registers an inference factory for every `[backends.<name>]` table
    /// whose `type` is `kind`. Errors on an empty kind.
    pub fn register_inference_type<F>(&mut self, kind: &str, factory: F) -> Result<()>
    where
        F: Fn(&str, &toml::Table) -> Result<Arc<dyn InferenceBackend>> + Send + Sync + 'static,
    {
        check_name(kind)?;
        self.inference_types
            .insert(kind.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Registers a classifier factory for `type = kind`. Errors on an empty
    /// kind and on `llm`, which [`BackendRegistry::build_classifier`] owns.
    pub fn register_classifier_type<F>(&mut self, kind: &str, factory: F) -> Result<()>
    where
        F: Fn(&str, &toml::Table) -> Result<Arc<dyn ClassifierBackend>> + Send + Sync + 'static,
    {
        check_name(kind)?;
        if kind == LLM_TYPE {
            return Err(Error::Config(format!(
                "classifier type `{LLM_TYPE}` is reserved"
            )));
        }
        self.classifier_types
            .insert(kind.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Builds the classifier `config.classifier.backend` names, with its
    /// options; `Ok(None)` for `none`. A `type = "llm"` table names its
    /// inference backend with `inference = "<name>"`, built from that
    /// backend's own table.
    pub fn build_classifier(&self, config: &Config) -> Result<Option<Arc<dyn ClassifierBackend>>> {
        let name = &config.classifier.backend;
        if name == NO_CLASSIFIER {
            return Ok(None);
        }
        let options = config.backend_options(name);
        if !self.classifiers.contains_key(name) && type_of(&options) == Some(LLM_TYPE) {
            let inner = llm_inference_name(name, &options)?;
            let inference = self.inference(inner, &config.backend_options(inner))?;
            return Ok(Some(Arc::new(LlmClassifier::new(
                name, inference, &options,
            )?)));
        }
        self.classifier(name, &options)
    }

    /// Registers an inference factory under `name`, replacing any earlier
    /// one. Errors on an empty name.
    pub fn register_inference<F>(&mut self, name: &str, factory: F) -> Result<()>
    where
        F: Fn(&toml::Table) -> Result<Arc<dyn InferenceBackend>> + Send + Sync + 'static,
    {
        check_name(name)?;
        self.inference.insert(name.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Registers a classifier factory under `name`. Errors on an empty name
    /// and on the reserved name `none`.
    pub fn register_classifier<F>(&mut self, name: &str, factory: F) -> Result<()>
    where
        F: Fn(&toml::Table) -> Result<Arc<dyn ClassifierBackend>> + Send + Sync + 'static,
    {
        check_name(name)?;
        if name == NO_CLASSIFIER {
            return Err(Error::Config(format!(
                "classifier name `{NO_CLASSIFIER}` is reserved"
            )));
        }
        self.classifiers.insert(name.to_string(), Arc::new(factory));
        Ok(())
    }

    /// Builds the inference backend named `name`.
    pub fn inference(
        &self,
        name: &str,
        options: &toml::Table,
    ) -> Result<Arc<dyn InferenceBackend>> {
        if let Some(factory) = self.inference.get(name) {
            return factory(options);
        }
        if let Some(factory) = type_of(options).and_then(|t| self.inference_types.get(t)) {
            return factory(name, options);
        }
        Err(Error::UnknownBackend {
            role: "inference",
            name: name.to_string(),
            known: known(&self.inference, &self.inference_types),
        })
    }

    /// Builds the classifier named `name`; `Ok(None)` for `none`.
    pub fn classifier(
        &self,
        name: &str,
        options: &toml::Table,
    ) -> Result<Option<Arc<dyn ClassifierBackend>>> {
        if name == NO_CLASSIFIER {
            return Ok(None);
        }
        if let Some(factory) = self.classifiers.get(name) {
            return factory(options).map(Some);
        }
        if let Some(factory) = type_of(options).and_then(|t| self.classifier_types.get(t)) {
            return factory(name, options).map(Some);
        }
        Err(Error::UnknownBackend {
            role: "classifier",
            name: name.to_string(),
            known: format!(
                "{NO_CLASSIFIER}, {}, or type = \"{LLM_TYPE}\"",
                known(&self.classifiers, &self.classifier_types)
            ),
        })
    }
}

/// The `type` option of a backend table, when it is a string.
pub(crate) fn type_of(options: &toml::Table) -> Option<&str> {
    options.get("type").and_then(toml::Value::as_str)
}

/// The `inference` option of a `type = "llm"` classifier table.
pub(crate) fn llm_inference_name<'a>(name: &str, options: &'a toml::Table) -> Result<&'a str> {
    options
        .get("inference")
        .and_then(toml::Value::as_str)
        .filter(|s| !s.trim().is_empty() && *s != name)
        .ok_or_else(|| {
            Error::Config(format!(
                "backends.{name}: a type = \"{LLM_TYPE}\" classifier needs \
                 inference = \"<inference backend name>\""
            ))
        })
}

fn known<A, B>(names: &BTreeMap<String, A>, types: &BTreeMap<String, B>) -> String {
    names
        .keys()
        .cloned()
        .chain(types.keys().map(|t| format!("type = \"{t}\"")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::Config("backend name must not be empty".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_backend_is_a_config_error() {
        let r = BackendRegistry::with_builtins();
        let err = r.inference("ollama", &toml::Table::new()).err().unwrap();
        assert_eq!(err.kind(), "unknown_backend");
        assert!(err.to_string().contains("extractive"));
        assert!(r.classifier("none", &toml::Table::new()).unwrap().is_none());
        assert!(r.classifier("fastembed", &toml::Table::new()).is_err());
    }

    #[test]
    fn endpoints_are_configured_by_type() {
        let r = BackendRegistry::with_builtins();
        let opts = |url: &str| -> toml::Table {
            toml::from_str(&format!(
                "type = \"openai\"\nbase_url = \"{url}\"\nmodel = \"m\""
            ))
            .unwrap()
        };
        // Two names, one implementation, no network at build time.
        assert!(
            r.inference("ollama", &opts("http://127.0.0.1:9/v1"))
                .is_ok()
        );
        assert!(
            r.inference("openrouter", &opts("https://example.invalid/api/v1"))
                .is_ok()
        );
        let err = r
            .inference("x", &toml::from_str("type = \"nope\"").unwrap())
            .err()
            .unwrap();
        assert!(err.to_string().contains("type = \"openai\""), "{err}");

        let c = crate::config::Config::from_toml(
            "[classifier]\nbackend = \"labels\"\n\
             [backends.labels]\ntype = \"llm\"\ninference = \"ep\"\n\
             [backends.ep]\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1\"\nmodel = \"m\"",
        )
        .unwrap();
        assert!(r.build_classifier(&c).unwrap().is_some());
        let c = crate::config::Config::from_toml(
            "[classifier]\nbackend = \"labels\"\n[backends.labels]\ntype = \"llm\"",
        )
        .unwrap();
        assert_eq!(r.build_classifier(&c).err().unwrap().kind(), "config");
    }

    #[test]
    fn scoped_credentials_replace_the_environment() {
        assert_eq!(
            Credentials::process_env().source("HOME"),
            KeySource::ProcessEnv
        );
        let c = Credentials::scoped([("A_KEY".to_string(), "v-secret-1".to_string())].into());
        assert_eq!(c.source("A_KEY"), KeySource::Given("v-secret-1"));
        assert_eq!(c.source("HOME"), KeySource::NotGiven);
        let shown = format!("{c:?}");
        assert!(
            shown.contains("A_KEY") && !shown.contains("v-secret-1"),
            "{shown}"
        );
    }

    #[test]
    fn costs_add_and_unknown_wins() {
        assert_eq!(Cost::None.plus(Cost::Provider(1.0)), Cost::Provider(1.0));
        assert_eq!(Cost::Provider(1.0).plus(Cost::Unknown), Cost::Unknown);
        assert_eq!(
            Cost::Provider(1.0).plus(Cost::ConfiguredPrices(0.5)),
            Cost::ConfiguredPrices(1.5)
        );
        assert_eq!(Cost::Unknown.usd(), None);
        assert_eq!(Cost::None.usd(), Some(0.0));
    }

    #[test]
    fn none_is_reserved_for_classifiers() {
        let mut r = BackendRegistry::empty();
        let res = r.register_classifier("none", |_| unreachable!("never built"));
        assert!(res.is_err());
    }
}
