//! #9603: a config parse failure must never echo the offending value.
//!
//! Why: `serde_yaml`'s error text quotes the scalar it could not read, and
//! trusty-mpm's `log_drain.secrets` holds plaintext site tokens, so a type error
//! there wrote the token to the warn log on every config load.
//! What: drives [`load_at`] and [`load_or_default_at`] against files whose bad
//! scalar is a unique sentinel, and asserts the sentinel reaches neither the
//! captured `tracing` output nor the error's `Display`/`Debug`, while the key
//! path an operator needs to find the field still does.
//! Test: this module.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::*;

/// Schema shaped like trusty-mpm's `TrustyToolsConfig` secret-bearing section.
#[derive(Debug, Default, PartialEq, Deserialize)]
struct SecretConfig {
    #[serde(default)]
    log_drain: Option<LogDrain>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    port: Option<u8>,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
struct LogDrain {
    #[serde(default)]
    secrets: Vec<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    On,
    Off,
}

/// A `tracing` writer that appends every formatted event to a shared buffer.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `f` under a thread-local TRACE subscriber; return its result and the log text.
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    let sink = Sink::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&sink.0.lock().expect("sink lock")).into_owned();
    (out, text)
}

/// A per-run token that cannot occur in any message by accident.
fn sentinel_digits() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("9603{:05}{nanos:09}", std::process::id() % 100_000)
}

fn sentinel() -> String {
    format!("SENTINEL-9603-{}", sentinel_digits())
}

/// Write `body` as the trusty-mpm config under a fresh temp home.
fn write_config(body: &str) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::TempDir::new().expect("temp home");
    let path = crate_config_path_at(home.path(), "trusty-mpm");
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir");
    std::fs::write(&path, body).expect("write config");
    (home, path)
}

/// Why: the #9603 defect itself — a string where `log_drain.secrets` expects a
/// list put the token in the warn log on every load.
/// What: loads through the warn-and-default path with logs captured, then
/// reads the strict error; the sentinel must be absent from all three outputs
/// while the key path is present in all three and the line in the two texts.
/// Test: itself.
#[test]
fn a_type_error_on_a_secret_field_never_reaches_the_log_9603() {
    let secret = sentinel();
    let (_home, path) = write_config(&format!("log_drain:\n  secrets: {secret}\n"));

    let (cfg, logs) = capture_logs(|| load_or_default_at::<SecretConfig>(&path, "trusty-mpm"));
    let err = load_at::<SecretConfig>(&path).expect_err("a type error must not parse");
    let display = err.to_string();
    let debug = format!("{err:?}");

    assert_eq!(cfg, SecretConfig::default(), "the fallback is the default");
    assert!(
        !logs.contains(&secret),
        "the warn log leaked the value: {logs}"
    );
    assert!(
        !display.contains(&secret),
        "Display leaked the value: {display}"
    );
    assert!(!debug.contains(&secret), "Debug leaked the value: {debug}");
    for (name, text) in [("log", &logs), ("Display", &display), ("Debug", &debug)] {
        assert!(
            text.contains("log_drain.secrets"),
            "{name} lost the key path: {text}"
        );
    }
    for (name, text) in [("log", &logs), ("Display", &display)] {
        assert!(text.contains("line 2"), "{name} lost the line: {text}");
    }
}

/// Why: the redaction must hold for every serde failure shape that quotes
/// input, not only `invalid type` — an enum variant, an out-of-range integer,
/// and a YAML syntax error each format the input differently.
/// What: one row per shape; each asserts the strict error's `Display` and
/// `Debug` omit the sentinel and, where serde knows one, keep the key path.
/// Test: itself.
#[test]
fn no_parse_failure_shape_echoes_the_offending_value_9603() {
    let word = sentinel();
    let digits = sentinel_digits();
    let rows: [(&str, String, &str, Option<&str>); 4] = [
        (
            "invalid type",
            format!("log_drain:\n  secrets: {word}\n"),
            &word,
            Some("log_drain.secrets"),
        ),
        (
            "unknown variant",
            format!("mode: {word}\n"),
            &word,
            Some("mode"),
        ),
        (
            "invalid value",
            format!("port: {digits}\n"),
            &digits,
            Some("port"),
        ),
        (
            "syntax",
            format!("log_drain:\n  secrets: [{word}\n"),
            &word,
            None,
        ),
    ];
    for (shape, body, secret, key_path) in rows {
        let (_home, path) = write_config(&body);
        let err = load_at::<SecretConfig>(&path).expect_err(shape);
        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(
            !display.contains(secret),
            "{shape}: Display leaked the value: {display}"
        );
        assert!(
            !debug.contains(secret),
            "{shape}: Debug leaked the value: {debug}"
        );
        if let Some(key_path) = key_path {
            assert!(
                display.contains(key_path),
                "{shape}: Display lost the key path: {display}"
            );
        }
    }
}

/// Why: the redaction must not change the failure branch — a malformed file
/// still downgrades to `Default` with one warn, and a valid file still parses
/// with no warn (Fail-Open Check: the downgrade's semantics are unchanged).
/// What: one malformed and one valid load through [`load_or_default_at`], logs
/// captured; asserts the returned value and the WARN line count for each.
/// Test: itself.
#[test]
fn a_parse_failure_falls_back_to_default_and_warns_once_9603() {
    let (_home, bad) = write_config("port: not-a-port\n");
    let (cfg, logs) = capture_logs(|| load_or_default_at::<SecretConfig>(&bad, "trusty-mpm"));
    assert_eq!(cfg, SecretConfig::default());
    assert_eq!(
        logs.matches("WARN").count(),
        1,
        "one warn per failed load: {logs}"
    );
    assert!(
        logs.contains(&bad.display().to_string()),
        "the warn names the file: {logs}"
    );
    assert!(
        logs.contains("falling back to default trusty-mpm config"),
        "{logs}"
    );

    let (_home2, good) = write_config("port: 7\nmode: on\n");
    let (cfg, logs) = capture_logs(|| load_or_default_at::<SecretConfig>(&good, "trusty-mpm"));
    assert_eq!(cfg.port, Some(7));
    assert_eq!(cfg.mode, Some(Mode::On));
    assert!(!logs.contains("WARN"), "a valid file logs no warn: {logs}");
}
