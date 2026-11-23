//! Tests for the OpenAI-compatible client against a local HTTP server that
//! runs inside the test process. No network, no Ollama.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::*;
use crate::backend::Credentials;

/// One captured request: the header block and the body.
struct Seen {
    head: String,
    body: String,
}

/// Serves one connection with `status` and `body`, after `delay`, and
/// sends the request it read back through the returned channel.
fn serve_once(status: u16, body: &'static str, delay: Duration) -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            head.push_str(&line);
        }
        let mut buf = vec![0; len];
        reader.read_exact(&mut buf).unwrap();
        let _ = tx.send(Seen {
            head,
            body: String::from_utf8(buf).unwrap(),
        });
        thread::sleep(delay);
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (url, rx)
}

fn backend(url: &str, extra: &str, env: EnvLookup) -> OpenAiBackend {
    let opts: toml::Table = toml::from_str(&format!(
        "type = \"openai\"\nbase_url = \"{url}\"\nmodel = \"m1\"\n{extra}"
    ))
    .unwrap();
    OpenAiBackend::with_env_lookup("ep", &opts, env).unwrap()
}

fn no_env() -> EnvLookup {
    Arc::new(|_| None)
}

fn request() -> InferenceRequest {
    InferenceRequest {
        instruction: "Summarize.".into(),
        input: "Some document text.".into(),
        max_output_tokens: 50,
        credentials: Credentials::process_env(),
    }
}

const OK: &str = r#"{"choices":[{"message":{"role":"assistant","content":"<think>hmm</think>\nA short summary."},"finish_reason":"stop"}],"usage":{"prompt_tokens":42,"completion_tokens":7,"cost":0.0012}}"#;

#[test]
fn posts_chat_completion_and_reads_usage() {
    let (url, seen) = serve_once(200, OK, Duration::ZERO);
    let env: EnvLookup = Arc::new(|k| (k == "TEST_KEY_VAR").then(|| "sk-test-123456".into()));
    let b = backend(
        &url,
        "api_key_env = \"TEST_KEY_VAR\"\n[request]\nreasoning_effort = \"none\"",
        env,
    );
    let r = b.complete(&request()).unwrap();
    assert_eq!(r.text, "A short summary.");
    assert_eq!(
        r.usage,
        Usage {
            tokens_in: 42,
            tokens_out: 7,
            cost: Cost::Provider(0.0012),
            ..Usage::default()
        }
    );
    let seen = seen.recv().unwrap();
    assert!(
        seen.head.starts_with("POST /v1/chat/completions "),
        "{}",
        seen.head
    );
    assert!(seen.head.contains("Bearer sk-test-123456"));
    let body: Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["model"], "m1");
    assert_eq!(body["max_tokens"], 50);
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(body["messages"][0]["content"], "Summarize.");
    assert_eq!(body["messages"][1]["content"], "Some document text.");
}

#[test]
fn endpoint_without_cost_or_prices_reports_unknown() {
    let body = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
    let (url, seen) = serve_once(200, body, Duration::ZERO);
    let r = backend(&url, "", no_env()).complete(&request()).unwrap();
    assert_eq!((r.usage.tokens_in, r.usage.tokens_out), (3, 1));
    assert_eq!(r.usage.cost, Cost::Unknown);
    assert_eq!(r.usage.cost.usd(), None);
    assert!(
        !seen
            .recv()
            .unwrap()
            .head
            .to_ascii_lowercase()
            .contains("authorization")
    );
}

#[test]
fn failures_name_their_cause() {
    // Unreachable: a port that was just released.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = backend(&format!("http://127.0.0.1:{port}/v1"), "", no_env())
        .complete(&request())
        .unwrap_err();
    assert_eq!(err.kind(), "backend_failed");
    assert!(err.to_string().contains("cannot reach"), "{err}");

    let (url, _seen) = serve_once(500, r#"{"error":"model not found"}"#, Duration::ZERO);
    let err = backend(&url, "", no_env())
        .complete(&request())
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 500"), "{err}");
    assert!(err.to_string().contains("model not found"), "{err}");

    let (url, _seen) = serve_once(200, "not json at all", Duration::ZERO);
    let err = backend(&url, "", no_env())
        .complete(&request())
        .unwrap_err();
    assert!(err.to_string().contains("malformed response"), "{err}");

    let (url, _seen) = serve_once(200, r#"{"choices":[]}"#, Duration::ZERO);
    let err = backend(&url, "", no_env())
        .complete(&request())
        .unwrap_err();
    assert!(err.to_string().contains("no choices[0]"), "{err}");

    let (url, _seen) = serve_once(200, OK, Duration::from_millis(1500));
    let err = backend(&url, "timeout_ms = 200", no_env())
        .complete(&request())
        .unwrap_err();
    assert!(err.to_string().contains("timed out after 200 ms"), "{err}");

    let err = backend(
        "http://127.0.0.1:9/v1",
        "api_key_env = \"ABSENT_VAR\"",
        no_env(),
    )
    .complete(&request())
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("environment variable ABSENT_VAR is not set"),
        "{err}"
    );
}

#[test]
fn key_value_never_appears_in_errors() {
    // A server that echoes the key back in its error body.
    let (url, _seen) = serve_once(
        401,
        r#"{"error":"bad key Bearer sk-secret-abcdef123"}"#,
        Duration::ZERO,
    );
    let env: EnvLookup = Arc::new(|_| Some("sk-secret-abcdef123".into()));
    let b = backend(&url, "api_key_env = \"MY_KEY\"", env);
    let err = b.complete(&request()).unwrap_err();
    let text = format!("{err} {err:?} {b:?}");
    assert!(!text.contains("sk-secret-abcdef123"), "{text}");
    assert!(text.contains("HTTP 401"), "{text}");
}

#[test]
fn options_are_validated_without_echoing_secrets() {
    let parse = |s: &str| -> Result<OpenAiBackend> {
        OpenAiBackend::with_env_lookup("ep", &toml::from_str(s).unwrap(), no_env())
    };
    assert!(parse("model = \"m\"").is_err());
    assert!(parse("base_url = \"ftp://x\"\nmodel = \"m\"").is_err());
    let err =
        parse("base_url = \"http://x\"\nmodel = \"m\"\napi_key_env = \"sk-live-zzz\"").unwrap_err();
    assert!(!err.to_string().contains("sk-live-zzz"), "{err}");
    let err =
        parse("base_url = \"http://x\"\nmodel = \"m\"\napi_key = \"sk-live-zzz\"").unwrap_err();
    assert!(!err.to_string().contains("sk-live-zzz"), "{err}");
    assert!(parse("base_url = \"http://x\"\nmodel = \"m\"\n[request]\nmodel = \"y\"").is_err());
    assert!(parse("base_url = \"http://x/\"\nmodel = \"m\"\ntimeout_ms = 5").is_ok());
}

/// The Atlas Cloud response seen live: content empty, reasoning in a
/// separate field, the whole budget spent on it.
const REASONING_EXHAUSTED: &str = r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"Let me think about the document..."},"finish_reason":"length"}],"usage":{"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":102},"prompt_tokens":294,"prompt_tokens_details":{"cached_tokens":0},"total_tokens":394}}"#;

#[test]
fn empty_and_reasoning_exhausted_answers_fail_with_cause() {
    // Through the HTTP path: a backend_failed error, never an empty success.
    let (url, _seen) = serve_once(200, REASONING_EXHAUSTED, Duration::ZERO);
    let err = backend(&url, "", no_env())
        .complete(&request())
        .unwrap_err();
    assert_eq!(err.kind(), "backend_failed");
    let msg = err.to_string();
    assert!(msg.contains("output budget spent on reasoning"), "{msg}");
    assert!(msg.contains("102 reasoning tokens"), "{msg}");

    for (body, want) in [
        (
            r#"{"choices":[{"message":{"content":"   \n\t "},"finish_reason":"stop"}]}"#,
            "empty answer (finish_reason `stop`",
        ),
        (
            r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#,
            "empty answer (finish_reason `stop`",
        ),
        (
            r#"{"choices":[{"message":{"content":"<think>long"},"finish_reason":"length"}]}"#,
            "output budget spent on reasoning",
        ),
        (
            r#"{"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{"completion_tokens":5}}"#,
            "empty answer (finish_reason `length`, 5 completion tokens)",
        ),
    ] {
        let e = parse_completion(body).unwrap_err();
        assert!(e.contains(want), "{body}: {e}");
    }
}

#[test]
fn cost_comes_from_provider_or_prices_or_is_unknown() {
    let with_details = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":2000000,"completion_tokens":1000000,"completion_tokens_details":{"reasoning_tokens":400},"prompt_tokens_details":{"cached_tokens":1500}}}"#;
    let prices = "price_in_per_mtok = 0.26\nprice_out_per_mtok = 0.38";

    // Configured prices: 2 M in x 0.26 + 1 M out x 0.38.
    let (url, _seen) = serve_once(200, with_details, Duration::ZERO);
    let r = backend(&url, prices, no_env())
        .complete(&request())
        .unwrap();
    let Cost::ConfiguredPrices(c) = r.usage.cost else {
        panic!("{:?}", r.usage.cost)
    };
    assert!((c - 0.90).abs() < 1e-9, "{c}");
    assert_eq!(r.usage.reasoning_tokens, Some(400));
    assert_eq!(r.usage.cached_tokens_in, Some(1500));

    // The provider's cost wins over configured prices.
    let (url, _seen) = serve_once(200, OK, Duration::ZERO);
    let r = backend(&url, prices, no_env())
        .complete(&request())
        .unwrap();
    assert_eq!(r.usage.cost, Cost::Provider(0.0012));
    assert_eq!(
        (r.usage.reasoning_tokens, r.usage.cached_tokens_in),
        (None, None)
    );

    // Neither: unknown, not zero.
    let (url, _seen) = serve_once(200, with_details, Duration::ZERO);
    let r = backend(&url, "", no_env()).complete(&request()).unwrap();
    assert_eq!(r.usage.cost, Cost::Unknown);

    // Half a price pair is a config error.
    let opts: toml::Table =
        toml::from_str("base_url = \"http://x\"\nmodel = \"m\"\nprice_in_per_mtok = 1").unwrap();
    assert!(OpenAiBackend::with_env_lookup("ep", &opts, no_env()).is_err());
}

#[test]
fn backend_budget_and_scoped_keys_reach_the_request() {
    let (url, seen) = serve_once(200, OK, Duration::ZERO);
    // The process env has a different key; scoped credentials replace it.
    let env: EnvLookup = Arc::new(|_| Some("sk-process-env".into()));
    let b = backend(
        &url,
        "api_key_env = \"K_VAR\"\nmax_output_tokens = 900",
        env,
    );
    let mut req = request();
    req.credentials = Credentials::scoped([("K_VAR".into(), "sk-from-client".into())].into());
    b.complete(&req).unwrap();
    let seen = seen.recv().unwrap();
    assert!(seen.head.contains("Bearer sk-from-client"), "{}", seen.head);
    let body: Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["max_tokens"], 900);

    // Scoped credentials without the variable: missing, process env ignored.
    req.credentials = Credentials::scoped(Default::default());
    let err = b.complete(&req).unwrap_err();
    assert!(err.to_string().contains("K_VAR is not set"), "{err}");
}
