//! Summary modes, the bounded excerpt, mode-separated caching, endpoint
//! failures and the embedding classifier through the pipeline. No network,
//! no Ollama, no model download.

use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_summarizer::record::Stage;
use trusty_summarizer::{
    BackendRegistry, CacheStatus, ClassResult, ClassifierBackend, Config, Converters, Embedder,
    EmbeddingClassifier, FileRecord, InferenceBackend, InferenceRequest, InferenceResponse, Result,
    Summarizer, SummaryRecord, Usage,
};

/// Records the character count of every input it is sent.
struct RecordingBackend {
    inputs: Arc<Mutex<Vec<usize>>>,
}

impl InferenceBackend for RecordingBackend {
    fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        let n = request.input.chars().count();
        self.inputs.lock().unwrap().push(n);
        Ok(InferenceResponse {
            text: format!("LLM summary of {n} chars"),
            usage: Usage {
                tokens_in: (n / 4) as u64,
                tokens_out: 6,
                ..Usage::default()
            },
            latency: Duration::ZERO,
        })
    }
}

fn registry(inputs: &Arc<Mutex<Vec<usize>>>) -> BackendRegistry {
    let mut r = BackendRegistry::with_builtins();
    let inputs = Arc::clone(inputs);
    r.register_inference("test-llm", move |_| {
        Ok(Arc::new(RecordingBackend {
            inputs: Arc::clone(&inputs),
        }) as Arc<dyn InferenceBackend>)
    })
    .unwrap();
    r
}

fn config(cache: Option<&Path>, toml: &str) -> Config {
    let mut c = Config::from_toml(toml).unwrap();
    match cache {
        Some(dir) => c.cache.dir = Some(dir.to_path_buf()),
        None => c.cache.enabled = false,
    }
    c
}

fn ok(record: &FileRecord) -> &SummaryRecord {
    match record {
        FileRecord::Ok(r) => r,
        FileRecord::Error(e) => panic!("unexpected error record: {e:?}"),
    }
}

fn run(s: &Summarizer, files: &[&Path]) -> Vec<FileRecord> {
    let mut out = Vec::new();
    let roots: Vec<_> = files.iter().map(|p| p.to_path_buf()).collect();
    s.summarize_batch(&roots, |r| {
        out.push(r);
        Ok(())
    })
    .unwrap();
    out
}

#[test]
fn llm_mode_makes_one_call_per_file_on_a_bounded_excerpt() {
    const BOUND: usize = 2_000;
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("small.md");
    std::fs::write(&small, "# Small\n\nA short note.\n").unwrap();
    let huge = dir.path().join("huge.md");
    let mut text = String::from("# Survey\n\nThe opening says what the survey covers.\n\n");
    for i in 0..3_000 {
        text.push_str(&format!(
            "## Pier {i}\n\nPier {i} has lights; some were dim and some were dark.\n\n"
        ));
    }
    assert!(text.chars().count() > 100 * BOUND);
    std::fs::write(&huge, &text).unwrap();
    let code = dir.path().join("main.rs");
    std::fs::write(&code, "fn main() {\n    println!(\"hi\");\n}\n".repeat(400)).unwrap();

    let inputs = Arc::new(Mutex::new(Vec::new()));
    let s = Summarizer::new(
        config(
            None,
            &format!("[summary]\nbackend = \"test-llm\"\nmax_input_chars = {BOUND}"),
        ),
        &registry(&inputs),
        Converters::with_builtins(),
    )
    .unwrap();
    let records = run(&s, &[&small, &huge, &code]);

    let sent = inputs.lock().unwrap().clone();
    assert_eq!(sent.len(), 3, "exactly one inference call per file");
    assert!(sent.iter().all(|&n| n <= BOUND), "{sent:?}");
    for r in &records {
        let r = ok(r);
        let stage = r
            .stages
            .iter()
            .find(|s| s.stage == Stage::Summarize)
            .unwrap();
        let chars = stage.chars_in.expect("record reports characters sent");
        assert!(chars <= BOUND);
        assert_eq!(r.summary, format!("LLM summary of {chars} chars"));
        assert_eq!(stage.tokens_out, 6);
    }
    let huge_sent = ok(&records[1]).stages[3].chars_in.unwrap();
    assert!(
        huge_sent > BOUND / 2,
        "the bound is used, not wasted: {huge_sent}"
    );
    assert_eq!(
        ok(&records[0]).stages[3].chars_in,
        Some("# Small\n\nA short note.\n".chars().count()),
        "a short file is sent whole"
    );
}

#[test]
fn cache_entries_never_serve_another_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.md");
    std::fs::write(
        &file,
        "# A\n\nThe cache keeps each mode apart from the others.\n",
    )
    .unwrap();
    let cache = dir.path().join("cache");
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let reg = registry(&inputs);
    let mode = |backend: &str| {
        Summarizer::new(
            config(Some(&cache), &format!("[summary]\nbackend = \"{backend}\"")),
            &reg,
            Converters::with_builtins(),
        )
        .unwrap()
    };

    let tf = mode("extractive-tf");
    assert_eq!(ok(&tf.summarize_file(&file)).cache, CacheStatus::Miss);
    assert_eq!(ok(&tf.summarize_file(&file)).cache, CacheStatus::Hit);
    let llm = mode("test-llm");
    let r = llm.summarize_file(&file);
    assert_eq!(ok(&r).cache, CacheStatus::Miss, "another mode recomputes");
    assert!(ok(&r).summary.starts_with("LLM summary"));
    let r = tf.summarize_file(&file);
    assert_eq!(ok(&r).cache, CacheStatus::Hit);
    assert_eq!(ok(&r).backends.summary, "extractive-tf");
}

#[test]
fn endpoint_failures_are_error_records_and_the_batch_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "# A\n\nFirst.\n").unwrap();
    std::fs::write(&b, "Second.\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cases = [
        (
            format!("base_url = \"http://127.0.0.1:{port}/v1\""),
            "cannot reach",
        ),
        (
            "base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"TRUSTY_SUMMARIZER_TEST_UNSET_KEY_VAR\""
                .to_string(),
            "environment variable TRUSTY_SUMMARIZER_TEST_UNSET_KEY_VAR is not set",
        ),
    ];
    for (endpoint, cause) in cases {
        let s = Summarizer::new(
            config(
                Some(&dir.path().join("cache")),
                &format!(
                    "[summary]\nbackend = \"ep\"\n[backends.ep]\ntype = \"openai\"\nmodel = \"m\"\ntimeout_ms = 2000\n{endpoint}"
                ),
            ),
            &BackendRegistry::with_builtins(),
            Converters::with_builtins(),
        )
        .unwrap();
        let records = run(&s, &[&a, &b]);
        assert_eq!(records.len(), 2, "the batch goes on");
        for r in &records {
            let FileRecord::Error(e) = r else {
                panic!("no silent fallback to another mode: {r:?}");
            };
            assert_eq!(e.error.kind, "backend_failed");
            assert!(e.error.message.contains("`ep`"), "{}", e.error.message);
            assert!(e.error.message.contains(cause), "{}", e.error.message);
        }
    }
}

/// Fixed vectors: a "survey" axis, a "code" axis and a constant.
struct FixedEmbedder;

impl Embedder for FixedEmbedder {
    fn name(&self) -> &str {
        "fixed"
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                let survey = ["survey", "findings", "report"]
                    .iter()
                    .map(|w| t.matches(w).count() as f32)
                    .sum::<f32>();
                let code = ["fn ", "source code", "println"]
                    .iter()
                    .map(|w| t.matches(w).count() as f32)
                    .sum::<f32>();
                let n = (survey * survey + code * code + 0.01).sqrt();
                vec![survey / n, code / n, 0.1 / n]
            })
            .collect())
    }
}

#[test]
fn embedding_classifier_labels_files_with_the_default_set() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("r.md");
    std::fs::write(
        &report,
        "# Survey\n\nThe survey findings: seven lights were dim.\n",
    )
    .unwrap();
    let code = dir.path().join("m.rs");
    std::fs::write(&code, "fn main() {\n    println!(\"x\");\n}\n").unwrap();
    let mut reg = BackendRegistry::with_builtins();
    reg.register_classifier("fixed", |_| {
        Ok(Arc::new(EmbeddingClassifier::new(Arc::new(FixedEmbedder)))
            as Arc<dyn ClassifierBackend>)
    })
    .unwrap();
    let s = Summarizer::new(
        config(None, "[classifier]\nbackend = \"fixed\""),
        &reg,
        Converters::with_builtins(),
    )
    .unwrap();
    let records = run(&s, &[&report, &code]);
    let label = |r: &FileRecord| match &ok(r).class {
        ClassResult::Classified {
            label, runner_up, ..
        } => (label.clone(), runner_up.clone().expect("runner-up").label),
        other => panic!("{other:?}"),
    };
    assert_eq!(label(&records[0]).0, "report");
    assert_eq!(label(&records[1]).0, "source code");
    let classify = ok(&records[0])
        .stages
        .iter()
        .find(|s| s.stage == Stage::Classify)
        .unwrap();
    assert_eq!((classify.tokens_in, classify.tokens_out), (0, 0));
}

#[test]
fn example_config_is_valid() {
    // Building the summarizer loads no model and contacts no endpoint.
    let text = include_str!("../config.example.toml");
    let mut c = Config::from_toml(text).unwrap();
    c.cache.enabled = false;
    assert_eq!(c.summary.backend, "llm");
    assert_eq!(c.labels().len(), 4);
    Summarizer::new(
        c,
        &BackendRegistry::with_builtins(),
        Converters::with_builtins(),
    )
    .unwrap();
}

/// Serves `n` connections, each answered with HTTP 200 and `body`.
fn serve_n(n: usize, body: &'static str) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for _ in 0..n {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut buf = vec![0; len];
            reader.read_exact(&mut buf).unwrap();
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

/// The Atlas Cloud answer seen live: empty content, the budget spent on reasoning.
const BUDGET_ON_REASONING: &str = r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"Let me think..."},"finish_reason":"length"}],"usage":{"prompt_tokens":294,"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":102},"total_tokens":394}}"#;

#[test]
fn failed_calls_record_the_usage_the_provider_bills() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n\nFirst paragraph of a test document.\n").unwrap();
    // One summary call, then one labelling call, each answered with an empty
    // answer whose budget went to reasoning.
    for (role, stage) in [
        ("summary", Stage::Summarize),
        ("classifier", Stage::Classify),
    ] {
        let url = serve_n(1, BUDGET_ON_REASONING);
        let endpoint = format!(
            "[backends.ep]\ntype = \"openai\"\nmodel = \"m\"\ntimeout_ms = 5000\nbase_url = \"{url}\"\nprice_in_per_mtok = 1.0\nprice_out_per_mtok = 2.0\n"
        );
        let toml = if role == "summary" {
            format!("[summary]\nbackend = \"ep\"\n{endpoint}")
        } else {
            format!(
                "[classifier]\nbackend = \"lab\"\n[backends.lab]\ntype = \"llm\"\ninference = \"ep\"\n{endpoint}"
            )
        };
        let s = Summarizer::new(
            config(None, &toml),
            &BackendRegistry::with_builtins(),
            Converters::with_builtins(),
        )
        .unwrap();
        let records = run(&s, &[&a]);
        let FileRecord::Error(e) = &records[0] else {
            panic!("{role}: expected an error record: {:?}", records[0]);
        };
        assert!(
            e.error.message.contains("output budget spent on reasoning"),
            "{role}: {}",
            e.error.message
        );
        let st = e
            .stages
            .iter()
            .find(|x| x.stage == stage)
            .unwrap_or_else(|| panic!("{role}: no {stage:?} stage in {:?}", e.stages));
        assert_eq!((st.tokens_in, st.tokens_out), (294, 100), "{role}");
        assert_eq!(st.reasoning_tokens, Some(102), "{role}");
        assert_eq!(st.cost_source, "configured_prices", "{role}");
        let want = (294.0 * 1.0 + 100.0 * 2.0) / 1e6;
        assert!(
            (st.cost_usd.unwrap() - want).abs() < 1e-12,
            "{role}: {:?}",
            st.cost_usd
        );
    }
}

/// Answers every request with whitespace only.
struct EmptyBackend;

impl InferenceBackend for EmptyBackend {
    fn complete(&self, _: &InferenceRequest) -> Result<InferenceResponse> {
        Ok(InferenceResponse {
            text: " \n".into(),
            usage: Usage::default(),
            latency: Duration::ZERO,
        })
    }
}

/// Every `.json` file under `dir`, recursively.
fn json_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            json_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

#[test]
fn an_empty_summary_is_an_error_record_and_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("table.md");
    std::fs::write(&file, "# 2024\n\n| a | b |\n|---|---|\n| 1 | 2 |\n").unwrap();
    let cache = dir.path().join("cache");
    let mut reg = BackendRegistry::with_builtins();
    reg.register_inference("test-empty", |_| {
        Ok(Arc::new(EmptyBackend) as Arc<dyn InferenceBackend>)
    })
    .unwrap();
    for mode in ["extractive-tf", "extractive", "test-empty"] {
        let s = Summarizer::new(
            config(Some(&cache), &format!("[summary]\nbackend = \"{mode}\"")),
            &reg,
            Converters::with_builtins(),
        )
        .unwrap();
        for round in 0..2 {
            match s.summarize_file(&file) {
                FileRecord::Error(e) => {
                    assert_eq!(e.error.kind, "empty_summary", "{mode}");
                    assert!(
                        e.error.message.contains(mode),
                        "{mode}: {}",
                        e.error.message
                    );
                }
                FileRecord::Ok(r) => {
                    panic!(
                        "{mode} round {round}: ok record with summary {:?}",
                        r.summary
                    )
                }
            }
        }
    }

    // An `ok` entry with an empty summary written by an earlier build is not served.
    let file = dir.path().join("prose.md");
    std::fs::write(
        &file,
        "# P\n\nThis document has one plain prose sentence.\n",
    )
    .unwrap();
    let s = Summarizer::new(
        config(Some(&cache), "[summary]\nbackend = \"extractive-tf\""),
        &reg,
        Converters::with_builtins(),
    )
    .unwrap();
    assert_eq!(ok(&s.summarize_file(&file)).cache, CacheStatus::Miss);
    let mut entries = Vec::new();
    json_files(&cache, &mut entries);
    let mut emptied = 0;
    for p in entries {
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        if v.get("summary").is_some() {
            v["summary"] = serde_json::Value::String(String::new());
            std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
            emptied += 1;
        }
    }
    assert_eq!(emptied, 1);
    let r = s.summarize_file(&file);
    assert_eq!(
        ok(&r).cache,
        CacheStatus::Miss,
        "the empty entry is recomputed"
    );
    assert!(!ok(&r).summary.trim().is_empty());
}
