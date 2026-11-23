//! Library-level tests: backend selection by name, the cache in both
//! directions, the converter seam and batch error isolation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use trusty_summarizer::record::Stage;
use trusty_summarizer::{
    BackendRegistry, CacheStatus, ClassResult, ClassifierBackend, ClassifyResponse, Config,
    ConvertInput, Converter, Converters, FileRecord, InferenceBackend, InferenceRequest,
    InferenceResponse, Label, LabelScore, Result, Summarizer, SummaryRecord, Usage,
};

/// A test-only inference backend that counts its calls and reports usage.
struct CountingBackend {
    calls: Arc<AtomicUsize>,
    prefix: String,
}

impl InferenceBackend for CountingBackend {
    fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(InferenceResponse {
            text: format!("{} {} chars", self.prefix, request.input.chars().count()),
            usage: Usage {
                tokens_in: 11,
                tokens_out: 3,
                cost: trusty_summarizer::Cost::Provider(0.25),
                ..Usage::default()
            },
            latency: Duration::from_millis(1),
        })
    }
}

/// A test-only classifier that prefers the label named in the text.
struct KeywordClassifier;

impl ClassifierBackend for KeywordClassifier {
    fn classify(&self, texts: &[&str], labels: &[Label]) -> Result<ClassifyResponse> {
        let scores = texts
            .iter()
            .map(|t| {
                labels
                    .iter()
                    .map(|l| if t.contains(&l.name) { 0.9 } else { 0.1 })
                    .collect()
            })
            .collect();
        Ok(ClassifyResponse {
            scores,
            usage: Usage::default(),
        })
    }
}

fn registry(calls: &Arc<AtomicUsize>) -> BackendRegistry {
    let mut r = BackendRegistry::with_builtins();
    let calls = Arc::clone(calls);
    r.register_inference("test-counting", move |opts| {
        let prefix = opts
            .get("prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("TEST")
            .to_string();
        Ok(Arc::new(CountingBackend {
            calls: Arc::clone(&calls),
            prefix,
        }) as Arc<dyn InferenceBackend>)
    })
    .unwrap();
    r.register_classifier("test-keyword", |_| {
        Ok(Arc::new(KeywordClassifier) as Arc<dyn ClassifierBackend>)
    })
    .unwrap();
    r
}

fn config(cache: &Path, extra: &str) -> Config {
    let mut c = Config::from_toml(&format!(
        r#"
        concurrency = 2
        [summary]
        backend = "test-counting"
        [classifier]
        backend = "test-keyword"
        [[classifier.labels]]
        name = "invoice"
        [[classifier.labels]]
        name = "recipe"
        {extra}
        "#
    ))
    .unwrap();
    c.cache.dir = Some(cache.to_path_buf());
    c
}

fn ok(record: FileRecord) -> SummaryRecord {
    match record {
        FileRecord::Ok(r) => r,
        FileRecord::Error(e) => panic!("unexpected error record: {e:?}"),
    }
}

fn ran(record: &SummaryRecord, stage: Stage) -> bool {
    record.stages.iter().any(|s| s.stage == stage)
}

#[test]
fn test_backend_selected_by_name_without_caller_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "a recipe for bread\n\nmix and bake\n").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    // The same calling code as the CLI: Summarizer::new + summarize_file.
    let s = Summarizer::new(
        config(
            &dir.path().join("cache"),
            "[backends.test-counting]\nprefix = \"CUSTOM\"",
        ),
        &registry(&calls),
        Converters::with_builtins(),
    )
    .unwrap();
    let r = ok(s.summarize_file(&file));

    assert_eq!(r.summary, "CUSTOM 33 chars");
    assert_eq!(
        r.class,
        ClassResult::Classified {
            label: "recipe".into(),
            score: 0.9,
            runner_up: Some(LabelScore {
                label: "invoice".into(),
                score: 0.1
            }),
        }
    );
    assert_eq!(r.backends.summary, "test-counting");
    assert_eq!(r.backends.classifier, "test-keyword");
    let summarize = r
        .stages
        .iter()
        .find(|s| s.stage == Stage::Summarize)
        .unwrap();
    assert_eq!((summarize.tokens_in, summarize.tokens_out), (11, 3));
    assert_eq!(summarize.cost_usd, Some(0.25));
    assert_eq!(summarize.cost_source, "provider");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn builtin_defaults_report_no_classifier() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.md");
    std::fs::write(
        &file,
        "# Notes\n\nThe default summary uses no model at all.\n",
    )
    .unwrap();
    let mut c = Config::default();
    c.cache.enabled = false;
    let s = Summarizer::new(
        c,
        &BackendRegistry::with_builtins(),
        Converters::with_builtins(),
    )
    .unwrap();
    let r = ok(s.summarize_file(&file));
    assert_eq!(r.class, ClassResult::NoClassifierConfigured);
    assert_eq!(r.summary, "The default summary uses no model at all.");
    assert_eq!(r.cache, CacheStatus::Disabled);
    assert_eq!(r.backends.converter, "direct");
    let json = serde_json::to_value(FileRecord::Ok(r)).unwrap();
    assert_eq!(json["class"]["status"], "no_classifier_configured");
    assert_eq!(json["outline"][0]["lines"], serde_json::json!([1, 3]));
}

#[test]
fn second_run_is_served_from_cache() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "an invoice for services\n").unwrap();
    let cache = dir.path().join("cache");
    let calls = Arc::new(AtomicUsize::new(0));

    let first = {
        let s = Summarizer::new(
            config(&cache, ""),
            &registry(&calls),
            Converters::with_builtins(),
        )
        .unwrap();
        ok(s.summarize_file(&file))
    };
    assert_eq!(first.cache, CacheStatus::Miss);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A fresh Summarizer, as a second CLI run would build.
    let s = Summarizer::new(
        config(&cache, ""),
        &registry(&calls),
        Converters::with_builtins(),
    )
    .unwrap();
    let second = ok(s.summarize_file(&file));
    assert_eq!(second.cache, CacheStatus::Hit);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "backend ran again on a hit"
    );
    assert!(!ran(&second, Stage::Convert) && !ran(&second, Stage::Summarize));
    assert_eq!(
        (&second.summary, &second.outline),
        (&first.summary, &first.outline)
    );
    assert_eq!(second.hash, first.hash);
}

#[test]
fn changed_file_or_setting_recomputes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "an invoice for services\n").unwrap();
    let cache = dir.path().join("cache");
    let calls = Arc::new(AtomicUsize::new(0));
    let run = |extra: &str| {
        let s = Summarizer::new(
            config(&cache, extra),
            &registry(&calls),
            Converters::with_builtins(),
        )
        .unwrap();
        ok(s.summarize_file(&file))
    };

    assert_eq!(run("").cache, CacheStatus::Miss);
    assert_eq!(run("").cache, CacheStatus::Hit);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    std::fs::write(&file, "an invoice for more services\n").unwrap();
    assert_eq!(
        run("").cache,
        CacheStatus::Miss,
        "changed content must recompute"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let changed = run("[backends.test-counting]\nprefix = \"OTHER\"");
    assert_eq!(
        changed.cache,
        CacheStatus::Miss,
        "changed backend option must recompute"
    );
    assert_eq!(changed.summary, "OTHER 29 chars");
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let mut c = config(&cache, "");
    c.references = false;
    let s = Summarizer::new(c, &registry(&calls), Converters::with_builtins()).unwrap();
    let no_refs = ok(s.summarize_file(&file));
    assert_eq!(
        no_refs.cache,
        CacheStatus::Miss,
        "changed references must recompute"
    );
    assert!(
        no_refs
            .outline
            .iter()
            .all(|o| o.lines.is_none() && o.paragraphs.is_none())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// A test-only converter for a made-up `.fake` document format.
struct UpperConverter;

impl Converter for UpperConverter {
    fn name(&self) -> &str {
        "upper-test"
    }

    fn convert(&self, input: &ConvertInput<'_>) -> Result<String> {
        Ok(String::from_utf8_lossy(input.bytes).to_uppercase())
    }
}

#[test]
fn registered_converter_handles_its_format() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("doc.pdf");
    std::fs::write(&file, "%PDF pretend text\nsecond line\n").unwrap();
    let mut c = Config::default();
    c.cache.enabled = false;

    let plain = Summarizer::new(
        c.clone(),
        &BackendRegistry::with_builtins(),
        Converters::with_builtins(),
    )
    .unwrap();
    let FileRecord::Error(e) = plain.summarize_file(&file) else {
        panic!("pdf without a converter must fail");
    };
    assert_eq!(e.error.kind, "unsupported_format");
    assert_eq!(e.error.format.as_deref(), Some("pdf"));

    let mut converters = Converters::with_builtins();
    converters.register("pdf", Arc::new(UpperConverter));
    let s = Summarizer::new(c, &BackendRegistry::with_builtins(), converters.clone()).unwrap();
    let r = ok(s.summarize_file(&file));
    assert_eq!(r.backends.converter, "upper-test");
    assert_eq!(r.lines, 2);
    assert_eq!(
        converters.load_text(&file, 1 << 20).unwrap(),
        "%PDF PRETEND TEXT\nSECOND LINE\n"
    );
}

/// B4: a converted document's outline titles skip footers and bare values.
#[test]
fn converted_outline_titles_skip_boilerplate_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("deck.pdf");
    std::fs::write(
        &file,
        "$2.58\n(c) 2025 snowflake\ncredit pricing\n\n14%\n\n$105,000\n",
    )
    .unwrap();
    let mut c = Config::default();
    c.cache.enabled = false;
    let mut converters = Converters::with_builtins();
    converters.register("pdf", Arc::new(UpperConverter));
    let s = Summarizer::new(c, &BackendRegistry::with_builtins(), converters).unwrap();
    let r = ok(s.summarize_file(&file));
    assert_eq!(r.outline[0].title, "CREDIT PRICING");
}

/// A test-only converter that opts into the converted-text cache and counts
/// its conversions.
struct CountingTextConverter {
    calls: Arc<AtomicUsize>,
}

impl Converter for CountingTextConverter {
    fn name(&self) -> &str {
        "counting-text-test"
    }

    fn convert(&self, input: &ConvertInput<'_>) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(String::from_utf8_lossy(input.bytes).to_uppercase())
    }

    fn caches_text(&self) -> bool {
        true
    }
}

/// #2: converted text stored by a schema-5 build (key settings
/// `"converted-text"`, no schema) is not served; the pipeline converts again.
#[test]
fn schema_5_converted_text_is_not_served() {
    use trusty_summarizer::cache::{Cache, content_hash};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("doc.pdf");
    let bytes = "%PDF quarterly report\nrevenue grew\n";
    std::fs::write(&file, bytes).unwrap();
    let cache_dir = dir.path().join("cache");
    let calls = Arc::new(AtomicUsize::new(0));
    let converter = CountingTextConverter {
        calls: Arc::clone(&calls),
    };

    // The entry exactly as a schema-5 build wrote it.
    let stale = Cache::open(&cache_dir).unwrap();
    let key = Cache::key(
        &content_hash(bytes.as_bytes()),
        converter.name(),
        "converted-text",
    );
    stale.put_text(&key, "STALE SCHEMA FIVE TEXT\n").unwrap();
    assert_eq!(
        stale.get_text(&key).as_deref(),
        Some("STALE SCHEMA FIVE TEXT\n")
    );

    let mut converters = Converters::with_builtins();
    converters.register("pdf", Arc::new(converter));
    let mut c = Config::default();
    c.cache.dir = Some(cache_dir);
    let s = Summarizer::new(c, &BackendRegistry::with_builtins(), converters).unwrap();
    let r = ok(s.summarize_file(&file));

    assert_eq!(calls.load(Ordering::SeqCst), 1, "schema-5 text was served");
    assert!(ran(&r, Stage::Convert));
    assert!(
        r.outline.iter().all(|o| !o.title.contains("STALE")),
        "outline built from stale text: {:?}",
        r.outline
    );
}

#[test]
fn batch_continues_past_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("in");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.md"), "# A\n\nFirst file text here.\n").unwrap();
    std::fs::write(root.join("b.pdf"), "%PDF").unwrap();
    std::fs::write(root.join("c.txt"), b"bad \xff utf8").unwrap();
    std::fs::write(root.join("sub/d.rs"), "fn main() {}\n").unwrap();
    let missing = dir.path().join("missing.txt");
    let mut c = Config::default();
    c.cache.dir = Some(dir.path().join("cache"));
    c.concurrency = Some(3);
    let s = Summarizer::new(
        c,
        &BackendRegistry::with_builtins(),
        Converters::with_builtins(),
    )
    .unwrap();

    let mut got: Vec<(String, String)> = Vec::new();
    let stats = s
        .summarize_batch(&[root.clone(), missing.clone()], |r| {
            let status = match &r {
                FileRecord::Ok(_) => "ok".to_string(),
                FileRecord::Error(e) => e.error.kind.clone(),
            };
            let path = PathBuf::from(r.path());
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            got.push((name, status));
            Ok(())
        })
        .unwrap();

    let want: Vec<(String, String)> = [
        ("a.md", "ok"),
        ("b.pdf", "unsupported_format"),
        ("c.txt", "invalid_text"),
        ("d.rs", "ok"),
        ("missing.txt", "io"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(got, want);
    assert_eq!((stats.files, stats.errors, stats.cache_hits), (5, 3, 0));
}

/// Step 7: the described outline holds its line bound on a long document
/// and names what each range holds. Ruling Dnb: the default outline is the
/// plain one, and an entry written with the described outline is never
/// served under the default (nor the default's under the described one).
#[test]
fn described_outline_is_bounded_named_and_cached_apart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("log.txt");
    // 600 lines with no blank line: one paragraph, as many PDFs convert.
    let mut text = String::new();
    for i in 0..600 {
        let place = ["Alder Annex", "Birch Hall", "Cedar Depot"][i / 200];
        if i % 7 == 3 {
            text.push_str(&format!("note: the {place} roof was checked\n"));
        } else {
            text.push_str(&format!("entry {i} checked by staff\n"));
        }
    }
    std::fs::write(&file, &text).unwrap();
    let cache = dir.path().join("cache");
    let calls = Arc::new(AtomicUsize::new(0));
    let build = |extra: &str| {
        Summarizer::new(
            config(&cache, extra),
            &registry(&calls),
            Converters::with_builtins(),
        )
        .unwrap()
    };

    let described = ok(build("[outline]\ndescribe = true\n").summarize_file(&file));
    assert_eq!(described.cache, CacheStatus::Miss);
    assert_eq!(described.outline.len(), 10);
    for e in &described.outline {
        let lines = e.lines.unwrap();
        assert!(lines.end - lines.start < 60, "{e:?}");
    }
    let first = &described.outline[0];
    assert!(first.terms.iter().any(|t| t == "Alder Annex"), "{first:?}");
    let last = described.outline.last().unwrap();
    assert!(last.terms.iter().any(|t| t == "Cedar Depot"), "{last:?}");

    // No [outline] table: the default.
    let default = ok(build("").summarize_file(&file));
    assert_eq!(default.cache, CacheStatus::Miss, "described entry served");
    assert_eq!(default.outline.len(), 1, "one paragraph, one plain entry");
    assert!(default.outline.iter().all(|e| e.terms.is_empty()));

    let plain = ok(build("[outline]\ndescribe = false\n").summarize_file(&file));
    assert_eq!(
        plain.cache,
        CacheStatus::Hit,
        "default is the plain outline"
    );
    assert_eq!(plain.outline, default.outline);
    let again = ok(build("[outline]\ndescribe = true\n").summarize_file(&file));
    assert_eq!(again.cache, CacheStatus::Hit);
    assert_eq!(again.outline, described.outline);
}
