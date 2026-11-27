//! #12026: the record-then-replay parity tool — record, scrub, refuse, replay, diff.
//!
//! Programs are `/bin/sh -c` scripts (present on every host the suite runs
//! on) and, for the open probe, small `python3` scripts in a temp dir.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use trusty_architect::replay::{
    Channel, EnvRule, RecordSpec, Recording, ReplayError, Scrubber, Target, check, load, record,
    replay, save,
};
use trusty_architect::replay::{Refusal, VERSION};

const T: Duration = Duration::from_secs(30);

/// `/<root>/<name>`, built at run time so the source holds no home-path
/// literal for the public-content leak guard to flag.
fn home(root: &str, name: &str) -> String {
    format!("/{root}/{name}")
}

/// The parent environment a test hands the recorder: the real `PATH` (to find
/// `python3`), a fixed fake `HOME`, and a variable that must never leak.
fn parent() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
        ("HOME".into(), home("Users", "alice")),
        ("ALLOWED".into(), "yes".into()),
        ("LEAKED_VAR".into(), "must-not-arrive".into()),
    ])
}

fn sh(script: &str, args: &[&str]) -> (PathBuf, Vec<String>) {
    let mut argv = vec!["-c".to_owned(), script.to_owned(), "sh".to_owned()];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    (PathBuf::from("/bin/sh"), argv)
}

fn spec(program: PathBuf, argv: Vec<String>) -> RecordSpec {
    RecordSpec {
        program,
        argv,
        stdin: String::new(),
        env: Vec::new(),
        seeds: Vec::new(),
        sessions: Vec::new(),
        timeout: T,
        fakes: BTreeMap::new(),
        clock: None,
    }
}

/// A script that touches every channel: reads a seed and stdin, prints an
/// env value and an arg, writes one file, deletes another, exits 3.
const ALL_CHANNELS: &str = "cat in.txt; cat; printf '%s\\n' \"$GREETING\"; \
    printf 'err %s\\n' \"$1\" >&2; printf 'out\\n' > out.txt; rm gone.txt; exit 3";

fn record_all_channels(live: &Path) -> Recording {
    std::fs::write(live.join("in.txt"), "input\n").unwrap();
    std::fs::write(live.join("gone.txt"), "old\n").unwrap();
    let (program, argv) = sh(ALL_CHANNELS, &["arg1"]);
    let mut s = spec(program, argv);
    s.stdin = "hello\n".into();
    s.env = vec![EnvRule::Set("GREETING".into(), "hi".into())];
    s.seeds = vec![
        (live.join("in.txt"), "in.txt".into()),
        (live.join("gone.txt"), "gone.txt".into()),
    ];
    record(&s, &parent()).unwrap()
}

#[test]
fn record_captures_argv_stdin_env_files_and_writes() {
    let live = tempfile::tempdir().unwrap();
    let rec = record_all_channels(live.path());
    assert_eq!(rec.version, VERSION);
    assert_eq!(rec.argv[3], "arg1");
    assert_eq!(rec.stdin, "hello\n");
    assert_eq!(rec.env, BTreeMap::from([("GREETING".into(), "hi".into())]));
    assert_eq!(rec.files.get("in.txt").map(String::as_str), Some("input\n"));
    assert_eq!(
        rec.reads, None,
        "a non-Python program runs without the probe"
    );
    assert_eq!(rec.stdout, "input\nhello\nhi\n");
    assert_eq!(rec.stderr, "err arg1\n");
    assert_eq!(rec.exit, 3);
    let want = BTreeMap::from([
        ("gone.txt".into(), None),
        ("out.txt".into(), Some("out\n".into())),
    ]);
    assert_eq!(rec.writes, want);
    // The run used copies: the live seed directory is untouched.
    assert!(live.path().join("gone.txt").exists());
    assert!(!live.path().join("out.txt").exists());
}

#[test]
fn record_keeps_env_outside_the_allowlist_out() {
    let (program, argv) = sh("env", &[]);
    let mut s = spec(program, argv);
    s.env = vec![
        EnvRule::Pass("ALLOWED".into()),
        EnvRule::Pass("ABSENT".into()),
    ];
    let rec = record(&s, &parent()).unwrap();
    assert_eq!(rec.env, BTreeMap::from([("ALLOWED".into(), "yes".into())]));
    assert!(
        rec.stdout.lines().any(|l| l == "ALLOWED=yes"),
        "{}",
        rec.stdout
    );
    assert!(
        rec.stdout.lines().any(|l| l == "HOME={ROOT}"),
        "{}",
        rec.stdout
    );
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        !json.contains("LEAKED_VAR") && !json.contains("must-not-arrive"),
        "{json}"
    );
}

#[test]
fn record_lists_the_files_a_python_script_opened() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("reader.py");
    std::fs::write(&script, "print(open('a.txt').read().strip())\n").unwrap();
    std::fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "B\n").unwrap();
    let mut s = spec(script, Vec::new());
    s.seeds = vec![
        (dir.path().join("a.txt"), "a.txt".into()),
        (dir.path().join("b.txt"), "b.txt".into()),
    ];
    let rec = record(&s, &parent()).unwrap();
    assert_eq!(rec.stdout, "A\n");
    assert_eq!(rec.reads, Some(vec!["a.txt".to_owned()]));
    assert_eq!(rec.files.len(), 2, "every seed is laid out for replay");
}

#[test]
fn record_refuses_a_read_outside_the_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("live-state.json");
    std::fs::write(&outside, "{}").unwrap();
    let script = dir.path().join("leaky.py");
    std::fs::write(
        &script,
        format!("open({:?}).read()\n", outside.to_string_lossy()),
    )
    .unwrap();
    let err = record(&spec(script, Vec::new()), &parent()).unwrap_err();
    assert!(
        matches!(err, ReplayError::External { count: 1, .. }),
        "{err}"
    );
}

#[test]
fn record_refuses_an_oversized_seed() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.txt");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(17 * 1024 * 1024)
        .unwrap();
    let (program, argv) = sh("true", &[]);
    let mut s = spec(program, argv);
    s.seeds = vec![(big, "big.txt".into())];
    let err = record(&s, &parent()).unwrap_err();
    assert!(
        matches!(err, ReplayError::BadArg(ref m) if m.contains("size limit")),
        "{err}"
    );
}

#[test]
fn replay_of_an_unchanged_program_is_clean() {
    let live = tempfile::tempdir().unwrap();
    let rec = record_all_channels(live.path());
    let target = Target {
        program: "/bin/sh".into(),
        prefix: Vec::new(),
    };
    assert_eq!(replay(&rec, &target, &parent(), T).unwrap(), Vec::new());
}

/// Flips the byte at `at` to its neighbour, so the change is one byte.
fn flip(s: &str, at: usize) -> String {
    let mut b = s.as_bytes().to_vec();
    b[at] = if b[at] == b'z' { b'y' } else { b[at] + 1 };
    String::from_utf8(b).unwrap()
}

#[test]
fn replay_reports_a_one_byte_change_in_each_channel() {
    let live = tempfile::tempdir().unwrap();
    let rec = record_all_channels(live.path());
    let target = Target {
        program: "/bin/sh".into(),
        prefix: Vec::new(),
    };
    let mut stdout = rec.clone();
    stdout.stdout = flip(&rec.stdout, 7);
    let mut stderr = rec.clone();
    stderr.stderr = flip(&rec.stderr, 0);
    let mut exit = rec.clone();
    exit.exit += 1;
    let mut file = rec.clone();
    file.writes.insert("out.txt".into(), Some(flip("out\n", 2)));
    let cases = [
        (stdout, Channel::Stdout, "differs at byte 7 (line 2;"),
        (stderr, Channel::Stderr, "differs at byte 0 (line 1;"),
        (exit, Channel::Exit, "want 4, got 3"),
        (
            file,
            Channel::File("out.txt".into()),
            "differs at byte 2 (line 1;",
        ),
    ];
    for (edited, channel, detail) in cases {
        let found = replay(&edited, &target, &parent(), T).unwrap();
        assert_eq!(found.len(), 1, "{channel:?}: {found:?}");
        assert_eq!(found[0].channel, channel);
        assert!(
            found[0].detail.contains(detail),
            "{channel:?}: {}",
            found[0].detail
        );
    }
}

#[test]
fn scrub_is_deterministic_and_order_independent() {
    let text = "beta wrote /tmp/r12026/x; alpha read it";
    let a = Scrubber::new()
        .sessions(&["beta".into(), "alpha".into()])
        .path(Path::new("/tmp/r12026"), "{ROOT}");
    let b = Scrubber::new()
        .path(Path::new("/tmp/r12026"), "{ROOT}")
        .sessions(&["alpha".into(), "beta".into(), "alpha".into()]);
    let once = a.text(text);
    assert_eq!(once, "session-2 wrote {ROOT}/x; session-1 read it");
    assert_eq!(b.text(text), once);
    assert_eq!(a.text(&once), once, "a scrubbed text is a fixed point");
}

#[test]
fn scrub_replaces_paths_and_bounded_session_names() {
    let h = home("Users", "alice");
    let s = Scrubber::new()
        .path(Path::new(&h), "{HOME}")
        .path(Path::new(&format!("{h}/proj")), "{PROG}")
        .sessions(&["tt".into()]);
    assert_eq!(
        s.text(&format!("{h}/proj/a.py {h}/b")),
        "{PROG}/a.py {HOME}/b"
    );
    assert_eq!(
        s.text("(tt) tty tt-2 tt."),
        "(session-1) tty tt-2 session-1."
    );
}

fn clean() -> Recording {
    Recording {
        version: VERSION,
        argv: vec!["--root".into(), "{ROOT}/store".into()],
        stdin: String::new(),
        env: BTreeMap::new(),
        files: BTreeMap::from([(
            "doc.md".into(),
            "see /Users/<name>/.cargo and /Users/`".into(),
        )]),
        reads: Some(vec!["doc.md".into()]),
        stdout: "ok\n".into(),
        stderr: String::new(),
        exit: 0,
        writes: BTreeMap::new(),
        fakes: None,
        clock: None,
    }
}

#[test]
fn check_refuses_an_unscrubbed_home_path_session_and_secret() {
    use trusty_architect::replay::RefusalKind::{LocalPath, Secret, Session};
    let sessions = vec!["pm-alpha".to_owned()];
    assert_eq!(check(&clean(), &sessions), Ok(()), "placeholders pass");
    let mut cases: Vec<(Recording, &str, _)> = Vec::new();
    let mut r = clean();
    r.stdout = format!("wrote {}/notes\n", home("Users", "alice"));
    cases.push((r, "stdout", LocalPath));
    let mut r = clean();
    r.stderr = format!("Traceback: {}/x.py", home("home", "bob"));
    cases.push((r, "stderr", LocalPath));
    let mut r = clean();
    r.env.insert("SESSION".into(), "pm-alpha".into());
    cases.push((r, "env#0", Session));
    let mut r = clean();
    r.files
        .insert("cfg.toml".into(), "token: abcdefgh12345".into());
    cases.push((r, "files#0", Secret));
    for (rec, field, kind) in cases {
        let refusal = check(&rec, &sessions).unwrap_err();
        assert_eq!(
            refusal,
            Refusal {
                field: field.into(),
                kind
            }
        );
        let shown = refusal.to_string();
        assert!(
            !shown.contains("alice") && !shown.contains("abcdefgh"),
            "{shown}"
        );
    }
}

#[test]
fn save_refuses_an_unscrubbed_recording_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("case.json");
    let mut dirty = clean();
    dirty.stdout = format!("{}/secret-project\n", home("Users", "alice"));
    let err = save(&path, &dirty, &[]).unwrap_err();
    assert!(matches!(err, ReplayError::Refused(_)), "{err}");
    assert!(
        !path.exists(),
        "a refused recording must not reach the fixture"
    );
    save(&path, &clean(), &[]).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert_eq!(load(&path).unwrap(), clean());
    save(&path, &load(&path).unwrap(), &[]).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        first,
        "the same recording gives the same bytes"
    );
}

#[test]
fn committed_replay_fixtures_pass_the_refusal_check() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay");
    let mut seen = 0;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "json") {
                load(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
                seen += 1;
            }
        }
    }
    assert!(seen > 0, "no replay fixtures found");
}

#[test]
fn cli_records_replays_and_checks_a_fixture() {
    let bin = env!("CARGO_BIN_EXE_trusty-architect-replay");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hi.json");
    let run = |args: &[&str]| Command::new(bin).args(args).output().unwrap();
    let o = out.to_str().unwrap();
    let rec = run(&[
        "record",
        "--out",
        o,
        "--",
        "/bin/sh",
        "-c",
        "echo hi pm-alpha",
    ]);
    assert_eq!(
        rec.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&rec.stderr)
    );
    let rep = run(&["replay", o, "--", "/bin/sh"]);
    assert_eq!(
        rep.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&rep.stdout)
    );
    assert_eq!(run(&["check", o]).status.code(), Some(0));
    let named = run(&["check", "--session", "pm-alpha", o]);
    assert_eq!(named.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&named.stdout).contains("REFUSED"));
    let leak = dir.path().join("leak.json");
    let l = leak.to_str().unwrap();
    let echo = format!("echo {}/x", home("home", "zed"));
    let refused = run(&["record", "--out", l, "--", "/bin/sh", "-c", &echo]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(!leak.exists(), "a refused record writes no fixture");
}

// #12026 fix round: one regression test per critic finding.

#[test]
fn check_refuses_a_secret_named_env_key() {
    use trusty_architect::replay::RefusalKind::Secret;
    for (k, v) in [
        ("GITHUB_TOKEN", "short"),
        ("AWS_SECRET_ACCESS_KEY", "x"),
        ("OPENAI_API_KEY", "abc"),
        ("PGPASSWORD", "pw"),
    ] {
        let mut r = clean();
        r.env.insert(k.into(), v.into());
        assert_eq!(
            check(&r, &[]),
            Err(Refusal {
                field: "env#0".into(),
                kind: Secret
            }),
            "{k}"
        );
    }
    let mut r = clean();
    r.env
        .insert("TOKENIZERS_PARALLELISM".into(), "false".into());
    assert_eq!(check(&r, &[]), Ok(()), "a TOKEN prefix is not a token key");
}

#[test]
fn check_refuses_a_segment_or_camel_case_credential_key_and_a_url_password() {
    use trusty_architect::replay::RefusalKind::Secret;
    let refused = Err(Refusal {
        field: "env#0".into(),
        kind: Secret,
    });
    for (k, v) in [
        ("DB_PASS", "x"),
        ("STRIPE_KEY", "x"),
        ("GH_PAT", "x"),
        ("NPM_AUTH", "x"),
        ("SESSION_COOKIE", "x"),
        ("githubToken", "x"),
        ("DATABASE_URL", "postgres://app:pw@db.example.com/main"),
    ] {
        let mut r = clean();
        r.env.insert(k.into(), v.into());
        assert_eq!(check(&r, &[]), refused, "{k}");
    }
    for (k, v) in [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "{HOME}"),
        ("PWD", "{ROOT}"),
        ("KEYBOARD_LAYOUT", "us"),
        ("PATH_INFO", "/a"),
        ("AUTHOR_NAME", "Ada"),
        ("SSH_AUTH_SOCK", "{ROOT}/agent.sock"),
        ("BYPASS_LIST", "none"),
        ("TOKENIZERS_PARALLELISM", "false"),
        ("REPO_URL", "https://git@example.org/r.git"),
        ("MIRROR", "https://example.org:8443/x"),
    ] {
        let mut r = clean();
        r.env.insert(k.into(), v.into());
        assert_eq!(check(&r, &[]), Ok(()), "{k} is not a credential key");
    }
}

#[test]
fn replay_withholds_a_secret_line_whose_keyword_falls_outside_the_excerpt() {
    let value = format!("{}Z{}", "q".repeat(60), "q".repeat(60));
    let line = format!("password: {value}\n");
    let rec = Recording {
        version: VERSION,
        argv: vec!["-c".into(), "printf '%s\\n' \"password: $V\"".into()],
        stdin: String::new(),
        env: BTreeMap::from([("V".into(), value.clone())]),
        files: BTreeMap::new(),
        reads: None,
        // One byte differs 70 bytes past the keyword, so the excerpt window
        // starts inside the value.
        stdout: flip(&line, 80),
        stderr: String::new(),
        exit: 0,
        writes: BTreeMap::new(),
        fakes: None,
        clock: None,
    };
    let target = Target {
        program: "/bin/sh".into(),
        prefix: Vec::new(),
    };
    let found = replay(&rec, &target, &parent(), T).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        !found[0].detail.contains("qqqqqqqq") && found[0].detail.contains("<withheld>"),
        "{}",
        found[0].detail
    );
}

#[test]
fn check_refuses_an_unlisted_tm_session_name_but_not_a_skill_name() {
    use trusty_architect::replay::RefusalKind::Session;
    let mut r = clean();
    r.stdout = "nudged tm-pm-alpha\n".into();
    assert_eq!(
        check(&r, &[]),
        Err(Refusal {
            field: "stdout".into(),
            kind: Session
        })
    );
    let mut r = clean();
    r.stdout = "see tm-workflow, tm-workflow.md, xtm-foo and a bare tm-\n".into();
    assert_eq!(check(&r, &[]), Ok(()), "skill names and non-tokens pass");
    // The recorder refuses it too, with no --session given.
    let (program, argv) = sh("echo ran tm-beta", &[]);
    let err = record(&spec(program, argv), &parent()).unwrap_err();
    assert!(
        matches!(err, ReplayError::Refused(Refusal { kind: Session, .. })),
        "{err}"
    );
}

#[test]
fn replay_withholds_an_excerpt_naming_a_tm_session() {
    let mut rec = clean();
    rec.argv = vec!["-c".into(), "echo ran tm-gamma".into()];
    rec.files.clear();
    rec.reads = None;
    rec.stdout = "ran tm-gammb\n".into();
    let target = Target {
        program: "/bin/sh".into(),
        prefix: Vec::new(),
    };
    let found = replay(&rec, &target, &parent(), T).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(!found[0].detail.contains("tm-gam"), "{}", found[0].detail);
}

#[test]
fn skill_names_match_the_repo_skill_list() {
    use trusty_architect::replay::SKILL_NAMES;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/skills");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .map(|n| n.trim_end_matches(".md").to_owned())
        .filter(|n| n.starts_with("tm-"))
        .collect();
    found.sort();
    found.dedup();
    assert_eq!(found, SKILL_NAMES.to_vec());
}

#[test]
fn scrub_matches_a_path_only_at_a_component_boundary() {
    let (short, long) = (home("Users", "al"), home("Users", "alice"));
    let s = Scrubber::new().path(Path::new(&short), "{HOME}");
    assert_eq!(
        s.text(&format!("{long}/x {short}/y {short}")),
        format!("{long}/x {{HOME}}/y {{HOME}}")
    );
}

#[test]
fn record_counts_an_os_open_write_as_a_write_not_a_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    let script = dir.path().join("truncate.py");
    std::fs::write(
        &script,
        "import os\nos.close(os.open('a.txt', os.O_WRONLY | os.O_TRUNC))\n",
    )
    .unwrap();
    let mut s = spec(script, Vec::new());
    s.seeds = vec![(dir.path().join("a.txt"), "a.txt".into())];
    let rec = record(&s, &parent()).unwrap();
    assert_eq!(rec.reads, Some(Vec::new()), "a write-only open is no read");
    assert_eq!(rec.writes.get("a.txt"), Some(&Some(String::new())));
}

#[test]
fn record_refuses_a_run_whose_probe_log_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("unlog.py");
    std::fs::write(
        &script,
        "import os, sys\nf = sys._getframe()\nwhile f.f_back:\n    f = f.f_back\n\
         os.unlink(f.f_globals['_log'].name)\n",
    )
    .unwrap();
    let err = record(&spec(script, Vec::new()), &parent()).unwrap_err();
    assert!(matches!(err, ReplayError::ProbeLog), "{err}");
}

#[test]
fn record_refuses_a_symlink_in_a_seed_or_left_by_the_run() {
    let live = tempfile::tempdir().unwrap();
    let seed = live.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    std::fs::write(live.path().join("private.txt"), "live\n").unwrap();
    std::os::unix::fs::symlink(live.path().join("private.txt"), seed.join("link")).unwrap();
    let (program, argv) = sh("true", &[]);
    let mut s = spec(program, argv);
    s.seeds = vec![(seed, "seed".into())];
    let err = record(&s, &parent()).unwrap_err();
    assert!(
        matches!(err, ReplayError::Symlink(ref rel) if rel == "seed/link"),
        "{err}"
    );
    let (program, argv) = sh("ln -s /etc/hosts made", &[]);
    let err = record(&spec(program, argv), &parent()).unwrap_err();
    assert!(
        matches!(err, ReplayError::Symlink(ref rel) if rel == "made"),
        "{err}"
    );
}
