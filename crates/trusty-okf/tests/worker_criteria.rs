//! SPEC-OKF-01 §6.2 criteria 100-105, 121-123, 145 and 147 for slice S10
//! of #10746 (the extraction-worker client, feature `extract-worker`).
//!
//! Why: the worker reads untrusted bytes in another process. Every failure
//! — crash, hang, oversize output, a bad frame, a silent or lying worker —
//! must fail only its item or its run, leave no process behind, and never
//! pass for success; a connector store's bytes must reach only a sandboxed
//! worker.
//! What: Perl test doubles from `worker_support/fake.rs` behind
//! `WorkerExtractor`, run through `plan_import_with` and `import` over a
//! source tree from `s8_support`, or called directly. Each test that waits
//! on a deadline runs under `within`, a timer of the deadlines it exercises
//! plus 5 s. Criterion 99 is the boundary script's `b9_no_extraction_backend`;
//! criterion 124 is a unit test (it needs a crate-private hook).
//! Test: `cargo test -p trusty-okf --features extract-worker --test worker_criteria`.

mod s8_support;
#[path = "worker_support/fake.rs"]
mod worker_support;

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use s8_support::{Fx, frontmatter, id, instant, now, outcome, snapshot};
use serde_yaml::Value;
use trusty_okf::WorkerError;
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::bundle::{Bundle, VerificationState};
use trusty_okf::extract::worker::{Limits, SandboxPolicy, WorkerConfig, WorkerExtractor};
use trusty_okf::extract::{Extractor, TypeHint};
use trusty_okf::format::{Actor, Verification, content_hash};
use trusty_okf::import::{ImportReport, import, plan_import_with};
use worker_support::{Fake, group_gone, pids_gone, within};

const S: fn(u64) -> Duration = Duration::from_secs;

/// Deadlines short enough for a test; the defaults are pinned separately.
fn quick() -> Limits {
    Limits {
        item: S(2),
        handshake: S(2),
        shutdown: S(1),
        reap: S(2),
        ..Limits::default()
    }
}

fn config(dir: &Path, fake: &Fake, name: &str) -> WorkerConfig {
    let mut cfg = WorkerConfig::new(fake.write(dir, name), dir);
    cfg.sandbox = SandboxPolicy::Off;
    cfg.limits = quick();
    cfg
}

fn worker(dir: &Path, fake: &Fake) -> WorkerExtractor {
    WorkerExtractor::new(config(dir, fake, "worker")).unwrap()
}

/// A local document-tree store: the worker needs no sandbox for it.
fn local() -> SourceSpec {
    let loc = Locator::DocStore {
        path: "/nonexistent".into(),
        extensions: Vec::new(),
        recursive: true,
    };
    SourceSpec::new("local", None, loc, "2026-11-04T00:00:00Z")
}

/// A worker client for direct `extract` calls on a local store.
fn local_worker(dir: &Path, fake: &Fake) -> WorkerExtractor {
    let ex = worker(dir, fake);
    ex.begin_run(&local());
    ex
}

/// Plan with `ex` and import with it, as the CLI does.
fn run(fx: &Fx, ex: &dyn Extractor, at: chrono::DateTime<chrono::FixedOffset>) -> ImportReport {
    let spec = fx.spec();
    let b = Bundle::open(&fx.root).unwrap().0;
    let plan = plan_import_with(&b, &spec, &mut fx.source(), ex).unwrap();
    let mut b = Bundle::open(&fx.root).unwrap().0;
    import(&mut b, &plan, &mut fx.source(), ex, at).unwrap()
}

fn reason(r: &ImportReport, item: &str) -> String {
    outcome(r, item).2.unwrap_or_default()
}

fn cli(args: &[&str]) -> (i32, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = trusty_okf::cli::run(&args, &mut out, &mut err);
    let text = String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err);
    (code, text)
}

/// Why: the host's `hello` names this crate's version, held as a literal
/// because B3 bars the `env` token from `src/`.
/// What: the literal equals `Cargo.toml`'s version; the defaults match the
/// §2.16 limits table.
/// Test: self-contained.
#[test]
fn host_version_matches_the_package_version() {
    assert_eq!(
        trusty_okf::extract::worker::HOST_VERSION,
        env!("CARGO_PKG_VERSION")
    );
    let d = Limits::default();
    assert_eq!((d.input_bytes, d.output_bytes), (8 << 20, 5 << 20));
    assert_eq!(
        (d.item, d.handshake, d.shutdown, d.reap),
        (S(30), S(10), S(5), S(5))
    );
    assert_eq!(
        (d.items_per_worker, d.notes, d.fields_bytes),
        (500, 100, 64 << 10)
    );
    let over = Limits { item: S(301), ..d };
    assert!(over.check().is_err(), "a limit over its ceiling is refused");
}

/// Why: criterion 100 — no common protocol version, or a non-deterministic
/// worker, is `worker_incompatible` for the run, the worker is killed, and
/// other items still import.
/// What: a protocol-99 double and a non-deterministic double over a tree
/// with Markdown and two PDFs.
/// Test: self-contained.
#[test]
fn c100_no_common_protocol_is_incompatible_and_other_items_import() {
    for fake in [
        Fake {
            protocol: 99,
            ..Fake::default()
        },
        Fake {
            deterministic: false,
            ..Fake::default()
        },
    ] {
        let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.pdf", "B"), ("c.pdf", "C")]);
        let dir = fx.tmp.path().to_path_buf();
        let (r, groups) = within(S(2 + 2 + 2 + 5), move || {
            let ex = worker(&dir, &fake);
            let r = run(&fx, &ex, now());
            (r, ex.spawned_groups())
        });
        assert_eq!(outcome(&r, "a.md").1, "created");
        for pdf in ["b.pdf", "c.pdf"] {
            assert_eq!(reason(&r, pdf), "worker_incompatible", "{r:?}");
        }
        assert_eq!(r.worker_errors, vec![WorkerError::Incompatible]);
        assert_eq!(groups.len(), 1, "the spawn is not retried in the run");
        assert!(group_gone(groups[0]));
        assert_eq!(r.exit_code, 3);
    }
}

/// Why: criterion 101 — with no worker at the configured path the plan
/// shows the affected items before any write, the run reports them
/// `worker_absent`, and the CLI exits 3.
/// What: `import plan` and `import run` with `--worker` naming no file.
/// Test: self-contained.
#[test]
fn c101_absent_worker_is_planned_skipped_and_exits_3() {
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("c.pdf", "%PDF")]);
    let (root, from) = (
        fx.root.to_string_lossy().into_owned(),
        format!("dir:{}", fx.src.display()),
    );
    let gone = fx
        .tmp
        .path()
        .join("no-worker")
        .to_string_lossy()
        .into_owned();
    let before = snapshot(&fx.root);
    let (code, out) = cli(&["import", "plan", &root, "--from", &from, "--worker", &gone]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("Skip c.pdf") && out.contains("reason=worker_absent"),
        "{out}"
    );
    assert_eq!(snapshot(&fx.root), before, "the plan wrote");
    let (code, out) = cli(&["import", "run", &root, "--from", &from, "--worker", &gone]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("item c.pdf: read skipped, write none, reason worker_absent"),
        "{out}"
    );
}

/// Why: criterion 102 and the Appendix B doubles — every limit is the
/// host's: an input over the limit is not sent; oversize output, a hang, a
/// frame over the cap, a crash, a lied hash and a worker error each fail
/// only their item, and the next item gets a fresh worker; a stderr flood
/// is capped and harms nothing.
/// What: one tree with one item per behaviour and two good items.
/// Test: self-contained.
#[test]
fn c102_limits_fail_only_their_item() {
    let files: &[(&str, &str)] = &[
        ("a.md", "Alpha.\n"),
        ("bigframe.pdf", "BIG"),
        ("crash.pdf", "CRASH"),
        ("err.pdf", "ERR"),
        ("flood.pdf", "FLOOD"),
        ("good1.pdf", "first good"),
        ("hang.pdf", "HANG"),
        ("huge.pdf", &"z".repeat(4096)),
        ("lie.pdf", "LIE"),
        ("notext.pdf", "NOTEXT"),
        ("stderr.pdf", "STDERR then text"),
        ("zgood2.pdf", "second good"),
    ];
    let fx = Fx::new(files);
    let dir = fx.tmp.path().to_path_buf();
    let mut cfg = config(&dir, &Fake::default(), "worker");
    cfg.limits.input_bytes = 1024;
    cfg.limits.output_bytes = 1024;
    // One hang (item deadline) and four kill sequences (2 s TERM grace at
    // most each; these doubles die on TERM).
    let (r, groups, sent) = within(S(2 + 4 * 2 + 2) + S(5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        let r = run(&fx, &ex, now());
        let sent = std::fs::read_to_string(fx.src.join("huge.pdf"))
            .unwrap()
            .len();
        (r, ex.spawned_groups(), sent)
    });
    let want = [
        ("bigframe.pdf", "protocol_error"),
        ("crash.pdf", "crashed"),
        ("err.pdf", "extract_error"),
        ("flood.pdf", "output_too_large"),
        ("hang.pdf", "timeout"),
        ("huge.pdf", "too_large"),
        ("lie.pdf", "protocol_error"),
    ];
    for (item, code) in want {
        assert_eq!(reason(&r, item), code, "{item}: {r:?}");
        assert_eq!(outcome(&r, item).1, "none", "{item} wrote");
    }
    for ok in [
        "a.md",
        "good1.pdf",
        "zgood2.pdf",
        "notext.pdf",
        "stderr.pdf",
    ] {
        assert_eq!(outcome(&r, ok).1, "created", "{ok}: {r:?}");
    }
    assert!(r.findings.iter().any(|f| f.kind == "no_text_layer"));
    let excerpt = r
        .findings
        .iter()
        .find(|f| f.kind == "worker_stderr")
        .unwrap();
    assert!(!excerpt.detail.is_empty() && excerpt.detail.len() <= 64 * 1024);
    // Five kills (bigframe, crash, flood, hang, lie): six workers.
    assert_eq!(groups.len(), 6, "a fresh worker after each kill");
    assert!(groups.iter().all(|g| group_gone(*g)));
    assert!(r.worker_errors.is_empty(), "{:?}", r.worker_errors);
    assert_eq!((r.exit_code, sent), (3, 4096));
}

/// Why: criterion 103 — a timeout, a cancel and a host panic each leave no
/// process in the worker's group, every child is reaped, and a child that
/// ignores SIGTERM dies by the group SIGKILL.
/// What: a double with a SIGTERM-ignoring child, three ways.
/// Test: self-contained.
#[test]
fn c103_timeout_cancel_and_panic_leave_no_process() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let hint = TypeHint::for_path("x.pdf");
    // Timeout: item deadline, then TERM grace, then the reap.
    let pidfile = dir.join("timeout.pids");
    let fake = Fake {
        child: true,
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let (d, h) = (dir.clone(), hint.clone());
    let code = within(S(2 + 2 + 2 + 5), move || {
        let ex = local_worker(&d, &fake);
        ex.extract(b"HANG", &h).unwrap_err().code
    });
    assert_eq!(code, "timeout");
    assert!(pids_gone(&pidfile), "the worker or its child survived");

    // Cancel: a second thread sets the flag while the item hangs.
    let pidfile = dir.join("cancel.pids");
    let fake = Fake {
        child: true,
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let (d, h) = (dir.clone(), hint.clone());
    let (code, took) = within(S(2 + 2 + 5), move || {
        let ex = local_worker(&d, &fake);
        let flag = ex.cancel_handle();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let t = Instant::now();
        let code = ex.extract(b"HANG", &h).unwrap_err().code;
        let again = ex.extract(b"fine", &h).unwrap_err().code;
        assert_eq!(again, "cancelled");
        (code, t.elapsed())
    });
    assert_eq!(code, "cancelled");
    assert!(
        took < S(2) + S(2),
        "cancel waited for the item deadline: {took:?}"
    );
    assert!(pids_gone(&pidfile));

    // Host panic: the extractor's Drop runs the kill sequence.
    let pidfile = dir.join("panic.pids");
    let fake = Fake {
        child: true,
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let d = dir.clone();
    let joined = within(S(2 + 2 + 2 + 5), move || {
        std::thread::spawn(move || {
            let ex = worker(&d, &fake);
            assert_ne!(ex.version(), "unavailable");
            panic!("host panic with a live worker");
        })
        .join()
        .is_err()
    });
    assert!(joined, "the host thread panicked");
    assert!(pids_gone(&pidfile));
}

/// Why: criterion 104 (B10) — the worker's environment holds only the
/// allow-listed variables and its working directory is fresh and empty.
/// What: a double that answers its environment and directory; the
/// directory is removed once the worker is reaped.
/// Test: self-contained.
#[test]
fn c104_worker_env_is_the_allow_list_and_cwd_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let fake = Fake {
        mode: "env",
        ..Fake::default()
    };
    let d = dir.clone();
    let text = within(S(2 + 2 + 1 + 5), move || {
        let ex = local_worker(&d, &fake);
        let got = ex.extract(b"x", &TypeHint::for_path("x.pdf")).unwrap();
        assert!(ex.end_run().errors.is_empty());
        got.text
    });
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(&lines[..2], &["LANG=C.UTF-8", "TZ=UTC"], "{text}");
    let cwd = lines[2].strip_prefix("CWD=").unwrap();
    assert!(Path::new(cwd).starts_with(&dir), "{cwd}");
    assert_eq!(lines[3], "ENTRIES=0");
    assert_eq!(lines.len(), 4, "{text}");
    assert!(
        !Path::new(cwd).exists(),
        "the working directory was left behind"
    );
}

/// Why: criterion 105 — each extracted concept records the §2.16
/// provenance, a re-import of unchanged bytes writes nothing, and a worker
/// configuration change re-extracts exactly the items the worker produced.
/// What: import with config `cfg1`, again unchanged, then with `cfg2`.
/// Test: self-contained.
#[test]
fn c105_config_change_reextracts_exactly_the_worker_items() {
    let fx = Fx::new(&[
        ("a.md", "Alpha.\n"),
        ("b.pdf", "Bravo text"),
        ("c.pdf", "Charlie"),
    ]);
    let dir = fx.tmp.path().to_path_buf();
    let fake = |config: &str| Fake {
        config: config.into(),
        ..Fake::default()
    };
    let (d, f1) = (dir.clone(), fake("cfg1"));
    let fx = within(S(2 + 1 + 5), move || {
        let r = run(&fx, &worker(&d, &f1), now());
        assert_eq!((r.created, r.exit_code), (3, 0), "{r:?}");
        fx
    });
    let fm = frontmatter(&fx.root, &id("b.pdf").file_name());
    let sha = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(b"Bravo text")
    );
    for (k, v) in [
        ("extracted_by", Value::from("xberg-worker@1.0.0")),
        ("extract_protocol", Value::from(1)),
        ("xberg_version", Value::from("1.4.0")),
        ("extract_config", Value::from("cfg1")),
        ("extract_input_sha256", Value::from(sha.as_str())),
    ] {
        assert_eq!(fm.get(k), Some(&v), "{k}: {fm:?}");
    }
    let before = snapshot(&fx.root);
    let (d, f1) = (dir.clone(), fake("cfg1"));
    let fx = within(S(2 + 1 + 5), move || {
        let r = run(&fx, &worker(&d, &f1), instant("2026-11-04T13:00:00Z"));
        assert_eq!((r.written, r.unchanged), (0, 3), "{r:?}");
        fx
    });
    assert_eq!(snapshot(&fx.root), before, "an unchanged re-import wrote");
    let (d, f2) = (dir.clone(), fake("cfg2"));
    let r = within(S(2 + 1 + 5), move || {
        run(&fx, &worker(&d, &f2), instant("2026-11-04T14:00:00Z"))
    });
    assert_eq!(outcome(&r, "a.md").1, "unchanged");
    for pdf in ["b.pdf", "c.pdf"] {
        assert_eq!(
            outcome(&r, pdf).0,
            "read",
            "{pdf} was not re-extracted: {r:?}"
        );
    }
}

/// Why: criteria 121 and 122 — a worker that never answers `hello`, never
/// reads stdin, or sends another frame first cannot hang the import: it is
/// killed and reaped within the handshake deadline plus the kill sequence,
/// its items fail, other items import, and the CLI exits 3.
/// What: three doubles through `import`, then the CLI with the default
/// 10 s handshake.
/// Test: self-contained.
#[test]
fn c121_silent_worker_times_out_its_handshake_and_is_reaped() {
    for (mode, code, err) in [
        (
            "no_hello",
            "handshake_timeout",
            Some(WorkerError::HandshakeTimeout),
        ),
        (
            "no_read",
            "handshake_timeout",
            Some(WorkerError::HandshakeTimeout),
        ),
        ("bad_first", "protocol_error", None),
    ] {
        let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.pdf", "B"), ("c.pdf", "C")]);
        let dir = fx.tmp.path().to_path_buf();
        let fake = Fake {
            mode,
            ..Fake::default()
        };
        let budget = S(2) + S(2) + S(2);
        let (r, groups, took) = within(budget + S(5), move || {
            let ex = worker(&dir, &fake);
            let t = Instant::now();
            let r = run(&fx, &ex, now());
            (r, ex.spawned_groups(), t.elapsed())
        });
        assert!(
            took <= budget + Duration::from_millis(500),
            "{mode}: {took:?}"
        );
        assert_eq!(outcome(&r, "a.md").1, "created", "{mode}");
        assert_eq!(
            (reason(&r, "b.pdf"), reason(&r, "c.pdf")),
            (code.into(), code.into())
        );
        assert_eq!(
            r.worker_errors,
            err.into_iter().collect::<Vec<_>>(),
            "{mode}"
        );
        assert_eq!(groups.len(), 1, "{mode}: no retry in the run");
        assert!(group_gone(groups[0]), "{mode}");
        assert_eq!(r.exit_code, 3);
    }
    // The CLI, default deadlines.
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.pdf", "B")]);
    let w = Fake {
        mode: "no_hello",
        ..Fake::default()
    }
    .write(fx.tmp.path(), "silent");
    let args = [
        "import".to_string(),
        "run".into(),
        fx.root.to_string_lossy().into_owned(),
        "--from".into(),
        format!("dir:{}", fx.src.display()),
        "--worker".into(),
        w.to_string_lossy().into_owned(),
    ];
    let (code, out) = within(S(10 + 2 + 5) + S(5), move || {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        cli(&a)
    });
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("reason handshake_timeout")
            && out.contains("worker: worker handshake timed out"),
        "{out}"
    );
}

/// Why: criterion 123 — a worker that ignores `shutdown`, a closed stdin
/// and SIGTERM is killed by SIGKILL within the shutdown deadline plus 2 s,
/// reaped, and recorded; committed items keep their outcomes.
/// What: an `ignore_shutdown` double with a SIGTERM-ignoring child.
/// Test: self-contained.
#[test]
fn c123_worker_ignoring_shutdown_is_killed_and_reaped() {
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.pdf", "B")]);
    let dir = fx.tmp.path().to_path_buf();
    let pidfile = dir.join("pids");
    let fake = Fake {
        mode: "ignore_shutdown",
        child: true,
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let (r, took, _fx) = within(S(2 + 1 + 2 + 2) + S(5), move || {
        let ex = worker(&dir, &fake);
        let spec = fx.spec();
        let b = Bundle::open(&fx.root).unwrap().0;
        let plan = plan_import_with(&b, &spec, &mut fx.source(), &ex).unwrap();
        let mut b = Bundle::open(&fx.root).unwrap().0;
        let mut src = fx.source();
        let t = Instant::now();
        let r = import(&mut b, &plan, &mut src, &ex, now()).unwrap();
        (r, t.elapsed(), fx)
    });
    assert_eq!(r.worker_errors, vec![WorkerError::ShutdownTimeout]);
    assert_eq!(outcome(&r, "b.pdf").1, "created");
    assert_eq!(r.created, 2);
    // shutdown (1 s) + TERM grace (2 s) + spawn and item work, with slack.
    assert!(took < S(1 + 2 + 3), "{took:?}");
    assert!(pids_gone(&pidfile), "the worker or its child survived");
    assert_eq!(r.exit_code, 3, "a run-level failure is never a clean exit");
}

fn connector() -> SourceSpec {
    let loc = Locator::Connector {
        kind: "gdocs".into(),
        params: ConnectorParams::new(),
    };
    SourceSpec::new("docs", None, loc, "2026-11-04T00:00:00Z")
}

/// Why: criterion 145 (item 9 B, r7.1) — the host decides from its own
/// spawn evidence. Without the sandbox a connector item is skipped
/// `sandbox_required` and a local item extracts; a worker whose `sandbox`
/// claim disagrees with the evidence is `worker_incompatible` and gets no
/// item; under the sandbox the worker can open no socket and read no file
/// outside its working directory.
/// What: `Off`-policy doubles, then (where the platform launcher exists)
/// `Platform`-policy doubles with a TCP listener and a file outside.
/// Test: self-contained.
#[test]
fn c145_sandbox_evidence_gates_connector_items_and_blocks_io() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let hint = TypeHint::for_path("x.pdf");
    let d = dir.clone();
    let h = hint.clone();
    within(S(2 + 2 + 2 + 5), move || {
        let ex = worker(&d, &Fake::default());
        // No begin_run yet: treated as a connector store (fails closed).
        assert_eq!(ex.extract(b"x", &h).unwrap_err().code, "sandbox_required");
        ex.begin_run(&connector());
        assert_eq!(ex.extract(b"x", &h).unwrap_err().code, "sandbox_required");
        ex.begin_run(&local());
        assert_eq!(ex.extract(b"local", &h).unwrap().text, "local");
    });
    let pidfile = dir.join("liar.pids");
    let liar = Fake {
        sandbox: "seatbelt".into(),
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let d = dir.clone();
    let h = hint.clone();
    let (code, run) = within(S(2 + 2 + 2 + 5), move || {
        let ex = local_worker(&d, &liar);
        (ex.extract(b"x", &h).unwrap_err().code, ex.end_run())
    });
    assert_eq!(code, "worker_incompatible");
    assert_eq!(run.errors, vec![WorkerError::Incompatible]);
    let log = std::fs::read_to_string(&pidfile).unwrap();
    assert!(
        !log.contains("item"),
        "the host sent an item to a lying worker"
    );

    let launcher = if cfg!(target_os = "macos") {
        "/usr/bin/sandbox-exec"
    } else {
        "/usr/bin/bwrap"
    };
    if !Path::new(launcher).is_file() {
        // No launcher on this host: the evidence shows no sandbox, so the
        // Platform policy is the Off case above. Pin that, and say so.
        eprintln!("c145: no {launcher}; sandboxed half covered by the no-evidence path");
        let mut cfg = config(&dir, &Fake::default(), "plain");
        cfg.sandbox = SandboxPolicy::Platform;
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&connector());
        assert_eq!(
            ex.extract(b"x", &hint).unwrap_err().code,
            "sandbox_required"
        );
        return;
    }
    eprintln!("c145: sandboxed half runs under {launcher}");
    // Spawned under the sandbox but claiming none: incompatible.
    let mut cfg = config(&dir, &Fake::default(), "claims-none");
    cfg.sandbox = SandboxPolicy::Platform;
    let h = hint.clone();
    let code = within(S(2 + 2 + 2 + 5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&connector());
        ex.extract(b"x", &h).unwrap_err().code
    });
    assert_eq!(code, "worker_incompatible");
    // Under the sandbox: a connector item extracts, and both escapes fail.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().canonicalize().unwrap().join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();
    let probe = Fake {
        mode: "probe",
        sandbox: "platform".into(),
        port: listener.local_addr().unwrap().port(),
        secret: Some(secret),
        ..Fake::default()
    };
    let mut cfg = config(&dir, &probe, "probe");
    cfg.sandbox = SandboxPolicy::Platform;
    let text = within(S(2 + 2 + 2 + 5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&connector());
        ex.extract(b"x", &hint).map(|e| e.text)
    });
    assert_eq!(text.unwrap(), "socket=fail;file=fail");
    assert!(
        listener.accept().is_err(),
        "the sandboxed worker reached the listener"
    );
}

/// Why: criterion 147 (item 21 B) — a worker upgrade that yields the same
/// text keeps a human-verified revision Verified; a text change or a
/// change to a recipient field makes it Unverified with history.
/// What: worker 1 imports and the concept is verified; worker 2 (new
/// version, xberg version and config) re-extracts the same text; then a
/// changed `to` field; then changed text.
/// Test: self-contained.
#[test]
fn c147_worker_upgrade_keeps_verification_and_text_change_clears_it() {
    let fx = Fx::new(&[("b.pdf", "Bravo")]);
    let dir = fx.tmp.path().to_path_buf();
    let v1 = Fake {
        fields: r#"{"to":"ann@example.com"}"#.into(),
        ..Fake::default()
    };
    let v2 = Fake {
        version: "2.0.0".into(),
        xberg: "1.5.0".into(),
        config: "cfg2".into(),
        ..v1.clone()
    };
    let v3 = Fake {
        version: "3.0.0".into(),
        fields: r#"{"to":"bob@example.com"}"#.into(),
        ..v2.clone()
    };
    let v4 = Fake {
        version: "4.0.0".into(),
        text: Some("Bravo, revised".into()),
        ..v3.clone()
    };
    let b_id = id("b.pdf");
    let state = |fx: &Fx| {
        Bundle::open(&fx.root)
            .unwrap()
            .0
            .verification_state(&b_id)
            .unwrap()
    };
    let step = |fx: Fx, fake: Fake, at: &str| {
        let (d, at) = (dir.clone(), instant(at));
        within(S(2 + 1 + 5), move || {
            let r = run(&fx, &worker(&d, &fake), at);
            assert_eq!(r.exit_code, 0, "{r:?}");
            fx
        })
    };
    let fx = step(fx, v1, "2026-11-04T12:00:00Z");
    let mut b = Bundle::open(&fx.root).unwrap().0;
    let seen = content_hash(&b.get(&b_id).unwrap());
    let v = Verification {
        by: Actor::Process("rev".into()),
        at: now(),
        stale_after: None,
    };
    b.verify(&b_id, v, &seen).unwrap();
    drop(b);
    assert!(matches!(state(&fx), VerificationState::Verified { .. }));

    let fx = step(fx, v2, "2026-11-04T13:00:00Z");
    let fm = frontmatter(&fx.root, &b_id.file_name());
    assert_eq!(
        fm.get("extracted_by"),
        Some(&Value::from("xberg-worker@2.0.0"))
    );
    assert_eq!(fm.get("xberg_version"), Some(&Value::from("1.5.0")));
    assert_eq!(fm.get("extract_config"), Some(&Value::from("cfg2")));
    let b = Bundle::open(&fx.root).unwrap().0;
    assert_eq!(
        content_hash(&b.get(&b_id).unwrap()),
        seen,
        "provenance counted as content"
    );
    assert!(
        matches!(state(&fx), VerificationState::Verified { .. }),
        "an upgrade cleared it"
    );

    let fx = step(fx, v3, "2026-11-04T14:00:00Z");
    assert!(
        matches!(state(&fx), VerificationState::Unverified { history } if history >= 1),
        "a recipient change kept the verification"
    );
    // Re-verify, then change the text.
    let mut b = Bundle::open(&fx.root).unwrap().0;
    let seen = content_hash(&b.get(&b_id).unwrap());
    let v = Verification {
        by: Actor::Process("rev".into()),
        at: instant("2026-11-04T14:30:00Z"),
        stale_after: None,
    };
    b.verify(&b_id, v, &seen).unwrap();
    drop(b);
    let fx = step(fx, v4, "2026-11-04T15:00:00Z");
    assert!(matches!(state(&fx), VerificationState::Unverified { history } if history >= 1));
}

/// Why: #10746 S10 fix round, finding 1 — a worker that floods frames must
/// not grow host memory, and a frame nobody asked for is a protocol
/// violation.
/// What: after its `ok` reply the double streams 2000 frames of 16 KiB,
/// logging each; one second later it has sent only a handful (it blocks on
/// its full pipe); the next item fails `protocol_error`, and every process
/// is gone after the run.
/// Test: self-contained.
#[test]
fn c102_flooded_frames_are_bounded_and_unsolicited_is_protocol_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let pidfile = dir.join("spam.pids");
    let fake = Fake {
        pidfile: Some(pidfile.clone()),
        ..Fake::default()
    };
    let (d, p) = (dir.clone(), pidfile.clone());
    let (sent, next) = within(S(2 + 1 + 2 + 2 + 5), move || {
        let ex = local_worker(&d, &fake);
        let hint = TypeHint::for_path("x.pdf");
        assert_eq!(ex.extract(b"SPAM", &hint).unwrap().text, "SPAM");
        std::thread::sleep(S(1));
        let log = std::fs::read_to_string(&p).unwrap();
        let sent = log.lines().filter(|l| l.starts_with("sent ")).count();
        let next = ex.extract(b"next", &hint).unwrap_err().code;
        assert!(ex.end_run().errors.is_empty());
        (sent, next)
    });
    assert!(sent < 100, "the host buffered {sent} unread frames");
    assert_eq!(next, "protocol_error");
    assert!(pids_gone(&pidfile));
}

/// Why: #10746 S10 fix round, finding 2 — §2.16 requires `RLIMIT_CPU` and
/// `RLIMIT_AS` where the OS honours them; a runaway item fails alone.
/// What: a 1 s CPU limit and (not on macOS, which does not honour it) a
/// 512 MiB address-space limit; a CPU-burning item fails `crashed` long
/// before its 10 s wall time, a 1 GiB allocation fails `crashed`, and a
/// normal item after each succeeds on a fresh worker.
/// Test: self-contained.
#[test]
fn rlimits_kill_a_runaway_item_and_spare_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let mut cfg = config(&dir, &Fake::default(), "worker");
    cfg.limits.item = S(10);
    cfg.limits.cpu_seconds = 1;
    cfg.limits.address_space = 512 << 20;
    let (spin, took, alloc, fine) = within(S(10 + 2 + 2 + 5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&local());
        let hint = TypeHint::for_path("x.pdf");
        let t = Instant::now();
        let spin = ex.extract(b"SPIN", &hint).unwrap_err().code;
        let took = t.elapsed();
        // macOS does not honour RLIMIT_AS, so the 1 GiB item is sent only
        // where the limit exists (on macOS it would just allocate).
        let alloc = if cfg!(target_os = "macos") {
            Ok(String::new())
        } else {
            ex.extract(b"ALLOC", &hint).map(|e| e.text)
        };
        let fine = ex.extract(b"fine", &hint).unwrap().text;
        (spin, took, alloc, fine)
    });
    assert_eq!(spin, "crashed", "RLIMIT_CPU did not stop the item");
    assert!(took < S(6), "{took:?}");
    if cfg!(target_os = "macos") {
        // macOS does not honour RLIMIT_AS; the wall time is the bound.
        eprintln!("rlimits: RLIMIT_AS half not applicable on macOS");
    } else {
        assert_eq!(alloc.unwrap_err().code, "crashed", "RLIMIT_AS did not hold");
    }
    assert_eq!(fine, "fine");
}

/// Kills every leftover `okf-setsid-<marker>` process when dropped.
struct Sweep(String);

impl Drop for Sweep {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f", &self.0])
            .status();
    }
}

fn marker_alive(marker: &str) -> bool {
    let until = Instant::now() + S(3);
    loop {
        let found = std::process::Command::new("pgrep")
            .args(["-f", marker])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if !found || Instant::now() >= until {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Why: #10746 S10 fix round, finding 4 — a sandboxed worker must not leave
/// a grandchild that `setsid`s out of its process group (and would hold the
/// host's pipes): macOS denies `fork`, Linux's PID namespace takes the
/// grandchild down with the worker.
/// What: under the platform sandbox a double tries to fork a `setsid`
/// grandchild with a unique name; after the run no such process remains,
/// and on macOS the fork itself fails.
/// Test: self-contained.
#[test]
fn sandboxed_worker_cannot_fork_a_setsid_grandchild() {
    let launcher = if cfg!(target_os = "macos") {
        "/usr/bin/sandbox-exec"
    } else {
        "/usr/bin/bwrap"
    };
    if !Path::new(launcher).is_file() {
        eprintln!("setsid: no {launcher}; nothing is sandboxed on this host");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let tag = format!("m{}x{}", std::process::id(), now().timestamp());
    let marker = format!("okf-setsid-{tag}");
    let _sweep = Sweep(marker.clone());
    let fake = Fake {
        config: tag,
        sandbox: "platform".into(),
        ..Fake::default()
    };
    let mut cfg = config(&dir, &fake, "worker");
    cfg.sandbox = SandboxPolicy::Platform;
    let text = within(S(2 + 1 + 2 + 2 + 5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&local());
        let text = ex
            .extract(b"FORK", &TypeHint::for_path("x.pdf"))
            .map(|e| e.text);
        let _ = ex.end_run();
        text
    });
    let text = text.unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(text, "fork=fail", "the macOS profile allowed fork");
    }
    assert!(
        !marker_alive(&marker),
        "a setsid grandchild outlived the worker"
    );
}

/// Why: #10746 S10 fix round, finding 3 — Ctrl-C on the CLI cancels the
/// import; the worker's process group is killed and reaped, the report is
/// printed, and the CLI exits 3 instead of dying with the worker orphaned.
/// What: the built binary imports a tree whose PDF hangs the worker; once
/// the worker runs, SIGINT goes to the CLI.
/// Test: self-contained.
#[test]
fn c103_sigint_cancels_the_cli_and_reaps_the_worker() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.pdf", "HANG"), ("c.pdf", "C")]);
    // The CLI applies the platform sandbox when its launcher exists; the
    // double's `hello` must agree with that spawn evidence.
    let launcher = if cfg!(target_os = "macos") {
        "/usr/bin/sandbox-exec"
    } else {
        "/usr/bin/bwrap"
    };
    let claim = if Path::new(launcher).is_file() {
        "platform"
    } else {
        "none"
    };
    let w = Fake {
        sandbox: claim.into(),
        ..Fake::default()
    }
    .write(fx.tmp.path(), "worker");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-okf"))
        .args(["import", "run"])
        .arg(&fx.root)
        .arg("--from")
        .arg(format!("dir:{}", fx.src.display()))
        .arg("--worker")
        .arg(&w)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let cli = child.id();
    let until = Instant::now() + S(10);
    let worker = loop {
        let out = std::process::Command::new("pgrep")
            .args(["-P", &cli.to_string()])
            .output()
            .unwrap();
        let pids = String::from_utf8_lossy(&out.stdout).into_owned();
        if let Some(p) = pids.split_whitespace().next() {
            break p.parse::<i32>().unwrap();
        }
        assert!(Instant::now() < until, "the CLI started no worker");
        std::thread::sleep(Duration::from_millis(50));
    };
    // Let the handshake finish and the hanging item go out.
    std::thread::sleep(Duration::from_millis(1500));
    kill(Pid::from_raw(cli as i32), Signal::SIGINT).unwrap();
    let out = within(S(2 + 2 + 5), move || child.wait_with_output().unwrap());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(3), "{:?} {text}", out.status);
    assert!(
        text.contains("item b.pdf: read failed, write none, reason cancelled"),
        "{text}"
    );
    assert!(
        text.contains("item c.pdf: read skipped, write none, reason cancelled"),
        "{text}"
    );
    assert!(group_gone(worker), "the worker group outlived the CLI");
}

/// Why: #10746 S10 last change — `RLIMIT_CPU` counts the worker's whole
/// life, so a run of valid items must never spend it and get a valid item
/// killed mid-request.
/// What: a 2 s CPU budget and six items of 0.5 s CPU each; every item
/// succeeds, on more than one worker (the host recycles before the budget
/// runs out).
/// Test: self-contained.
#[test]
fn cpu_budget_recycles_before_a_valid_item_is_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let mut cfg = config(&dir, &Fake::default(), "worker");
    cfg.limits.item = S(10);
    cfg.limits.cpu_seconds = 2;
    let (codes, groups) = within(S(6 * 10 + 2 + 5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        ex.begin_run(&local());
        let hint = TypeHint::for_path("x.pdf");
        let codes: Vec<String> = (0..6)
            .map(|_| match ex.extract(b"BURN", &hint) {
                Ok(e) => e.text,
                Err(e) => e.code,
            })
            .collect();
        assert!(ex.end_run().errors.is_empty());
        (codes, ex.spawned_groups())
    });
    assert_eq!(codes, vec!["BURN"; 6], "a valid item was killed");
    assert!(groups.len() > 1, "the worker was never recycled");
    assert!(groups.iter().all(|g| group_gone(*g)));
}
