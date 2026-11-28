//! The port's failure arms and in-process seams (#12029 P2g): refusals, the
//! kick, the reminders read, the shared lock and the level-read warning.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::cli::{Kick, Options, run};
use super::normalize::{question_title, reader_path};
use crate::clock::FakeClock;
use crate::dashboard::store::writer::{WriterClass, claims};
use crate::spec_drift::Outcome;

type R = Result<(), Box<dyn std::error::Error>>;

const NOW: f64 = 1_795_784_400.0;

/// A store under `<checkout>/local/owner-queue` with a few synthetic items.
fn checkout() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let items = tmp.path().join("local/owner-queue/items");
    std::fs::create_dir_all(&items)?;
    let docs = [
        (
            "D01",
            r#"{"kind": "decision", "status": "open", "title": "Pick", "task": "T01", "version": 1}"#,
        ),
        (
            "D04",
            r#"{"kind": "decision", "status": "interactive", "title": "Talk", "version": 1}"#,
        ),
        (
            "T01",
            r#"{"kind": "task", "status": "open", "title": "Ship", "goal": "G01", "projects": ["p/q"], "progress": {"pct": 5}, "version": 1}"#,
        ),
        (
            "D02",
            r#"{"kind": "decision", "status": "answered", "answer": "Yes", "version": 1}"#,
        ),
    ];
    for (id, body) in docs {
        std::fs::write(items.join(format!("{id}.json")), body)?;
    }
    Ok(tmp)
}

/// Runs the command line on the checkout at `root` with `env`, `datg` and `kick`.
fn go(root: &Path, args: &[&str], env: &[(&str, &str)], datg: &Path, kick: Kick) -> Outcome {
    go_bounded(root, args, env, datg, kick, crate::TMUX_TIMEOUT)
}

/// [`go`] with the kick waited for at most `bound`.
fn go_bounded(
    root: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    datg: &Path,
    kick: Kick,
    bound: Duration,
) -> Outcome {
    let vars: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    let lookup = move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let o = Options {
        checkout: root.to_path_buf(),
        cwd: root.to_path_buf(),
        env: &lookup,
        clock: Arc::new(FakeClock::new(NOW)),
        datg: datg.to_path_buf(),
        kick,
        kick_timeout: bound,
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    run(&args, &mut std::io::empty(), &o)
}

fn no_kick() -> Kick {
    Arc::new(|| Err("the kick must not run here".into()))
}

/// Every file under `root` with its bytes, the lock file left out.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n != ".lock") {
                out.push((p.clone(), std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn question_title_matches_the_script() {
    // The frozen script's `question_title` on each input (architect 0fbbc81eed).
    let cases = [
        ("What time works? More text", "What time works?"),
        ("[Read this](http://x) first.  Then more", "Read this first"),
        (
            "no punctuation at all here in this long sentence that goes on and on",
            "no punctuation at all here in this long sentence that\u{2026}",
        ),
        ("Ends with colon: and more", "Ends with colon"),
        ("a.b. c", "a.b"),
        ("?x", "?x"),
        ("[](x) [y](z)", "[](x) y"),
        (
            "one two three four five six seven eight nine ten.",
            "one two three four five six seven eight nine ten",
        ),
        (
            "one two three four five six seven eight nine ten eleven.",
            "one two three four five six seven eight nine ten\u{2026}",
        ),
        ("  \t ", ""),
        ("Hi!", "Hi!"),
    ];
    for (asked, want) in cases {
        assert_eq!(question_title(asked), want, "{asked:?}");
    }
}

/// Each write path's refusal: the exit the script gives, its error line,
/// and not one byte of the store changed.
#[test]
fn every_write_refusal_leaves_the_store_unchanged() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let r = ["--root", "local/owner-queue"];
    let cases: [(&[&str], i32, &str); 12] = [
        (
            &["set", "items", "D01", "--json", "{}"],
            3,
            "dashboard-db: conflict: items/D01 already exists",
        ),
        (
            &["set", "items", "D01", "--if-version", "4", "--json", "{}"],
            3,
            "if_version 4 but current version is 1",
        ),
        (
            &[
                "set",
                "items",
                "T09",
                "--json",
                r#"{"kind": "task", "steps": [{"text": "a"}]}"#,
            ],
            2,
            "('text' is not rendered",
        ),
        (
            &["update", "items", "T01", "--json", r#"{"progress": null}"#],
            2,
            "every open task needs progress",
        ),
        (
            &["update", "items", "D0z", "--json", "{}"],
            1,
            "dashboard-db: not_found: items/D0z not found",
        ),
        (
            &["create", "items", "D", "--json", r#"{"detail": "short"}"#],
            2,
            "a D item needs `detail`",
        ),
        (
            &["create", "items", "A", "--json", "{}"],
            2,
            "prefix A is retired",
        ),
        (
            &["delete", "items", "D02", "--if-version", "2"],
            3,
            "if_version 2 but current version is 1",
        ),
        (&["log", "items", "T01", " "], 2, "log text is empty"),
        (&["log", "items", "T0z", "x"], 1, "items/T0z not found"),
        (
            &[
                "batch",
                r#"[{"op": "set", "collection": "items", "id": "D01", "data": {}}]"#,
            ],
            3,
            "already exists",
        ),
        (
            &[
                "batch",
                r#"[{"op": "update", "collection": "items", "id": "D01", "data": {}}, {"op": "delete", "collection": "items", "id": "D0z"}]"#,
            ],
            1,
            "items/D0z not found",
        ),
    ];
    let datg = root.join("no-datg");
    for (args, code, line) in cases {
        let before = snapshot(root);
        let argv: Vec<&str> = r.iter().chain(args).copied().collect();
        let out = go(root, &argv, &[], &datg, no_kick());
        assert_eq!(out.code, code, "{args:?}: {}", out.stderr);
        assert!(out.stderr.contains(line), "{args:?}: {}", out.stderr);
        assert!(out.stdout.is_empty(), "{args:?}: {}", out.stdout);
        assert_eq!(snapshot(root), before, "{args:?} changed the store");
    }
    // status-brief: the schema refuses the entry, nothing is written.
    let schemas = root.join("schemas/dashboard");
    std::fs::create_dir_all(&schemas)?;
    std::fs::write(
        schemas.join("items.schema.json"),
        r#"{"type": "object", "properties": {"status_briefs": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string", "minLength": 1}}}}}}"#,
    )?;
    let before = snapshot(root);
    let out = go(
        root,
        &[
            "--root",
            "local/owner-queue",
            "status-brief",
            "T01",
            "",
            "x.md",
        ],
        &[],
        &datg,
        no_kick(),
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("/status_briefs/0/id"), "{}", out.stderr);
    assert_eq!(snapshot(root), before);
    Ok(())
}

/// A delete whose prune-log line cannot be written is refused and keeps the document.
#[test]
fn a_delete_whose_prune_log_cannot_be_written_is_refused() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    // `<root>/../owner-queue-archive` is a file, so the log's directory cannot be made.
    std::fs::write(root.join("local/owner-queue-archive"), "not a dir")?;
    let out = go(
        root,
        &["--root", "local/owner-queue", "delete", "items", "D02"],
        &[],
        &root.join("x"),
        no_kick(),
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("delete refused"), "{}", out.stderr);
    assert!(root.join("local/owner-queue/items/D02.json").exists());
    Ok(())
}

/// `create` writes the document, then the counter file; a counter that
/// cannot be written is the script's traceback (exit 1) after the write,
/// as in the frozen script.
#[test]
fn a_create_whose_counter_cannot_be_written_exits_1_after_the_write() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    std::fs::create_dir(root.join("local/owner-queue/.id-counters.json"))?;
    let body = r#"{"title": "x", "task": "T01"}"#;
    let out = go(
        root,
        &[
            "--root",
            "local/owner-queue",
            "create",
            "items",
            "B",
            "--json",
            body,
        ],
        &[],
        &root.join("x"),
        no_kick(),
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("Error"), "{}", out.stderr);
    assert!(root.join("local/owner-queue/items/B01.json").exists());
    Ok(())
}

/// The kick runs once the update is on disk and the store lock is free; its
/// error is a stderr line and the exit stays 0. `QUESTIONS_RC_KICK=off` skips it.
#[test]
fn kick_runs_after_the_write_and_never_changes_the_exit_code() -> R {
    let tmp = checkout()?;
    let root = tmp.path().to_path_buf();
    let ran = Arc::new(AtomicUsize::new(0));
    let (count, dir) = (Arc::clone(&ran), root.clone());
    let kick: Kick = Arc::new(move || -> Result<(), String> {
        count.fetch_add(1, Ordering::SeqCst);
        let doc = std::fs::read_to_string(dir.join("local/owner-queue/items/D04.json"))
            .map_err(|e| e.to_string())?;
        if !doc.contains("\"answered\"") {
            return Err("the write is not on disk yet".into());
        }
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("local/owner-queue/.lock"))
            .map_err(|e| e.to_string())?;
        // SAFETY: the fd stays open for `lock`'s life; flock touches nothing else.
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err("the store lock is still held".into());
        }
        Err("tmux is not running".into())
    });
    let args = [
        "--root",
        "local/owner-queue",
        "update",
        "items",
        "D04",
        "--json",
        r#"{"status": "answered"}"#,
    ];
    let out = go(&root, &args, &[], &root.join("x"), kick.clone());
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stderr,
        "questions_rc kick failed: tmux is not running\n"
    );
    assert!(out.stdout.contains("\"version\": 2"), "{}", out.stdout);
    // Off: no kick at all; an update that stays interactive never kicks.
    std::fs::write(
        root.join("local/owner-queue/items/D04.json"),
        r#"{"status": "interactive", "version": 1}"#,
    )?;
    let out = go(
        &root,
        &args,
        &[("QUESTIONS_RC_KICK", "off")],
        &root.join("x"),
        kick.clone(),
    );
    assert_eq!(
        (ran.load(Ordering::SeqCst), out.code, out.stderr.as_str()),
        (1, 0, "")
    );
    let stay = [
        "--root",
        "local/owner-queue",
        "update",
        "items",
        "D01",
        "--json",
        r#"{"status": "answered"}"#,
    ];
    assert_eq!(go(&root, &stay, &[], &root.join("x"), kick).code, 0);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    Ok(())
}

/// #12029 P2g: a kick that outlasts its bound costs one warning, never the
/// command: `update` returns at the bound with exit 0 and the write on disk.
#[test]
fn a_kick_slower_than_its_bound_is_one_warning_and_exit_0() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    // Blocks until the test ends (the sender drops), far past the bound.
    let kick: Kick = Arc::new(move || {
        let _ = rx.lock().map(|r| r.recv_timeout(Duration::from_secs(5)));
        Ok(())
    });
    let args = [
        "--root",
        "local/owner-queue",
        "update",
        "items",
        "D04",
        "--json",
        r#"{"status": "answered"}"#,
    ];
    let start = Instant::now();
    let out = go_bounded(
        root,
        &args,
        &[],
        &root.join("x"),
        kick,
        Duration::from_millis(50),
    );
    let took = start.elapsed();
    drop(tx);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stderr,
        "questions_rc kick failed: timed out after 0.05 seconds\n"
    );
    assert!(out.stdout.contains("\"version\": 2"), "{}", out.stdout);
    assert!(took < Duration::from_secs(2), "{took:?}");
    let doc = std::fs::read_to_string(root.join("local/owner-queue/items/D04.json"))?;
    assert!(doc.contains("\"answered\""), "{doc}");
    Ok(())
}

/// A fake `datg` that prints `stdout`, `stderr` and exits `code`; each call
/// of this helper gets its own files, so several fakes can share `dir`.
fn fake_datg(
    dir: &Path,
    stdout: &str,
    stderr: &str,
    code: i32,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let n = std::fs::read_dir(dir)?.count();
    let path = dir.join(format!("datg-{n}"));
    let (out, err) = (
        dir.join(format!("out-{n}.txt")),
        dir.join(format!("err-{n}.txt")),
    );
    std::fs::write(&out, stdout)?;
    std::fs::write(&err, stderr)?;
    let script = format!(
        "#!/bin/sh\necho \"$@\" > '{}'\ncat '{}'\ncat '{}' >&2\nexit {code}\n",
        dir.join("argv.txt").display(),
        out.display(),
        err.display()
    );
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

fn footer(root: &Path, datg: &Path, env: &[(&str, &str)]) -> Outcome {
    go(
        root,
        &["--root", "local/owner-queue", "footer"],
        env,
        datg,
        no_kick(),
    )
}

/// `il_remind.footer_line` on the rows `datg` prints: five at most, notes cut
/// at 60 characters, then `+N more`; no state file is no line.
#[test]
fn reminders_line_formats_the_datg_rows_as_il_remind_does() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let bin = tempfile::tempdir()?;
    let row = |t: &str, n: i32, note: &str| {
        format!("{{\"task\": \"{t}\", \"n\": {n}, \"note\": \"{note}\"}}\n")
    };
    let long = "x".repeat(70);
    let rows: String = [
        row("s-T01", 1, &long),
        row("s-T01", 2, "b"),
        row("s-T02", 1, "c"),
        row("s-T02", 2, "d"),
        row("s-T03", 1, "e"),
        row("s-T03", 2, "f"),
        row("s-T04", 1, "g"),
    ]
    .concat();
    let datg = fake_datg(bin.path(), &rows, "", 0)?;
    // No state file: no line, and datg never runs.
    let out = footer(root, &datg, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(!bin.path().join("argv.txt").exists());
    let state = root.join("local/il/.trusty-mpm/datg");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("state.json"), "{}")?;
    let out = footer(root, &datg, &[]);
    let want = format!(
        "Reminders (ack): s-T01#1 {} \u{b7} s-T01#2 b \u{b7} s-T02#1 c \u{b7} s-T02#2 d \u{b7} s-T03#1 e \u{b7} +2 more",
        "x".repeat(60)
    );
    assert_eq!(
        out.stdout.lines().nth(1),
        Some(want.as_str()),
        "{}",
        out.stdout
    );
    let argv = std::fs::read_to_string(bin.path().join("argv.txt"))?;
    assert_eq!(
        argv.trim_end(),
        format!(
            "--project {} reminders --due --json",
            root.join("local/il").display()
        )
    );
    // `$DATG_STATE_DIR` names the state directory; an empty one there is no line.
    let other = root.join("elsewhere");
    std::fs::create_dir_all(&other)?;
    let out = footer(root, &datg, &[("DATG_STATE_DIR", "elsewhere")]);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    // `$ARCHITECT_IL_DIR` moves the ledger; nothing due prints no line.
    let none = fake_datg(bin.path(), "\n", "", 0)?;
    let out = footer(root, &none, &[]);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    let out = footer(root, &datg, &[("ARCHITECT_IL_DIR", "nowhere")]);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    Ok(())
}

/// Each failed read is one `Reminders: check failed (<Type>: <msg[:80]>)`
/// line, as the script's `except` names it; the footer still prints, exit 0.
#[test]
fn reminders_line_names_each_failure() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let state = root.join("local/il/.trusty-mpm/datg");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("state.json"), "{}")?;
    let bin = tempfile::tempdir()?;
    let long = "y".repeat(100);
    let cases = [
        (
            fake_datg(
                bin.path(),
                "",
                "datg: state.json malformed (bad shape); refusing to write\n",
                2,
            )?,
            "DatgError: state.json malformed (bad shape); refusing to write".to_owned(),
        ),
        (
            fake_datg(
                bin.path(),
                "",
                &format!("Traceback\nKeyError: '{long}'\n"),
                1,
            )?,
            format!("KeyError: '{}", "y".repeat(79)),
        ),
        (
            fake_datg(bin.path(), "{\"task\": \"s-T01\", \"n\": 1}\n", "", 0)?,
            "KeyError: 'note'".to_owned(),
        ),
        (
            fake_datg(bin.path(), "not json\n", "", 0)?,
            "JSONDecodeError: Expecting value: line 1 column 1 (char 0)".to_owned(),
        ),
        (
            fake_datg(bin.path(), "", "", 3)?,
            "DatgError: exit 3".to_owned(),
        ),
        (
            bin.path().join("no-such-datg"),
            format!(
                "FileNotFoundError: [Errno 2] No such file or directory: '{}'",
                bin.path().join("no-such-datg").display()
            )
            .chars()
            .take(80 + "FileNotFoundError: ".len())
            .collect(),
        ),
    ];
    for (datg, want) in cases {
        let out = footer(root, &datg, &[]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(
            out.stdout.lines().nth(1),
            Some(format!("Reminders: check failed ({want})").as_str()),
            "{}",
            out.stdout
        );
    }
    Ok(())
}

/// #12029 P2g: with per-PM state directories and `$TMUX_PANE` set, tmux
/// names the directory; when `datg` answers that its `state.json` is
/// missing, the footer has no reminders line, as the script's `isfile` is
/// false. Any other failure there is still the `check failed` line.
#[test]
fn reminders_line_is_absent_when_the_tmux_pm_has_no_ledger() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let sub = root.join("local/il/.trusty-mpm/datg/pm-alpha");
    std::fs::create_dir_all(&sub)?;
    std::fs::write(sub.join("state.json"), "{}")?;
    let bin = tempfile::tempdir()?;
    let missing = fake_datg(bin.path(), "", "datg: state.json missing; run init\n", 2)?;
    let pane = [("TMUX_PANE", "%9")];
    let out = footer(root, &missing, &pane);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(!out.stdout.contains("Reminders"), "{}", out.stdout);
    assert!(bin.path().join("argv.txt").exists(), "datg did not run");
    // Without the tmux pane the base directory decides: no file, no datg run.
    std::fs::remove_file(bin.path().join("argv.txt"))?;
    let out = footer(root, &missing, &[]);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(!bin.path().join("argv.txt").exists());
    // Another failure under the tmux pane still names itself.
    let other = fake_datg(bin.path(), "", "datg: state.json malformed (x)\n", 2)?;
    let out = footer(root, &other, &pane);
    assert_eq!(
        out.stdout.lines().nth(1),
        Some("Reminders: check failed (DatgError: state.json malformed (x))"),
        "{}",
        out.stdout
    );
    // A known ledger keeps the line even for a `state.json missing` answer.
    let base = root.join("local/il/.trusty-mpm/datg");
    std::fs::write(base.join("state.json"), "{}")?;
    let out = footer(root, &missing, &[]);
    assert_eq!(
        out.stdout.lines().nth(1),
        Some("Reminders: check failed (DatgError: state.json missing; run init)"),
        "{}",
        out.stdout
    );
    Ok(())
}

/// #12029 AC2: `dashboard-db` writes under the store's shared `flock` (the
/// frozen script's lock) and claims its collections for the Python writer
/// class, so a Rust-class store is refused them.
#[test]
fn dashboard_db_writes_take_the_shared_store_lock() -> R {
    let tmp = checkout()?;
    let root = tmp.path().to_path_buf();
    let store = root.join("local/owner-queue");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(".lock"))?;
    // SAFETY: the fd stays open for `held`'s life; flock touches nothing else.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let worker = {
        let root = root.clone();
        std::thread::spawn(move || {
            let args = [
                "--root",
                "local/owner-queue",
                "set",
                "items",
                "B05",
                "--json",
                r#"{"title": "b", "task": "T01"}"#,
            ];
            go(&root, &args, &[], &root.join("x"), no_kick())
        })
    };
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !store.join("items/B05.json").exists(),
        "written while the lock was held"
    );
    drop(held);
    let out = worker.join().map_err(|_| "the writer panicked")?;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(store.join("items/B05.json").exists());
    assert_eq!(claims(&store)?.get("items"), Some(&WriterClass::Python));
    let rust = crate::dashboard::store::Store::new(&store, crate::dashboard::store::Format::Dir)
        .with_secret_scan(|_| None)
        .with_writer_claim();
    let got = rust.set("items", "B06", serde_json::json!({}), None);
    assert!(
        matches!(got, Err(crate::dashboard::DashError::WriterClass(_))),
        "{got:?}"
    );
    Ok(())
}

/// A `meta/settings` the level read cannot load costs the lint, never the
/// write: one `level lint skipped` warning, then the write lands.
#[test]
fn a_level_read_failure_is_a_warning_and_the_write_lands() -> R {
    let tmp = checkout()?;
    let root = tmp.path();
    let meta = root.join("local/owner-queue/meta");
    std::fs::create_dir_all(&meta)?;
    std::fs::write(meta.join("settings.json"), "{broken")?;
    let body = r#"{"kind": "decision", "status": "open", "title": "t", "task": "T01"}"#;
    let out = go(
        root,
        &[
            "--root",
            "local/owner-queue",
            "set",
            "items",
            "D07",
            "--allow-id",
            "--json",
            body,
        ],
        &[],
        &root.join("x"),
        no_kick(),
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .starts_with("warning: level lint skipped: StoreError('unreadable document "),
        "{}",
        out.stderr
    );
    assert!(root.join("local/owner-queue/items/D07.json").exists());
    Ok(())
}

/// The legacy `files/local/briefs/<f>` specs form becomes the Architect's own
/// `#doc/` reader path; another `files/` path stays, `doc/` gains `#`. The
/// recordings use neutral paths, so this branch is pinned here (#12029 P2g).
#[test]
fn reader_path_rewrites_the_legacy_brief_form() {
    let legacy = reader_path("files/local/briefs/c.md");
    assert_eq!(
        legacy.strip_prefix("#doc/architect/"),
        Some("local/briefs/c.md")
    );
    assert_eq!(reader_path("files/sample/c.md"), "files/sample/c.md");
    assert_eq!(reader_path("doc/sample/plan.md"), "#doc/sample/plan.md");
    assert_eq!(reader_path("other/x.md"), "other/x.md");
}
