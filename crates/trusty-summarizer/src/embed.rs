//! The local embedding model and the classifier built on it.
//!
//! Why: plan section 1, step 3. Classification by embedding runs in-process,
//! needs no Python and no server, and spends no LLM tokens. The same model
//! ranks sentences for the `extractive` summary mode.
//!
//! What: [`Embedder`] turns texts into unit vectors. [`LocalEmbedder`] is
//! fastembed-rs on ONNX Runtime (default all-MiniLM-L6-v2, 384 dimensions);
//! [`LocalEmbedder::shared`] hands out one instance per (model, cache dir)
//! per process, and the model loads on first use, once.
//! [`EmbeddingClassifier`] scores a document against label prototypes
//! (`"<name>: <description>"`) by cosine similarity of the mean of the
//! document's chunk embeddings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::backend::{ClassifierBackend, ClassifyResponse, Usage};
use crate::config::Label;
use crate::error::{Error, Result};

/// The default model name.
pub const DEFAULT_MODEL: &str = "all-MiniLM-L6-v2";
/// The `type` value that selects the embedding classifier.
pub const EMBEDDING_TYPE: &str = "embedding";

/// Turns texts into vectors.
///
/// Why: the classifier and the embedding summary ranking need only this, so
/// tests substitute fixed vectors and never load a model.
/// What: one vector per text, in order, each of unit length. Shared across
/// the batch's threads.
pub trait Embedder: Send + Sync {
    /// A stable name for errors, e.g. `all-MiniLM-L6-v2`.
    fn name(&self) -> &str;
    /// Embeds `texts`.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Which model, cached where: the options shared by every user of the
/// local model (`model`, `cache_dir`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelSpec {
    /// The model name as configured.
    pub name: String,
    /// Where the model files are kept.
    pub cache_dir: PathBuf,
}

impl ModelSpec {
    /// Reads `model` and `cache_dir` from `options`, ignoring other keys.
    /// Default model [`DEFAULT_MODEL`]; default cache `$FASTEMBED_CACHE_DIR`,
    /// else `~/.cache/fastembed`.
    pub fn from_options(context: &str, options: &toml::Table) -> Result<Self> {
        let name = match options.get("model") {
            None => DEFAULT_MODEL.to_string(),
            Some(toml::Value::String(s)) => s.clone(),
            Some(v) => {
                return Err(Error::Config(format!(
                    "{context}: model = {v} is not a string"
                )));
            }
        };
        parse_model(&name).map_err(|e| Error::Config(format!("{context}: {e}")))?;
        let cache_dir = match options.get("cache_dir") {
            Some(toml::Value::String(s)) => PathBuf::from(s),
            Some(v) => {
                return Err(Error::Config(format!(
                    "{context}: cache_dir = {v} is not a string"
                )));
            }
            None => default_cache_dir()?,
        };
        Ok(ModelSpec { name, cache_dir })
    }
}

fn default_cache_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("FASTEMBED_CACHE_DIR") {
        return Ok(PathBuf::from(d));
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".cache").join("fastembed"))
        .ok_or_else(|| Error::Config("no model cache directory: set cache_dir or HOME".into()))
}

fn parse_model(name: &str) -> std::result::Result<EmbeddingModel, String> {
    match name {
        "all-MiniLM-L6-v2" => Ok(EmbeddingModel::AllMiniLML6V2),
        "bge-small-en-v1.5" => Ok(EmbeddingModel::BGESmallENV15),
        other => other.parse::<EmbeddingModel>(),
    }
}

/// fastembed-rs on ONNX Runtime, loaded once on first use.
///
/// Why: loading the model costs far more than embedding one file; a batch
/// (and a later warm server) must pay it once.
/// What: [`LocalEmbedder::shared`] returns the process-wide instance for a
/// [`ModelSpec`]. The first [`Embedder::embed`] call loads the model,
/// downloading it into `cache_dir` if absent, and prints the load time to
/// stderr. A load failure is remembered and returned to every later call
/// as [`Error::Backend`], so each file gets an error record naming it.
/// `TextEmbedding::embed` needs `&mut`, so calls serialize on a mutex.
/// Test: live check in the README (loads a real model); the shared-instance
/// rule is `shared_embedder_is_one_per_spec`.
pub struct LocalEmbedder {
    spec: ModelSpec,
    model: OnceLock<std::result::Result<Mutex<TextEmbedding>, String>>,
}

impl std::fmt::Debug for LocalEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEmbedder")
            .field("spec", &self.spec)
            .field("loaded", &self.model.get().is_some())
            .finish()
    }
}

impl LocalEmbedder {
    /// The process-wide embedder for `spec`; never loads the model itself.
    pub fn shared(spec: ModelSpec) -> Arc<LocalEmbedder> {
        static ALL: OnceLock<Mutex<HashMap<ModelSpec, Arc<LocalEmbedder>>>> = OnceLock::new();
        let mut all = ALL
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        Arc::clone(all.entry(spec.clone()).or_insert_with(|| {
            Arc::new(LocalEmbedder {
                spec,
                model: OnceLock::new(),
            })
        }))
    }

    fn load(&self) -> std::result::Result<Mutex<TextEmbedding>, String> {
        let model = parse_model(&self.spec.name)?;
        std::fs::create_dir_all(&self.spec.cache_dir)
            .map_err(|e| format!("cannot create {}: {e}", self.spec.cache_dir.display()))?;
        let started = Instant::now();
        let options = TextInitOptions::new(model)
            .with_cache_dir(self.spec.cache_dir.clone())
            .with_show_download_progress(false);
        let embedding = TextEmbedding::try_new(options).map_err(|e| {
            format!(
                "cannot load model {} (cache {}): {e}",
                self.spec.name,
                self.spec.cache_dir.display()
            )
        })?;
        eprintln!(
            "trusty-summarizer: loaded embedding model {} in {} ms (cache {})",
            self.spec.name,
            started.elapsed().as_millis(),
            self.spec.cache_dir.display()
        );
        Ok(Mutex::new(embedding))
    }
}

impl Embedder for LocalEmbedder {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let fail = |message: String| Error::Backend {
            backend: self.spec.name.clone(),
            message,
            usage: None,
        };
        let model = self
            .model
            .get_or_init(|| self.load())
            .as_ref()
            .map_err(|e| fail(e.clone()))?;
        let mut model = model.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = model
            .embed(texts, None)
            .map_err(|e| fail(format!("embedding failed: {e}")))?;
        if out.len() != texts.len() {
            return Err(fail(format!(
                "expected {} vectors, got {}",
                texts.len(),
                out.len()
            )));
        }
        out.iter_mut().for_each(|v| normalize(v));
        Ok(out)
    }
}

/// Scales `v` to unit length; a zero vector stays zero.
pub(crate) fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// Dot product; the cosine for unit vectors.
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The normalized mean of `vectors`; `None` when empty or of mixed length.
pub(crate) fn centroid(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    if vectors.iter().any(|v| v.len() != first.len()) {
        return None;
    }
    let mut c = vec![0.0f32; first.len()];
    for v in vectors {
        c.iter_mut().zip(v).for_each(|(a, b)| *a += b);
    }
    normalize(&mut c);
    Some(c)
}

/// The prototype text embedded for a label.
pub(crate) fn label_prototype(label: &Label) -> String {
    if label.description.trim().is_empty() {
        label.name.clone()
    } else {
        format!("{}: {}", label.name, label.description)
    }
}

/// Splits `text` into at most `max_chunks` chunks of about `chunk_chars`
/// characters, cut at whitespace, taken evenly across the text.
pub(crate) fn chunks(text: &str, chunk_chars: usize, max_chunks: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut all = Vec::new();
    let mut cur = String::new();
    for w in words {
        if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > chunk_chars {
            all.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        all.push(cur);
    }
    if all.len() <= max_chunks || max_chunks == 0 {
        return all;
    }
    // Evenly spaced, always keeping the first chunk.
    (0..max_chunks)
        .map(|i| all[i * all.len() / max_chunks].clone())
        .collect()
}

/// Classifies by cosine similarity to label prototypes; registered as
/// `local`, or any name with `type = "embedding"`.
///
/// Why: a label per file for no LLM tokens.
/// What: embeds up to `max_chunks` chunks of `chunk_chars` characters of
/// each text, averages them, and scores the mean against each label's
/// prototype vector. Label vectors are computed once per label and kept.
/// Scores are cosines in `[-1, 1]`; usage is always zero.
/// Test: `embedding_classifier_scores_by_cosine`, `chunks_are_bounded_and_spread`
pub struct EmbeddingClassifier {
    embedder: Arc<dyn Embedder>,
    chunk_chars: usize,
    max_chunks: usize,
    labels: Mutex<HashMap<String, Vec<f32>>>,
}

impl std::fmt::Debug for EmbeddingClassifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingClassifier")
            .field("embedder", &self.embedder.name())
            .field("chunk_chars", &self.chunk_chars)
            .field("max_chunks", &self.max_chunks)
            .finish_non_exhaustive()
    }
}

impl EmbeddingClassifier {
    /// A classifier over `embedder` with default chunking (1000 chars, 8).
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        EmbeddingClassifier {
            embedder,
            chunk_chars: 1000,
            max_chunks: 8,
            labels: Mutex::new(HashMap::new()),
        }
    }

    /// Builds from `[backends.<name>]`: `model`, `cache_dir` (see
    /// [`ModelSpec::from_options`]), `chunk_chars` and `max_chunks`.
    pub fn from_options(name: &str, options: &toml::Table) -> Result<Self> {
        let context = format!("backends.{name}");
        let spec = ModelSpec::from_options(&context, options)?;
        let mut c = EmbeddingClassifier::new(LocalEmbedder::shared(spec));
        for (key, value) in options {
            match (key.as_str(), value) {
                ("type" | "model" | "cache_dir", _) => {}
                ("chunk_chars", toml::Value::Integer(n)) if *n >= 50 => c.chunk_chars = *n as usize,
                ("max_chunks", toml::Value::Integer(n)) if *n >= 1 => c.max_chunks = *n as usize,
                _ => {
                    return Err(Error::Config(format!(
                        "{context}: unsupported option `{key} = {value}`"
                    )));
                }
            }
        }
        Ok(c)
    }

    fn label_vectors(&self, labels: &[Label]) -> Result<Vec<Vec<f32>>> {
        let prototypes: Vec<String> = labels.iter().map(label_prototype).collect();
        let mut known = self.labels.lock().unwrap_or_else(|p| p.into_inner());
        let missing: Vec<&str> = prototypes
            .iter()
            .filter(|p| !known.contains_key(p.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            let vectors = self.embedder.embed(&missing)?;
            for (p, v) in missing.iter().zip(vectors) {
                known.insert((*p).to_string(), v);
            }
        }
        Ok(prototypes.iter().map(|p| known[p].clone()).collect())
    }
}

impl ClassifierBackend for EmbeddingClassifier {
    fn classify(&self, texts: &[&str], labels: &[Label]) -> Result<ClassifyResponse> {
        let label_vecs = self.label_vectors(labels)?;
        let mut scores = Vec::with_capacity(texts.len());
        for text in texts {
            let parts = chunks(text, self.chunk_chars, self.max_chunks);
            let row = if parts.is_empty() {
                vec![0.0; labels.len()]
            } else {
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                let doc = centroid(&self.embedder.embed(&refs)?).ok_or_else(|| Error::Backend {
                    backend: self.embedder.name().to_string(),
                    message: "vectors of mixed length".into(),
                    usage: None,
                })?;
                label_vecs.iter().map(|l| dot(&doc, l)).collect()
            };
            scores.push(row);
        }
        Ok(ClassifyResponse {
            scores,
            usage: Usage::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed 3-d vectors by keyword; counts calls.
    struct FixedEmbedder(std::sync::atomic::AtomicUsize);

    impl Embedder for FixedEmbedder {
        fn name(&self) -> &str {
            "fixed"
        }
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![
                        t.matches("invoice").count() as f32,
                        t.matches("recipe").count() as f32,
                        0.1,
                    ];
                    normalize(&mut v);
                    v
                })
                .collect())
        }
    }

    fn label(name: &str, description: &str) -> Label {
        Label {
            name: name.into(),
            description: description.into(),
        }
    }

    #[test]
    fn embedding_classifier_scores_by_cosine() {
        let e = Arc::new(FixedEmbedder(Default::default()));
        let c = EmbeddingClassifier::new(e.clone());
        let labels = [label("invoice", "a bill"), label("recipe", "cooking steps")];
        let r = c
            .classify(
                &["invoice total due, invoice number 7", "a recipe"],
                &labels,
            )
            .unwrap();
        assert_eq!(r.scores.len(), 2);
        assert!(r.scores[0][0] > r.scores[0][1], "{:?}", r.scores);
        assert!(r.scores[1][1] > r.scores[1][0], "{:?}", r.scores);
        assert!(
            r.scores
                .iter()
                .flatten()
                .all(|s| (-1.0..=1.0001).contains(s))
        );
        assert_eq!(r.usage, Usage::default());
        // Label vectors are computed once: a second call embeds only the text.
        let before = e.0.load(std::sync::atomic::Ordering::SeqCst);
        c.classify(&["invoice"], &labels).unwrap();
        assert_eq!(e.0.load(std::sync::atomic::Ordering::SeqCst), before + 1);
    }

    #[test]
    fn chunks_are_bounded_and_spread() {
        let text = (0..1000)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let c = chunks(&text, 100, 4);
        assert_eq!(c.len(), 4);
        assert!(c.iter().all(|p| p.chars().count() <= 100));
        assert!(c[0].starts_with("w0 "));
        assert!(chunks("  ", 100, 4).is_empty());
    }

    #[test]
    fn centroid_is_unit_mean() {
        let c = centroid(&[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        assert!((c[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!(centroid(&[vec![1.0], vec![1.0, 2.0]]).is_none());
        assert!(centroid(&[]).is_none());
    }

    #[test]
    fn shared_embedder_is_one_per_spec() {
        let spec = |n: &str| ModelSpec {
            name: DEFAULT_MODEL.into(),
            cache_dir: PathBuf::from(n),
        };
        let a = LocalEmbedder::shared(spec("/tmp/a"));
        let b = LocalEmbedder::shared(spec("/tmp/a"));
        let c = LocalEmbedder::shared(spec("/tmp/b"));
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(a.model.get().is_none(), "shared() must not load the model");
    }

    #[test]
    fn model_options_are_checked() {
        let t: toml::Table = toml::from_str("model = \"no-such-model\"").unwrap();
        assert!(ModelSpec::from_options("x", &t).is_err());
        let t: toml::Table = toml::from_str("cache_dir = \"/tmp/m\"").unwrap();
        assert_eq!(
            ModelSpec::from_options("x", &t).unwrap().name,
            DEFAULT_MODEL
        );
        let t: toml::Table = toml::from_str("colour = 1").unwrap();
        assert!(EmbeddingClassifier::from_options("x", &t).is_err());
    }
}
