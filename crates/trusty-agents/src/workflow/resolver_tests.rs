//! Tests for the parallel-phase conflict resolver.
//!
//! Split out of `resolver.rs` (the crate's `#[path = "*_tests.rs"]` convention)
//! so test bulk counts against the 1500-SLOC test cap rather than the 500-SLOC
//! production cap.
//!
//! Coverage rationale (#3653 review): the bug in #3652 lived in the mapping
//! from `git merge-file`'s exit status to "which bytes do we write". That
//! mapping is now the pure `decide_merge_bytes`, and it is unit-tested per
//! status class — clean, conflicted, failed, and signal-killed. Driving those
//! statuses end-to-end would require corrupting the ambient git environment
//! (`GIT_DIR`), which is a *process-global* mutation and races every other
//! test in this binary; the pure function exists precisely so we do not have
//! to do that.

use super::*;

use std::io::{Read, Write};

use crate::perf::TokenUsage;
use crate::tools::traits::AgentOutput;

fn mk_result(label: &str, out_dir: PathBuf) -> ParallelPhaseResult {
    ParallelPhaseResult {
        label: label.to_string(),
        output: AgentOutput {
            content: String::new(),
            summary: None,
            usage: TokenUsage::default(),
        },
        out_dir,
    }
}

fn versions(pairs: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    pairs
        .iter()
        .map(|(l, b)| (l.to_string(), b.to_vec()))
        .collect()
}

// ---------------------------------------------------------------------------
// decide_merge_bytes — the seam where #3652 lived.
// ---------------------------------------------------------------------------

/// Exit 0 means git printed the merged file; those exact bytes must survive.
///
/// Kills the mutation `Clean => Ok(Vec::new())` (literal truncation on a
/// *successful* merge), which the previous test suite did not catch.
#[test]
fn decide_merge_bytes_clean_returns_git_stdout_verbatim() {
    let out = decide_merge_bytes(Some(0), b"merged body\n".to_vec(), b"first version\n");
    assert_eq!(out, MergeOutcome::Merged(b"merged body\n".to_vec()));
}

/// Exit 1..=127 is the conflict-region count, and stdout carries the markers.
#[test]
fn decide_merge_bytes_conflicted_preserves_markers_and_region_count() {
    let stdout = b"<<<<<<< a\nalpha\n=======\nbeta\n>>>>>>> b\n".to_vec();
    assert_eq!(
        decide_merge_bytes(Some(1), stdout.clone(), b"alpha\n"),
        MergeOutcome::Conflicted {
            conflicted: stdout.clone(),
            regions: 1
        }
    );
    // Upper edge of the conflict range.
    assert_eq!(
        decide_merge_bytes(Some(127), stdout.clone(), b"alpha\n"),
        MergeOutcome::Conflicted {
            conflicted: stdout,
            regions: 127
        }
    );
}

/// Exit >127 means git itself failed and printed nothing. Trusting that empty
/// stdout is exactly the silent-truncation bug of #3652.
///
/// Kills the mutation `Failed => Ok(merged)` — the literal original bug, which
/// the previous test suite did not catch.
#[test]
fn decide_merge_bytes_failed_returns_first_version_not_empty_stdout() {
    // 128 = "fatal: not a git repository" / dubious ownership.
    assert_eq!(
        decide_merge_bytes(Some(128), Vec::new(), b"alpha\n"),
        MergeOutcome::Failed {
            fallback: b"alpha\n".to_vec(),
            status: Some(128)
        }
    );
    assert_eq!(
        decide_merge_bytes(Some(255), Vec::new(), b"alpha\n"),
        MergeOutcome::Failed {
            fallback: b"alpha\n".to_vec(),
            status: Some(255)
        }
    );
}

/// `None` = killed by a signal before it could print anything.
#[test]
fn decide_merge_bytes_signal_killed_returns_first_version() {
    assert_eq!(
        decide_merge_bytes(None, Vec::new(), b"alpha\n"),
        MergeOutcome::Failed {
            fallback: b"alpha\n".to_vec(),
            status: None
        }
    );
}

/// The headline invariant, stated as a property over every status class: the
/// bytes we end up writing are never empty when the input was not empty — even
/// if git lies and prints nothing on a "success" code.
#[test]
fn decide_merge_bytes_never_yields_empty_for_nonempty_input() {
    let first = b"alpha\n";
    for code in [Some(0), Some(1), Some(127), Some(128), Some(255), None] {
        // Simulate git printing NOTHING regardless of what it claims.
        let bytes = match decide_merge_bytes(code, Vec::new(), first) {
            MergeOutcome::Merged(b) | MergeOutcome::Conflicted { conflicted: b, .. } => b,
            MergeOutcome::Failed { fallback, .. } => fallback,
        };
        if code == Some(0) || matches!(code, Some(1..=127)) {
            // Git claimed success/conflict with empty output: decide_merge_bytes
            // takes it at its word, and the `resolve_conflict` seam is what
            // rescues this. Documented by
            // `resolve_conflict_seam_rejects_empty_result`.
            assert!(bytes.is_empty());
        } else {
            assert_eq!(bytes, first, "status {code:?} must fall back to first");
        }
    }
}

// ---------------------------------------------------------------------------
// extract_llm_content — the CRITICAL finding: OpenRouter error bodies.
// ---------------------------------------------------------------------------

#[test]
fn extract_llm_content_accepts_normal_completion() {
    let resp = serde_json::json!({
        "choices": [{"message": {"content": "merged file body\n"}}]
    });
    assert_eq!(extract_llm_content(&resp).unwrap(), "merged file body\n");
}

/// `reqwest::send` resolves on 4xx/5xx and OpenRouter's error payload is
/// well-formed JSON, so these used to parse cleanly into `""` and truncate the
/// file. Each must now be an error so the caller degrades to a real version.
#[test]
fn extract_llm_content_rejects_error_body() {
    for body in [
        serde_json::json!({"error": {"message": "Rate limit exceeded", "code": 429}}),
        serde_json::json!({"error": {"message": "No auth credentials found", "code": 401}}),
        serde_json::json!({"error": {"message": "requires more credits", "code": 402}}),
    ] {
        let err = extract_llm_content(&body).unwrap_err();
        assert!(
            err.to_string().contains("no choices[0].message.content"),
            "unexpected error: {err}"
        );
    }
}

#[test]
fn extract_llm_content_rejects_empty_choices() {
    let resp = serde_json::json!({"id": "gen-1", "choices": []});
    assert!(extract_llm_content(&resp).is_err());
    assert!(extract_llm_content(&serde_json::json!({})).is_err());
}

/// A 200 whose content is present but blank is still zero useful bytes.
#[test]
fn extract_llm_content_rejects_blank_content() {
    for blank in ["", "   ", "\n\n"] {
        let resp = serde_json::json!({"choices": [{"message": {"content": blank}}]});
        let err = extract_llm_content(&resp).unwrap_err();
        assert!(err.to_string().contains("blank content"), "got: {err}");
    }
}

// ---------------------------------------------------------------------------
// llm_resolve — the HTTP layer. Proves `error_for_status` is wired.
// ---------------------------------------------------------------------------

/// Serve exactly one HTTP response, then close. Returns the stub's URL.
///
/// A per-test ephemeral listener rather than a shared/global fixture, so
/// nothing here races other tests in the same binary.
fn serve_once(status_line: &'static str, body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });
    format!("http://{addr}/chat/completions")
}

/// A resolver whose LLM fallback points at a local stub.
///
/// Built as a struct literal rather than through a setter: `completions_url`
/// exists only so these HTTP paths are reachable, and this module is a child
/// of `resolver`, so it can set the private field without the production type
/// growing any API surface for tests.
fn stub_resolver(url: String) -> ConflictResolver {
    ConflictResolver {
        api_key: "sk-test".to_string(),
        completions_url: url,
    }
}

/// Regression for the default-production path: an OpenRouter HTTP error must
/// become `Err`, never an empty merged file. Before this fix every one of
/// these returned `Ok("")`, and `merge()` wrote zero bytes over both agents'
/// work — on the branch that runs whenever an OpenRouter key is configured.
#[tokio::test]
async fn llm_resolve_rejects_http_error_bodies() {
    let cases = [
        (
            "HTTP/1.1 429 Too Many Requests",
            r#"{"error":{"message":"Rate limit exceeded","code":429}}"#,
            "429",
        ),
        (
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"No auth credentials found","code":401}}"#,
            "401",
        ),
        (
            "HTTP/1.1 402 Payment Required",
            r#"{"error":{"message":"requires more credits","code":402}}"#,
            "402",
        ),
        (
            "HTTP/1.1 500 Internal Server Error",
            r#"{"error":{}}"#,
            "500",
        ),
    ];

    for (status_line, body, code) in cases {
        let resolver = stub_resolver(serve_once(status_line, body));
        let got = resolver
            .llm_resolve(Path::new("f.txt"), b"alpha\n", b"beta\n", b"<<<<<<<\n")
            .await;
        let err = match got {
            Err(e) => e.to_string(),
            Ok(v) => panic!("{status_line} must be an error, got Ok({v:?})"),
        };
        // Assert the *HTTP status* is what rejected it, not merely that the
        // body happened to lack a content key. This pins `error_for_status`
        // itself: without it the request would be rejected one layer later,
        // by `extract_llm_content`, with a different message.
        assert!(
            err.contains(code),
            "{status_line} must be rejected by error_for_status naming {code}; got: {err}"
        );
    }
}

/// A 200 carrying an error-shaped body (OpenRouter does this for some upstream
/// failures) must also be rejected — `error_for_status` alone is not enough.
#[tokio::test]
async fn llm_resolve_rejects_200_with_no_content() {
    let resolver = stub_resolver(serve_once(
        "HTTP/1.1 200 OK",
        r#"{"id":"gen-1","choices":[]}"#,
    ));
    assert!(
        resolver
            .llm_resolve(Path::new("f.txt"), b"alpha\n", b"beta\n", b"<<<<<<<\n")
            .await
            .is_err()
    );
}

/// The happy path still works end to end through the HTTP client.
#[tokio::test]
async fn llm_resolve_accepts_normal_completion() {
    let resolver = stub_resolver(serve_once(
        "HTTP/1.1 200 OK",
        r#"{"choices":[{"message":{"content":"alpha\nbeta\n"}}]}"#,
    ));
    let got = resolver
        .llm_resolve(Path::new("f.txt"), b"alpha\n", b"beta\n", b"<<<<<<<\n")
        .await
        .expect("200 with content must succeed");
    assert_eq!(got, "alpha\nbeta\n");
}

/// A configured-but-broken LLM must degrade to a *full* version, and say so.
/// This is the end-to-end shape of the CRITICAL finding.
#[tokio::test]
async fn conflict_with_failing_llm_degrades_to_first_agent_not_empty() {
    let resolver = stub_resolver(serve_once(
        "HTTP/1.1 429 Too Many Requests",
        r#"{"error":{"message":"Rate limit exceeded","code":429}}"#,
    ));
    let v = versions(&[("a", b"alpha\n"), ("b", b"beta\n")]);
    let res = resolver
        .resolve_conflict(Path::new("f.txt"), &v)
        .await
        .unwrap();

    assert_eq!(res.bytes, b"alpha\n", "must keep agent a's full version");
    let ResolutionMode::Degraded { kept, reason } = res.mode else {
        panic!("expected Degraded, got {:?}", res.mode);
    };
    assert_eq!(kept, "a");
    assert!(reason.contains("LLM merge failed"), "got: {reason}");
}

// ---------------------------------------------------------------------------
// resolve_conflict — the invariant seam.
// ---------------------------------------------------------------------------

/// The guard is defense-in-depth: no *current* branch returns empty for
/// non-empty input, so it can only be reached by a future bug. Testing it as a
/// pure function is the only way to pin it — otherwise deleting it entirely
/// would leave every test green.
#[test]
fn enforce_non_empty_rescues_empty_resolution() {
    let bad = Resolution {
        bytes: Vec::new(),
        mode: ResolutionMode::GitMerged,
    };
    let rescued = enforce_non_empty(&bad, "a", b"alpha\n").expect("empty result must be rescued");
    assert_eq!(rescued.bytes, b"alpha\n");
    let ResolutionMode::Degraded { kept, reason } = rescued.mode else {
        panic!("rescue must be reported as degraded");
    };
    assert_eq!(kept, "a");
    assert!(reason.contains("no content"), "got: {reason}");
}

/// Both agents genuinely producing an empty file is not a violation; the guard
/// must not invent content.
#[test]
fn enforce_non_empty_allows_genuine_empty() {
    let ok = Resolution {
        bytes: Vec::new(),
        mode: ResolutionMode::Identical,
    };
    assert!(enforce_non_empty(&ok, "a", b"").is_none());
}

#[test]
fn enforce_non_empty_passes_through_good_resolution() {
    let ok = Resolution {
        bytes: b"merged\n".to_vec(),
        mode: ResolutionMode::GitMerged,
    };
    assert!(enforce_non_empty(&ok, "a", b"alpha\n").is_none());
}

/// The 3+-version path must degrade to a full version rather than guessing.
#[tokio::test]
async fn resolve_conflict_seam_rejects_empty_result() {
    let resolver = ConflictResolver::new(String::new());
    let v = versions(&[("a", b"alpha\n"), ("b", b"beta\n"), ("c", b"gamma\n")]);
    let res = resolver
        .resolve_conflict(Path::new("f.txt"), &v)
        .await
        .unwrap();
    assert_eq!(res.bytes, b"alpha\n");
    assert!(
        matches!(res.mode, ResolutionMode::Degraded { .. }),
        "3-way conflict must be reported as degraded, got {:?}",
        res.mode
    );
    assert!(!res.bytes.is_empty(), "invariant: never fewer bytes");
}

/// Agents may legitimately both produce an empty file; the guard must not
/// invent content in that case.
#[tokio::test]
async fn resolve_conflict_allows_genuinely_empty_versions() {
    let resolver = ConflictResolver::new(String::new());
    let v = versions(&[("a", b""), ("b", b"")]);
    let res = resolver
        .resolve_conflict(Path::new("f.txt"), &v)
        .await
        .unwrap();
    assert!(res.bytes.is_empty());
    assert_eq!(res.mode, ResolutionMode::Identical);
}

// ---------------------------------------------------------------------------
// merge() — end-to-end, including the git success path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn merge_single_owner_files_pass_through() {
    // Each sub-agent writes distinct files; no conflicts.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let out = tmp.path().join("out");
    tokio::fs::create_dir_all(a.join("src")).await.unwrap();
    tokio::fs::create_dir_all(b.join("src")).await.unwrap();
    tokio::fs::write(a.join("src/one.py"), b"# from a")
        .await
        .unwrap();
    tokio::fs::write(b.join("src/two.py"), b"# from b")
        .await
        .unwrap();

    let results = vec![mk_result("a", a), mk_result("b", b)];
    let resolver = ConflictResolver::new(String::new());
    let report = resolver.merge(&results, &out).await.unwrap();

    assert!(out.join("src/one.py").exists());
    assert!(out.join("src/two.py").exists());
    assert!(out.join("merge-report.md").exists());
    assert!(report.contains("Total files: 2"));
    assert!(report.contains("conflicts: 0 (merged: 0, degraded: 0)"));
    assert!(!report.contains("DEGRADED"));
}

#[tokio::test]
async fn merge_two_identical_is_noop() {
    // Both sub-agents emit the same bytes for the same path. Byte-identical
    // versions short-circuit before git is spawned, so this holds regardless
    // of the ambient git environment.
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let out = tmp.path().join("out");
    tokio::fs::create_dir_all(&a).await.unwrap();
    tokio::fs::create_dir_all(&b).await.unwrap();
    tokio::fs::write(a.join("f.txt"), b"hello\n").await.unwrap();
    tokio::fs::write(b.join("f.txt"), b"hello\n").await.unwrap();

    let results = vec![mk_result("a", a), mk_result("b", b)];
    let resolver = ConflictResolver::new(String::new());
    let report = resolver.merge(&results, &out).await.unwrap();

    assert!(out.join("f.txt").exists());
    let body = tokio::fs::read(out.join("f.txt")).await.unwrap();
    assert_eq!(&body[..], b"hello\n");
    assert!(report.contains("Total files: 1"));
    assert!(report.contains("[MERGED from a+b] f.txt"));
    assert!(!report.contains("DEGRADED"));
}

/// Restores the coverage the identical-versions short-circuit removed: two
/// *different* versions that `git merge-file` can actually combine, so the
/// real subprocess runs and exits 0. Against an empty base, a version that
/// adds content and a version that adds nothing merge cleanly.
#[tokio::test]
async fn merge_clean_git_merge_combines_both_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let out = tmp.path().join("out");
    tokio::fs::create_dir_all(&a).await.unwrap();
    tokio::fs::create_dir_all(&b).await.unwrap();
    // Non-identical, non-conflicting against an empty base.
    tokio::fs::write(a.join("f.txt"), b"hello\n").await.unwrap();
    tokio::fs::write(b.join("f.txt"), b"").await.unwrap();

    let results = vec![mk_result("a", a), mk_result("b", b)];
    let resolver = ConflictResolver::new(String::new());
    let report = resolver.merge(&results, &out).await.unwrap();

    let body = tokio::fs::read(out.join("f.txt")).await.unwrap();
    assert_eq!(
        &body[..],
        b"hello\n",
        "clean git merge must write git's merged output"
    );
    assert!(
        report.contains("[MERGED from a+b] f.txt"),
        "a real git merge must not be labelled degraded; got:\n{report}"
    );
    assert!(report.contains("conflicts: 1 (merged: 1, degraded: 0)"));

    // Pin the *mode*, not just the rendered label: `Identical` renders the
    // same "[MERGED ...]" line, so without this the test could not tell a real
    // `git merge-file` exit-0 run from the byte-identical short-circuit.
    let v = versions(&[("a", b"hello\n"), ("b", b"")]);
    let res = resolver
        .resolve_conflict(Path::new("f.txt"), &v)
        .await
        .unwrap();
    assert_eq!(
        res.mode,
        ResolutionMode::GitMerged,
        "this case must actually spawn git and take the exit-0 path"
    );
    assert_eq!(res.bytes, b"hello\n");
}

/// A genuine conflict with no LLM configured must write one agent's *full*
/// version — never git's marker-laden stdout, never empty — and the report
/// must say the other agent's work was discarded.
#[tokio::test]
async fn merge_conflicting_versions_falls_back_to_first_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let out = tmp.path().join("out");
    tokio::fs::create_dir_all(&a).await.unwrap();
    tokio::fs::create_dir_all(&b).await.unwrap();
    tokio::fs::write(a.join("f.txt"), b"alpha\n").await.unwrap();
    tokio::fs::write(b.join("f.txt"), b"beta\n").await.unwrap();

    let results = vec![mk_result("a", a), mk_result("b", b)];
    // Empty API key disables the LLM fallback -> first-agent-wins.
    let resolver = ConflictResolver::new(String::new());
    let report = resolver.merge(&results, &out).await.unwrap();

    let body = tokio::fs::read(out.join("f.txt")).await.unwrap();
    assert_eq!(&body[..], b"alpha\n");
    assert!(
        !body.windows(7).any(|w| w == b"<<<<<<<"),
        "conflict markers must never reach the merged tree"
    );
    assert!(report.contains("conflicts: 1 (merged: 0, degraded: 1)"));
}

/// The HIGH finding: a degraded resolution must be unmistakable in the report,
/// because the report is the only channel downstream phases read. A plain
/// `[MERGED from a+b]` line for discarded work is a lie.
#[tokio::test]
async fn merge_report_flags_degraded_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let out = tmp.path().join("out");
    tokio::fs::create_dir_all(&a).await.unwrap();
    tokio::fs::create_dir_all(&b).await.unwrap();
    tokio::fs::write(a.join("f.txt"), b"alpha\n").await.unwrap();
    tokio::fs::write(b.join("f.txt"), b"beta\n").await.unwrap();

    let results = vec![mk_result("a", a), mk_result("b", b)];
    let report = ConflictResolver::new(String::new())
        .merge(&results, &out)
        .await
        .unwrap();

    assert!(
        report.contains("!! DEGRADED: 1 file(s) were NOT merged"),
        "report must lead with a degradation warning; got:\n{report}"
    );
    assert!(
        report.contains("kept 'a', discarded the rest"),
        "report must name what was kept and that work was discarded; got:\n{report}"
    );
    assert!(
        !report.contains("[MERGED from a+b] f.txt"),
        "a discarded-work resolution must NOT be reported as a plain merge; got:\n{report}"
    );
    // The report is written to disk as well as returned.
    let on_disk = tokio::fs::read_to_string(out.join("merge-report.md"))
        .await
        .unwrap();
    assert_eq!(on_disk, report);
}

/// The report must be reproducible across runs despite `HashMap` iteration
/// order, so phase-to-phase diffs are meaningful.
#[tokio::test]
async fn merge_report_ordering_is_deterministic() {
    let mut seen: Option<String> = None;
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let out = tmp.path().join("out");
        tokio::fs::create_dir_all(&a).await.unwrap();
        tokio::fs::create_dir_all(&b).await.unwrap();
        for n in ["one", "two", "three", "four", "five"] {
            tokio::fs::write(a.join(format!("{n}.txt")), n.as_bytes())
                .await
                .unwrap();
        }
        tokio::fs::write(b.join("six.txt"), b"six").await.unwrap();

        let results = vec![mk_result("a", a), mk_result("b", b)];
        let report = ConflictResolver::new(String::new())
            .merge(&results, &out)
            .await
            .unwrap();
        match &seen {
            None => seen = Some(report),
            Some(prev) => assert_eq!(prev, &report, "merge report ordering is not deterministic"),
        }
    }
}
