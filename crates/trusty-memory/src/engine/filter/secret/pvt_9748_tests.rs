//! The secret gate admits GitHub Projects v2 node ids, keeps every look-alike's
//! verdict, and reports a refusal by offset and rule only (issue #9748).
//!
//! Why: `memory_remember` and `memory_note` refused content naming a project
//! (`PVT_…`) or project item (`PVTI_…`), and the refusal quoted the first four
//! characters and the length of the token it flagged.
//! What: drives the gate (`check_secret`, `FilterConfig::apply`,
//! `find_secret_spans`) only. Every fixture is synthetic. Wired in from
//! `secret.rs`; #9278 made the gate crate-only, so the file moved here from
//! `tests/secret_gate_pvt_9748.rs`.
//! Test: this is the test file.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use super::{check_secret, find_secret_spans};
use crate::engine::filter::FilterConfig;

/// Synthetic ids of the measured shapes: `PVT_` + 16, `PVTI_` + 23, with each
/// owner-kind header, and with `-`/`_` inside the tail.
const NODE_IDS: [&str; 4] = [
    "PVT_kwDOAbCdEf12GhIj",         // pragma: allowlist secret
    "PVT_kwHOZz9-Yy8_Xx7W",         // pragma: allowlist secret
    "PVTI_lADOAbCdEf12GhIjKlMnOp3", // pragma: allowlist secret
    "PVTI_lAHOZz9-Yy8_Xx7WvU6tS5r", // pragma: allowlist secret
];

/// A synthetic mixed-case credential, 24 characters.
const MIXED: &str = "AbCd1234EfGh5678IjKl9012"; // pragma: allowlist secret

/// True when `haystack` carries any 4-character window of `value`.
fn quotes_part_of(haystack: &str, value: &str) -> bool {
    value
        .as_bytes()
        .windows(4)
        .filter_map(|w| std::str::from_utf8(w).ok())
        .any(|w| haystack.contains(w))
}

/// Why: the acceptance half of #9748 — a body naming a project or project item
/// id is stored, alone, in prose, after `key=`, in JSON and in inline code.
/// What: asserts the fixture lengths, then that `check_secret`, the full
/// `apply` gate and `find_secret_spans` all admit every shape.
/// Test: itself; red on the pre-fix crate, which refuses every id.
#[test]
fn github_project_node_ids_are_accepted_after_9748() {
    for id in NODE_IDS {
        let (prefix, tail) = id.split_at(id.find('_').map_or(0, |i| i + 1));
        let want = if prefix == "PVT_" { 16 } else { 23 };
        assert_eq!(tail.len(), want, "fixture {id} has the wrong tail length");
        for content in [
            id.to_string(),
            format!("Moved the card {id} to In Review after the merge."),
            format!("Recorded project_id={id} as the tracker state."),
            format!("The query returned {{\"id\": \"{id}\"}} for the board."),
            format!("Status lives on `{id}` until the release ships."),
        ] {
            assert_eq!(check_secret(&content), Ok(()), "refused: {content}");
            assert_eq!(
                FilterConfig::default().apply(&content, false),
                Ok(()),
                "apply refused: {content}"
            );
            assert!(find_secret_spans(&content).is_empty(), "span in: {content}");
        }
    }
}

/// Why: the exemption must be exactly the measured shapes. A wrong length,
/// casing, header or charset, a glued prefix or suffix, and every other shape
/// named in #9748 keeps the verdict it had before the fix.
/// What: one row per look-alike with its pre-fix verdict (`true` = refused),
/// observed by running this table against the pre-fix crate.
/// Test: itself; shown red by widening the exemption's length to a window.
#[test]
fn github_project_node_id_look_alikes_keep_their_verdict_after_9748() {
    let rows: &[(&str, bool)] = &[
        // Tail one short, one long, and the 20 the issue first guessed. One
        // short is 19 characters in all, under the 20-character floor.
        ("PVT_kwDOAbCdEf12GhI", false),     // pragma: allowlist secret
        ("PVT_kwDOAbCdEf12GhIjK", true),    // pragma: allowlist secret
        ("PVT_kwDOAbCdEf12GhIjKlMn", true), // pragma: allowlist secret
        ("PVTI_lADOAbCdEf12GhIjKlMnOp", true), // pragma: allowlist secret
        ("PVTI_lADOAbCdEf12GhIjKlMnOp3q", true), // pragma: allowlist secret
        ("PVTI_lADOAbCdEf12GhIjKlMn", true), // pragma: allowlist secret
        // Prefix casing.
        ("pvt_kwDOAbCdEf12GhIj", true), // pragma: allowlist secret
        ("Pvt_kwDOAbCdEf12GhIj", true), // pragma: allowlist secret
        ("pvti_lADOAbCdEf12GhIjKlMnOp3", true), // pragma: allowlist secret
        // Right length, wrong header; one kind's header on the other kind.
        ("PVT_AbCdEf12GhIjKlMn", true), // pragma: allowlist secret
        ("PVT_kwXOAbCdEf12GhIj", true), // pragma: allowlist secret
        ("PVT_lADOAbCdEf12GhIj", true), // pragma: allowlist secret
        ("PVTI_kwDOAbCdEf12GhIjKlMnOp3", true), // pragma: allowlist secret
        // A base64 symbol in the tail; text glued to either end.
        ("PVT_kwDOAbCd+f12GhIj", true),     // pragma: allowlist secret
        ("xPVT_kwDOAbCdEf12GhIj", true),    // pragma: allowlist secret
        ("PVT_kwDOAbCdEf12GhIj-old", true), // pragma: allowlist secret
        // A random 22-character mixed-case token.
        ("Qw3rTy9UiOp2AsDf4GhJkL", true), // pragma: allowlist secret
        // A filesystem path, a URL, and a 40-hex commit SHA.
        ("/srv/data/build-cache/release/notes.md", false),
        ("https://github.com/orgs/example/projects/45/views/2", false),
        ("0fda534e0fda534e0fda534e0fda534e0fda534e", false),
    ];
    let mut changed = Vec::new();
    // Provider keys, assembled at runtime so no source line holds a full
    // provider-shaped token (GitHub push protection reads source text).
    let provider_keys = [
        provider_fixture("gh", "p_", "abcdefghijklmnopqrstuvwxyz0123456789"),
        provider_fixture("s", "k-", "proj-AbCd1234EfGh5678IjKl9012MnOp"),
        provider_fixture("AK", "IA", "IOSFODNN7EXAMPLE"),
        provider_fixture("xo", "xb-", "1234567890-AbCdEfGhIjKlMnOp"),
    ];
    let rows = rows
        .iter()
        .map(|(token, refused)| (token.to_string(), *refused))
        .chain(provider_keys.into_iter().map(|token| (token, true)));
    for (token, refused) in rows {
        for content in [
            token.clone(),
            format!("Logged {token} in the handoff note."),
        ] {
            if check_secret(&content).is_err() != refused {
                changed.push(content);
            }
        }
    }
    assert!(changed.is_empty(), "verdict changed for: {changed:#?}");
}

/// Why: a refusal must locate the hit without quoting it — no characters of
/// the value and not its length, in the message or the `Debug` form.
/// What: refuses a mixed-case token behind three spaces of leading whitespace
/// through `apply` (the offset indexes the caller's content, not the trimmed
/// view) and a provider key through `check_secret`, then checks both renderings.
/// Test: itself; red on the pre-fix crate, whose message quoted `AbCd…(24 chars)`.
#[test]
fn potential_secret_error_names_offset_and_rule_only_after_9748() {
    let ghp = &provider_fixture("gh", "p_", "abcdefghijklmnopqrstuvwxyz0123456789");
    let applied = FilterConfig::default()
        .apply(&format!("   Rotate {MIXED} before the deploy."), false)
        .expect_err("a mixed-case credential is refused");
    let checked =
        check_secret(&format!("Pasted {ghp} by mistake.")).expect_err("a provider key is refused");
    for (err, offset, rule, value) in [
        (applied, 10, "mixed_case_alnum", MIXED),
        (checked, 7, "provider_prefix", ghp),
    ] {
        let msg = err.to_string();
        let dbg = format!("{err:?}");
        assert!(msg.contains(&format!("offset {offset}")), "{msg}");
        assert!(msg.contains(rule), "{msg}");
        for text in [&msg, &dbg] {
            assert!(!quotes_part_of(text, value), "quotes the value: {text}");
            assert!(!text.contains(&value.len().to_string()), "length: {text}");
            assert!(!text.contains("chars"), "preview: {text}");
        }
    }
}

/// Joins the parts of a provider-shaped fixture at runtime, so the source
/// never spells the full token (GitHub push protection scans source text).
fn provider_fixture(head: &str, mid: &str, tail: &str) -> String {
    format!("{head}{mid}{tail}")
}

/// A `tracing` writer that appends to a shared buffer.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buf {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A plain-text subscriber writing every event at WARN and above into `buf`.
fn subscriber(buf: &Buf) -> impl tracing::Subscriber + Send + Sync + use<> {
    let buf = buf.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || buf.clone())
        .finish()
}

/// Why: an operator must be able to see that and where the gate refused, from
/// the log alone, without the log carrying the value.
/// What: runs one refusal through `apply` and one accepted body under a
/// capturing subscriber, and asserts exactly one WARN line naming the offset
/// and rule, with no part of the value and not its length.
/// Test: itself; red on the pre-fix crate, which logged nothing.
#[test]
fn secret_refusal_logs_one_warn_with_offset_and_rule_after_9748() {
    let buf = Buf::default();
    // A callsite first reached on another test thread with no subscriber is
    // cached as disabled; a second live dispatcher plus a rebuild re-decides it.
    let _second = tracing::Dispatch::new(subscriber(&buf));
    tracing::subscriber::with_default(subscriber(&buf), || {
        tracing::callsite::rebuild_interest_cache();
        let refused = FilterConfig::default().apply(&format!("Rotate {MIXED} today."), false);
        assert!(refused.is_err(), "a mixed-case credential is refused");
        let accepted = FilterConfig::default().apply("Moved the card to review.", false);
        assert_eq!(accepted, Ok(()));
    });
    let bytes = buf.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let log = String::from_utf8_lossy(&bytes).into_owned();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one log line per refusal: {log}");
    let line = lines[0];
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("offset=7"), "{line}");
    assert!(line.contains("mixed_case_alnum"), "{line}");
    assert!(!quotes_part_of(line, MIXED), "quotes the value: {line}");
    assert!(!line.contains(&MIXED.len().to_string()), "length: {line}");
}
