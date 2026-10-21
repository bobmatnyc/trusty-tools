//! `tm hook --egress-guard`'s CTO tripwire through the real binary (#8454 G3,
//! slice S6 prep).
//!
//! Why: a Slack-tainted session must not reach a configured project's files.
//! The blanket `egress-channel-tainted` deny already refuses its gated calls;
//! the tripwire names a call that lands in a `HostChannel::projects()` dir with
//! its own slug, `egress-channel-sensitive-path`, and appends a durable JSONL
//! record, so the operator can see a trip. Those claims are only true of the
//! binary as Claude Code runs it, over a policy that is ready.
//! What: a canonical temp root holds `home` (the host `config.yaml`, the taint
//! files, the tripwire record), `proj` (a listed project: a git repo with its
//! route file committed on `main`, so the inline policy load is ready), `out`
//! (a directory no project covers), `ws` (the taint mark's workspace) and one
//! data directory per run. Each case pipes one `PreToolUse` payload into
//! `tm hook --egress-guard` and reads the verdict, the deny audit slug and the
//! record back. The S4-critic MEDIUM (#9741) fixture is
//! `tripwire_keeps_the_blanket_taint_deny_for_non_sensitive_calls`: a ready
//! policy, through the binary, still denies a non-sensitive tainted call as
//! `egress-channel-tainted`.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_tripwire_8454::`;
//! `scripts/check_tripwire_gate.sh` runs these by name.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};
use trusty_mpm::daemon::bug_report::{aggregate_errors_from_paths, store_paths_under};

/// Nothing listens on port 1: no daemon verdict, so the policy loads inline.
const LOOPBACK: &str = "http://127.0.0.1:1";
/// The channel-tainted session.
const TAINTED: &str = "s-trip-8454";
/// A session with no taint file that no mark covers.
const CLEAN: &str = "s-clean-8454";
/// The tripwire's deny slug.
const SENSITIVE: &str = "egress-channel-sensitive-path";
/// The blanket taint deny slug.
const TAINT: &str = "egress-channel-tainted";

/// One temp root per test; see the module doc.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    out: PathBuf,
    runs: AtomicUsize,
}

/// A guard run's verdict: the printed deny reason and its audit slug, or an
/// allow (nothing printed).
#[derive(Debug)]
enum Seen {
    Allow,
    Deny { check: String, reason: String },
}

impl Fixture {
    /// A ready policy listing `proj`, with [`TAINTED`] tainted under `home`.
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let (home, proj, out, ws) = (
            root.join("home"),
            root.join("proj"),
            root.join("out"),
            root.join("ws"),
        );
        for dir in [&home, &proj, &out, &ws] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(proj.join("secret.txt"), "s3cret\n").expect("write secret");
        std::fs::write(out.join("notes.txt"), "plain\n").expect("write notes");
        committed_routes(&proj, "bob-dm", "U0ABCDEF1");
        let fx = Self {
            _tmp: tmp,
            root,
            home,
            proj,
            out,
            runs: AtomicUsize::new(0),
        };
        fx.list(&[&fx.proj]);
        let mark = TaintMark {
            channel: "slack".into(),
            route: "owner-dm".into(),
            question_id: "Q-8454".into(),
            tainted_at: chrono::Utc::now(),
            // Scoped, so a session launched in `out` stays untainted (#9923).
            workspace: ws,
        };
        taint_managed_session(&taint_dir(&fx.home), "m-8454", TAINTED, &mark).expect("taint");
        fx
    }

    /// Rewrite the host ceiling so Slack and Telegram list exactly `dirs`.
    fn list(&self, dirs: &[&Path]) {
        let list: Vec<String> = dirs
            .iter()
            .map(|d| format!("\"{}\"", d.display()))
            .collect();
        let list = list.join(", ");
        let text = format!(
            "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: \
             slack, app_ref: slack-app }}\n    projects: [{list}]\n  telegram:\n    enabled: \
             true\n    connection: {{ bot_ref: telegram }}\n    projects: [{list}]\n"
        );
        let path = trusty_common::crate_config::crate_config_path_at(&self.home, "trusty-mpm");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir host dir");
        std::fs::write(path, text).expect("write config.yaml");
    }

    /// The tripwire record file the guard appends to.
    fn record(&self) -> PathBuf {
        trusty_common::crate_config::crate_config_dir_at(&self.home, "trusty-mpm")
            .join("channels")
            .join("tripwire")
            .join("trips.jsonl")
    }

    /// The record's lines, each parsed as JSON; empty when the file is absent.
    fn trips(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.record()) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).expect("a trip line is JSON"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("read the tripwire record: {e}"),
        }
    }

    /// Run the guard on `call` for `session`, launched in `out`, with `env`.
    fn guard(&self, session: &str, call: Value, env: &[(&str, &str)]) -> Seen {
        let mut payload = call;
        payload["session_id"] = json!(session);
        payload["hook_event_name"] = json!("PreToolUse");
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        let data = self.root.join(format!("data-{n}"));
        std::fs::create_dir_all(&data).expect("mkdir data");
        let mut cmd = common::tm_command_in(&self.home);
        cmd.args(["--url", LOOPBACK, "hook", "--egress-guard"])
            .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
            .env_remove("TRUSTY_NO_BUG_CAPTURE")
            .env("CLAUDE_PROJECT_DIR", &self.out)
            .current_dir(&self.out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in [
            "TRUSTY_MPM_DISABLE_HOOKS",
            "TRUSTY_MPM_PM_UNRESTRICTED",
            "CLAUDE_MPM_SUB_AGENT",
        ] {
            cmd.env_remove(key);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn tm hook --egress-guard");
        let mut pipe = child.stdin.take().expect("child stdin");
        let _ = pipe.write_all(payload.to_string().as_bytes());
        drop(pipe);
        let out = child.wait_with_output().expect("wait for the guard");
        seen(&out, &data, session)
    }

    /// [`Self::guard`] for [`TAINTED`], no extra env.
    fn tainted(&self, call: Value) -> Seen {
        self.guard(TAINTED, call, &[])
    }
}

/// Read a guard run: exit 0 always; one deny object, or nothing.
fn seen(out: &Output, data: &Path, session: &str) -> Seen {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the guard exits 0, got {:?}; stderr={stderr}",
        out.status
    );
    let line = stdout.trim();
    if line.is_empty() {
        return Seen::Allow;
    }
    let value: Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}): {stdout:?}"));
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{value}"
    );
    let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let errors = aggregate_errors_from_paths(&store_paths_under(data), 100);
    let check = errors
        .iter()
        .find(|e| e.record.fields.contains(&format!("session={session}")))
        .and_then(|e| {
            e.record
                .fields
                .split_whitespace()
                .find_map(|f| f.strip_prefix("check="))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("no deny audit record for {session}: {errors:#?}"));
    Seen::Deny { check, reason }
}

/// Assert `seen` is a deny audited as `slug`.
fn assert_slug(seen: &Seen, slug: &str, case: &str) {
    match seen {
        Seen::Deny { check, reason } => {
            assert_eq!(check, slug, "{case}: {reason}");
            assert!(
                reason.contains("#8454") || reason.contains("#8433"),
                "{case}: {reason}"
            );
        }
        Seen::Allow => panic!("{case}: a tainted gated call was allowed"),
    }
}

/// Commit a one-route `.trusty-channels/routes.toml` on `main` in `dir`.
fn committed_routes(dir: &Path, name: &str, recipient: &str) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
    std::fs::create_dir_all(dir.join(".trusty-channels")).expect("mkdir");
    let routes = format!(
        "version = 2\n\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\n\
         kinds = [\"question\"]\n"
    );
    std::fs::write(dir.join(".trusty-channels/routes.toml"), routes).expect("write routes");
    git(dir, &["add", ".trusty-channels/routes.toml"]);
    git(dir, &["commit", "-q", "-m", "routes"]);
}

/// Run git in `dir` with every `GIT_*` variable cleared and a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = trusty_common::git::command();
    cmd.current_dir(dir).args([
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A `Read` of `path`, with the payload's `cwd` at `cwd`.
fn read_at(path: &Path, cwd: &Path) -> Value {
    json!({
        "tool_name": "Read",
        "tool_input": {"file_path": path.display().to_string()},
        "cwd": cwd.display().to_string(),
    })
}

/// A `Bash` call of `command` with the payload's `cwd` at `cwd`.
fn bash_at(command: &str, cwd: &Path) -> Value {
    json!({
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": cwd.display().to_string(),
    })
}

/// 🔴 #8454 S6: every gated tool whose path lands in a listed project denies
/// as `egress-channel-sensitive-path` and appends one durable record line.
#[test]
fn tripwire_blocks_tainted_session_read_of_sensitive_path() {
    let fx = Fixture::new();
    let secret = fx.proj.join("secret.txt").display().to_string();
    let proj = fx.proj.display().to_string();
    let out = fx.out.display().to_string();
    let cases = [
        ("Read", json!({"file_path": secret})),
        (
            "Edit",
            json!({"file_path": secret, "old_string": "s3", "new_string": "x"}),
        ),
        (
            "MultiEdit",
            json!({"file_path": secret, "edits": [{"old_string": "s3", "new_string": "x"}]}),
        ),
        ("Write", json!({"file_path": secret, "content": "x"})),
        (
            "NotebookEdit",
            json!({"notebook_path": format!("{proj}/nb.ipynb"), "new_source": "x"}),
        ),
        ("Glob", json!({"pattern": "**/*.txt", "path": proj})),
        ("Glob", json!({"pattern": format!("{proj}/**/*.txt")})),
        ("Grep", json!({"pattern": "s3cret", "path": proj})),
        ("Grep", json!({"pattern": secret})),
        ("Bash", json!({"command": format!("cat {secret}")})),
        (
            "Bash",
            json!({"command": format!("echo x > {proj}/new.txt")}),
        ),
    ];
    for (n, (tool, input)) in cases.iter().enumerate() {
        let call = json!({"tool_name": tool, "tool_input": input, "cwd": out});
        let case = format!("{tool} {input}");
        assert_slug(&fx.tainted(call), SENSITIVE, &case);
        let trips = fx.trips();
        assert_eq!(trips.len(), n + 1, "{case}: one record line per trip");
        let last = trips.last().expect("a trip line");
        assert_eq!(last["session_id"], TAINTED, "{case}: {last}");
        assert_eq!(last["tool_name"], *tool, "{case}: {last}");
        assert!(last["at"].is_string(), "{case}: {last}");
    }
}

/// 🔴 #8454 S6 (Architect Q2, Q5): a tainted call that lands outside every
/// listed project keeps the blanket `egress-channel-tainted` deny on a ready,
/// configured host, and writes no trip.
#[test]
fn tripwire_keeps_the_blanket_taint_deny_for_non_sensitive_calls() {
    let fx = Fixture::new();
    let notes = fx.out.join("notes.txt");
    assert_slug(&fx.tainted(read_at(&notes, &fx.out)), TAINT, "Read outside");
    assert_slug(
        &fx.tainted(bash_at("ls -la notes.txt", &fx.out)),
        TAINT,
        "Bash outside",
    );
    // The positive control: the same fixture trips on a project path.
    let secret = fx.proj.join("secret.txt");
    assert_slug(&fx.tainted(read_at(&secret, &fx.out)), SENSITIVE, "control");
    assert_eq!(fx.trips().len(), 1, "only the project read trips");
}

/// 🔴 #8454 S6: an untainted session is not judged: a project read allows and
/// writes no record, while the tainted session in the same home trips.
#[test]
fn tripwire_ignores_untainted_session() {
    let fx = Fixture::new();
    let secret = fx.proj.join("secret.txt");
    let clean = fx.guard(CLEAN, read_at(&secret, &fx.out), &[]);
    assert!(matches!(clean, Seen::Allow), "untainted: {clean:?}");
    assert!(fx.trips().is_empty(), "an untainted call writes no trip");
    assert_slug(&fx.tainted(read_at(&secret, &fx.out)), SENSITIVE, "tainted");
    assert_eq!(fx.trips().len(), 1);
}

/// 🔴 #8454 S6: neither pm-guard bypass variable nor a subagent payload lifts
/// the tripwire; the arm reads none of them.
#[test]
fn tripwire_holds_under_both_bypass_env_vars_and_subagent() {
    let fx = Fixture::new();
    let secret = fx.proj.join("secret.txt");
    let both = [
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("CLAUDE_MPM_SUB_AGENT", "1"),
    ];
    let seen = fx.guard(TAINTED, read_at(&secret, &fx.out), &both);
    assert_slug(&seen, SENSITIVE, "both bypasses");
    let mut sub = read_at(&secret, &fx.out);
    sub["agent_id"] = json!("agent-1");
    sub["agent_type"] = json!("engineer");
    assert_slug(&fx.guard(TAINTED, sub, &both), SENSITIVE, "subagent");
    assert_eq!(fx.trips().len(), 2);
}

/// 🔴 #8454 S6: a malformed or unreadable taint file is tainted, so its
/// session's project read trips; only an absent file is untainted.
#[test]
fn tripwire_denies_on_unreadable_taint_file() {
    let fx = Fixture::new();
    let dir = taint_dir(&fx.home);
    std::fs::write(dir.join("s-malformed.json"), b"{not json").expect("write malformed");
    std::fs::create_dir_all(dir.join("s-unreadable.json")).expect("a dir at the taint file");
    let secret = fx.proj.join("secret.txt");
    for session in ["s-malformed", "s-unreadable"] {
        let seen = fx.guard(session, read_at(&secret, &fx.out), &[]);
        assert_slug(&seen, SENSITIVE, session);
        let outside = fx.guard(session, read_at(&fx.out.join("notes.txt"), &fx.out), &[]);
        assert_slug(&outside, TAINT, &format!("{session}, outside"));
    }
    assert_eq!(fx.trips().len(), 2);
}

/// 🔴 #8454 S6 (plan §5, box 10): a project added to the host config is
/// sensitive at once, with no code change.
#[test]
fn tripwire_new_project_needs_no_code_change() {
    let fx = Fixture::new();
    let added = fx.root.join("added");
    std::fs::create_dir_all(&added).expect("mkdir added");
    committed_routes(&added, "carol-dm", "U0ABCDEF2");
    let file = added.join("plan.md");
    std::fs::write(&file, "plan\n").expect("write plan");
    assert_slug(
        &fx.tainted(read_at(&file, &fx.out)),
        TAINT,
        "before listing",
    );
    fx.list(&[&fx.proj, &added]);
    assert_slug(
        &fx.tainted(read_at(&file, &fx.out)),
        SENSITIVE,
        "after listing",
    );
    assert_eq!(fx.trips().len(), 1);
}

/// 🔴 #8454 S6: a symlink into a project, as a directory or as a file, is
/// judged by where it points.
#[test]
fn tripwire_follows_symlink_to_sensitive_path() {
    let fx = Fixture::new();
    let dir_link = fx.out.join("proj-link");
    let file_link = fx.out.join("secret-link.txt");
    std::os::unix::fs::symlink(&fx.proj, &dir_link).expect("dir symlink");
    std::os::unix::fs::symlink(fx.proj.join("secret.txt"), &file_link).expect("file symlink");
    for path in [dir_link.join("secret.txt"), file_link.clone()] {
        let case = path.display().to_string();
        assert_slug(&fx.tainted(read_at(&path, &fx.out)), SENSITIVE, &case);
    }
    let cat = format!("cat {}", file_link.display());
    assert_slug(
        &fx.tainted(bash_at(&cat, &fx.out)),
        SENSITIVE,
        "Bash via link",
    );
    assert_eq!(fx.trips().len(), 3);
}

/// 🔴 #8454 S6: a path the guard must place — relative, through `..`, in
/// another case, with no cwd, or a Bash write it cannot place — denies as
/// sensitive, never allows.
#[test]
fn tripwire_denies_unplaceable_bash_path_when_tainted() {
    let fx = Fixture::new();
    let upper = PathBuf::from(fx.proj.display().to_string().to_uppercase()).join("secret.txt");
    let relative = json!({"tool_name": "Read", "tool_input": {"file_path": "secret.txt"}});
    let cases = [
        (
            "relative to a project cwd",
            read_at(Path::new("secret.txt"), &fx.proj),
        ),
        (
            "dot-dot",
            read_at(&fx.out.join("../proj/secret.txt"), &fx.out),
        ),
        (
            "relative dot-dot",
            read_at(Path::new("../proj/secret.txt"), &fx.out),
        ),
        ("APFS case variant", read_at(&upper, &fx.out)),
        ("missing cwd", relative),
        ("Bash in a project cwd", bash_at("cat secret.txt", &fx.proj)),
        ("Bash dot-dot", bash_at("cat ../proj/secret.txt", &fx.out)),
        (
            "Bash unplaceable write",
            bash_at("echo x > \"$TARGET/out.txt\"", &fx.out),
        ),
        (
            "Bash write after cd",
            bash_at("cd \"$D\" && echo x > out.txt", &fx.out),
        ),
        (
            "Bash no cwd",
            json!({"tool_name": "Bash", "tool_input": {"command": "cat secret.txt"}}),
        ),
    ];
    for (n, (case, call)) in cases.into_iter().enumerate() {
        assert_slug(&fx.tainted(call), SENSITIVE, case);
        assert_eq!(fx.trips().len(), n + 1, "{case}: one record line");
    }
}

/// 🔴 #8454 S6 (fail closed): a record that cannot be written leaves the
/// verdict a sensitive-path deny.
#[test]
fn tripwire_record_failure_keeps_the_deny() {
    let fx = Fixture::new();
    let dir = fx.record().parent().expect("record dir").to_path_buf();
    std::fs::create_dir_all(dir.parent().expect("channels dir")).expect("mkdir channels");
    // A file where the record directory belongs: no record can be created.
    std::fs::write(&dir, b"blocked").expect("block the record dir");
    let secret = fx.proj.join("secret.txt");
    assert_slug(&fx.tainted(read_at(&secret, &fx.out)), SENSITIVE, "blocked");
    assert!(dir.is_file(), "the guard must not replace the blocker");
}

/// 🔴 #8454 S6: a policy that is not ready keeps its own slug on a sensitive
/// call, ahead of the tripwire.
#[test]
fn tripwire_policy_not_ready_stays_not_ready() {
    let fx = Fixture::new();
    let host = trusty_common::crate_config::crate_config_path_at(&fx.home, "trusty-mpm");
    std::fs::remove_file(host).expect("remove config.yaml");
    let secret = fx.proj.join("secret.txt");
    let seen = fx.tainted(read_at(&secret, &fx.out));
    assert_slug(&seen, "egress-policy-not-ready", "config missing");
    assert!(fx.trips().is_empty(), "a not-ready deny writes no trip");
    // The positive control: with the host back, the same call trips.
    fx.list(&[&fx.proj]);
    assert_slug(
        &fx.tainted(read_at(&secret, &fx.out)),
        SENSITIVE,
        "ready again",
    );
}
