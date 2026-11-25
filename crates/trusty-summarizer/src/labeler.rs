//! A classifier that labels through any inference backend.
//!
//! Why: plan section 2, "any inference backend used as a labeller", for a
//! remote or model-based classification behind the same
//! [`ClassifierBackend`] interface as the local embedding classifier.
//!
//! What: one inference call per text. The prompt lists the labels with
//! their descriptions and asks for a JSON object mapping each label name to
//! a confidence in `[0, 1]`. Labels the answer omits score 0; an answer
//! that names none of the labels is a malformed response.

use std::sync::Arc;

use serde_json::Value;

use crate::backend::{
    ClassifierBackend, ClassifyResponse, Credentials, InferenceBackend, InferenceRequest, Usage,
};
use crate::config::Label;
use crate::error::{Error, Result};

/// The `type` value that selects this classifier.
pub const LLM_TYPE: &str = "llm";

/// Labels with an inference backend; configured as
/// `[backends.<name>] type = "llm", inference = "<inference backend name>"`.
///
/// Why: lets a model choose the label when the embedding prototypes are not
/// good enough, without a second classifier interface.
/// What: [`ClassifierBackend::classify`] makes one `complete` call per text
/// and parses the scores; usage is the sum over the calls. Errors from the
/// inner backend pass through; an empty or unparseable answer is
/// [`Error::Backend`].
/// Test: `labeler_parses_scores_and_sums_usage`, `labeler_rejects_unusable_answers`
pub struct LlmClassifier {
    name: String,
    inference: Arc<dyn InferenceBackend>,
    max_output_tokens: u32,
}

impl std::fmt::Debug for LlmClassifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClassifier")
            .field("name", &self.name)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish_non_exhaustive()
    }
}

impl LlmClassifier {
    /// Wraps `inference` as the classifier configured under `name`.
    /// `options` may set `max_output_tokens` (default 200); `type` and
    /// `inference` are read by the caller.
    pub fn new(
        name: &str,
        inference: Arc<dyn InferenceBackend>,
        options: &toml::Table,
    ) -> Result<Self> {
        let mut max_output_tokens = 200;
        for (key, value) in options {
            match (key.as_str(), value) {
                ("type" | "inference", toml::Value::String(_)) => {}
                ("max_output_tokens", toml::Value::Integer(n)) if *n >= 1 => {
                    max_output_tokens = u32::try_from(*n).unwrap_or(u32::MAX);
                }
                _ => {
                    return Err(Error::Config(format!(
                        "backends.{name}: unsupported option `{key} = {value}`"
                    )));
                }
            }
        }
        Ok(LlmClassifier {
            name: name.to_string(),
            inference,
            max_output_tokens,
        })
    }

    fn instruction(labels: &[Label]) -> String {
        let mut s = String::from(
            "Classify the document into the labels below. Answer with only a JSON object \
             that maps every label name to a confidence between 0 and 1, for example \
             {\"report\": 0.8, \"other\": 0.1}. No other text.\n\nLabels:\n",
        );
        for l in labels {
            if l.description.trim().is_empty() {
                s.push_str(&format!("- {}\n", l.name));
            } else {
                s.push_str(&format!("- {}: {}\n", l.name, l.description));
            }
        }
        s
    }
}

impl ClassifierBackend for LlmClassifier {
    fn classify(&self, texts: &[&str], labels: &[Label]) -> Result<ClassifyResponse> {
        self.classify_with(texts, labels, &Credentials::process_env())
    }

    fn classify_with(
        &self,
        texts: &[&str],
        labels: &[Label],
        credentials: &Credentials,
    ) -> Result<ClassifyResponse> {
        let instruction = Self::instruction(labels);
        let mut usage = Usage::default();
        let mut scores = Vec::with_capacity(texts.len());
        for text in texts {
            // A failure carries the usage of every call made so far, since
            // the provider bills each of them.
            let r = match self.inference.complete(&InferenceRequest {
                instruction: instruction.clone(),
                input: (*text).to_string(),
                max_output_tokens: self.max_output_tokens,
                credentials: credentials.clone(),
            }) {
                Ok(r) => r,
                Err(Error::Backend {
                    backend,
                    message,
                    usage: failed,
                }) => {
                    if let Some(f) = failed {
                        usage.add(&f);
                    }
                    let billed = (usage != Usage::default()).then_some(usage);
                    return Err(Error::Backend {
                        backend,
                        message,
                        usage: billed,
                    });
                }
                Err(e) => return Err(e),
            };
            usage.add(&r.usage);
            let fail = |message: String| Error::Backend {
                backend: self.name.clone(),
                message,
                usage: Some(usage),
            };
            if r.text.trim().is_empty() {
                return Err(fail("empty answer from the labelling model".into()));
            }
            scores.push(parse_scores(&r.text, labels).map_err(fail)?);
        }
        Ok(ClassifyResponse { scores, usage })
    }
}

/// One score per label from a model's answer.
///
/// Accepts the first `{…}` object in the text; keys match label names
/// case-insensitively; values are numbers, clamped to `[0, 1]`. A bare label
/// name as the whole answer scores that label 1.
pub(crate) fn parse_scores(
    answer: &str,
    labels: &[Label],
) -> std::result::Result<Vec<f32>, String> {
    let trimmed = answer
        .trim()
        .trim_matches(|c| c == '"' || c == '`' || c == '.');
    if let Some(i) = labels
        .iter()
        .position(|l| l.name.eq_ignore_ascii_case(trimmed))
    {
        let mut row = vec![0.0; labels.len()];
        row[i] = 1.0;
        return Ok(row);
    }
    let (Some(start), Some(end)) = (answer.find('{'), answer.rfind('}')) else {
        return Err(format!("answer is not a JSON object: {}", short(answer)));
    };
    if end < start {
        return Err(format!("answer is not a JSON object: {}", short(answer)));
    }
    let v: Value = serde_json::from_str(&answer[start..=end])
        .map_err(|e| format!("answer is not valid JSON ({e}): {}", short(answer)))?;
    let obj = v.as_object().ok_or("answer is not a JSON object")?;
    let mut matched = 0;
    let row = labels
        .iter()
        .map(|l| {
            obj.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&l.name))
                .and_then(|(_, v)| v.as_f64())
                .map(|x| {
                    matched += 1;
                    x.clamp(0.0, 1.0) as f32
                })
                .unwrap_or(0.0)
        })
        .collect();
    if matched == 0 {
        return Err(format!(
            "answer names none of the labels: {}",
            short(answer)
        ));
    }
    Ok(row)
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::backend::InferenceResponse;

    struct Canned(&'static str);

    impl InferenceBackend for Canned {
        fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
            assert!(
                request
                    .instruction
                    .contains("- report: a findings document")
            );
            Ok(InferenceResponse {
                text: self.0.into(),
                usage: Usage {
                    tokens_in: 10,
                    tokens_out: 4,
                    cost: crate::backend::Cost::Provider(0.5),
                    ..Usage::default()
                },
                latency: Duration::ZERO,
            })
        }
    }

    fn labels() -> Vec<Label> {
        vec![
            Label {
                name: "report".into(),
                description: "a findings document".into(),
            },
            Label {
                name: "other".into(),
                description: String::new(),
            },
        ]
    }

    #[test]
    fn labeler_parses_scores_and_sums_usage() {
        let c = LlmClassifier::new(
            "lab",
            Arc::new(Canned("Sure: {\"Report\": 0.9, \"other\": 2}")),
            &toml::Table::new(),
        )
        .unwrap();
        let r = c.classify(&["a", "b"], &labels()).unwrap();
        assert_eq!(r.scores, vec![vec![0.9, 1.0], vec![0.9, 1.0]]);
        assert_eq!((r.usage.tokens_in, r.usage.tokens_out), (20, 8));
        assert_eq!(parse_scores("other", &labels()).unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn labeler_rejects_unusable_answers() {
        let c = LlmClassifier::new("lab", Arc::new(Canned(" \n ")), &toml::Table::new()).unwrap();
        let err = c.classify(&["a"], &labels()).unwrap_err();
        assert_eq!(err.kind(), "backend_failed");
        assert!(err.to_string().contains("empty answer"), "{err}");
        for bad in ["no idea", "{\"x\": 1}", "{not json}"] {
            let c = LlmClassifier::new("lab", Arc::new(Canned(bad)), &toml::Table::new()).unwrap();
            let err = c.classify(&["a"], &labels()).unwrap_err();
            assert_eq!(err.kind(), "backend_failed", "{bad}");
            assert!(err.to_string().contains("`lab`"), "{err}");
        }
    }
}
