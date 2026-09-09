//! The bundled `prepare-commit-msg` hook never blocks and never delays a
//! commit (#7074, round-2 review).
//!
//! Why: `git` blocks on this hook, so two things have to hold no matter what
//! the stamper does. It must exit 0 on every path — a non-zero exit aborts the
//! operator's commit — and it must give up on a stamper that outruns its
//! wall-clock budget rather than holding the commit open. Asserting on the
//! hook's TEXT would prove neither; these tests run the real script under
//! `/bin/sh` with a stamper on `PATH` that stalls, fails, or succeeds.
//! It must also honour git's SECOND argument, the commit source (#7249) — a
//! decision about WHOSE tokens the message describes, which likewise cannot be
//! proved by reading the script.
//! What: runs of the script under `/bin/sh` — a stalled `tm`, a failing `tm`, a
//! prompt `tm`, no session id, the real `tm` binary folding a transcript larger
//! than the fold's byte cap, and one run per commit source that changes the
//! answer: `merge`, `squash`, and `commit` with and without a session.
//! Test: this file IS the test module.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use trusty_mpm::core::push_guard::PREPARE_COMMIT_MSG_HOOK;
use trusty_mpm::core::session_record::{KIND_TRANSCRIPT, record_session_value};
use trusty_mpm::core::transcript_usage::TRANSCRIPT_TAIL_BYTES;

/// A temp tree holding the hook, a `bin/` directory that goes on `PATH`, and a
/// commit message file.
struct Fixture {
    root: tempfile::TempDir,
    hook: PathBuf,
    bin: PathBuf,
    message: PathBuf,
}

fn write_executable(path: &Path, body: &str) {
    let mut file = std::fs::File::create(path).expect("create script");
    file.write_all(body.as_bytes()).expect("write script");
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("tm-test-commit-stats-")
            .tempdir()
            .expect("temp dir");
        let hook = root.path().join("prepare-commit-msg");
        write_executable(&hook, PREPARE_COMMIT_MSG_HOOK);

        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("create bin dir");

        let message = root.path().join("COMMIT_EDITMSG");
        std::fs::write(&message, "feat: a thing (Refs #7074)\n").expect("write message");

        Self {
            root,
            hook,
            bin,
            message,
        }
    }

    /// Put a fake `tm` with `body` on the hook's `PATH`.
    fn fake_tm(&self, body: &str) {
        write_executable(&self.bin.join("tm"), body);
    }

    /// Put the real `tm` binary on the hook's `PATH`, as a shim rather than a
    /// copy: it keeps the test off any code-signing question and costs nothing.
    fn real_tm(&self) {
        write_executable(
            &self.bin.join("tm"),
            &format!("#!/bin/sh\nexec {:?} \"$@\"\n", env!("CARGO_BIN_EXE_tm")),
        );
    }

    /// Run the hook with git's one-argument form, as an ordinary editor commit.
    fn run(&self, session_id: Option<&str>, budget: &str) -> (std::process::ExitStatus, Duration) {
        self.run_sourced(session_id, budget, None, None)
    }

    /// Run the hook, returning its exit status and how long it took.
    ///
    /// `source` is git's second `prepare-commit-msg` argument, passed only when
    /// given so the one-argument call git makes for an editor commit is
    /// exercised too. `root` sets `TRUSTY_MPM_ROOT` for a run against the real
    /// binary.
    fn run_sourced(
        &self,
        session_id: Option<&str>,
        budget: &str,
        source: Option<&str>,
        root: Option<&Path>,
    ) -> (std::process::ExitStatus, Duration) {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&self.hook)
            .arg(&self.message)
            .env("PATH", path)
            .env("TM_COMMIT_STATS_TIMEOUT", budget)
            .env_remove("TM_SKIP_COMMIT_STATS")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(source) = source {
            cmd.arg(source);
        }
        if let Some(id) = session_id {
            cmd.env("CLAUDE_CODE_SESSION_ID", id);
        }
        if let Some(root) = root {
            cmd.env("TRUSTY_MPM_ROOT", root);
        }

        let started = Instant::now();
        let status = cmd.status().expect("the hook must be runnable");
        (status, started.elapsed())
    }

    /// Overwrite the commit message the hook will be handed.
    fn set_message(&self, text: &str) {
        std::fs::write(&self.message, text).expect("write message");
    }

    /// A managed root whose only session, `session_id`, has a transcript worth
    /// 10 input and 7 output tokens.
    fn seed_root(&self, session_id: &str) -> PathBuf {
        let root = self.root.path().join("mpm-root");
        std::fs::create_dir_all(&root).expect("create root");
        let transcript = self.root.path().join(format!("{session_id}.jsonl"));
        std::fs::write(
            &transcript,
            b"{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":7}}}\n",
        )
        .expect("write transcript");
        record_session_value(
            &root,
            KIND_TRANSCRIPT,
            session_id,
            &transcript.display().to_string(),
        );
        root
    }

    fn message_text(&self) -> String {
        std::fs::read_to_string(&self.message).expect("read message")
    }
}

/// Why: this is the failure the round-2 review named — `tm commit-trailers`
/// streaming a hundreds-of-megabytes transcript while `git` waits, so every
/// commit of the session slows. The budget has to end the wait whatever the
/// stamper is doing, and the commit has to proceed with no stats block rather
/// than not at all.
/// Test: itself.
#[test]
fn a_stalled_stamper_is_abandoned_and_the_commit_proceeds() {
    let fixture = Fixture::new();
    fixture.fake_tm("#!/bin/sh\nsleep 120\n");

    let (status, elapsed) = fixture.run(Some("sess-stall"), "1");

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert!(
        elapsed < Duration::from_secs(30),
        "the hook waited {elapsed:?} on a stamper that sleeps 120s"
    );
    assert_eq!(
        fixture.message_text(),
        "feat: a thing (Refs #7074)\n",
        "an abandoned stamper leaves the message exactly as git wrote it"
    );
}

/// Why: a `tm` that fails outright — a bad root, a corrupt store, a panic —
/// must not take the operator's commit down with it.
/// Test: itself.
#[test]
fn a_failing_stamper_leaves_the_message_and_exits_zero() {
    let fixture = Fixture::new();
    fixture.fake_tm("#!/bin/sh\necho boom >&2\nexit 3\n");

    let (status, _) = fixture.run(Some("sess-fail"), "2");

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert_eq!(fixture.message_text(), "feat: a thing (Refs #7074)\n");
}

/// Why: the budget is a bound, not a delay — the ordinary stamper finishes in
/// milliseconds and its work must still land. A poll loop that waited out its
/// budget before checking would pass the two tests above and fail this one; a
/// 60-second budget puts that failure two orders of magnitude clear of any
/// scheduling noise, so the bound below is a structural claim and not a
/// timing race.
/// Test: itself.
#[test]
fn a_prompt_stamper_still_stamps_and_costs_no_wait() {
    let fixture = Fixture::new();
    // The hook invokes `tm commit-trailers --message-file <path>`, so the
    // message file is the third argument.
    fixture.fake_tm("#!/bin/sh\nprintf '\\nTokens-In: 42\\n' >> \"$3\"\n");

    let (status, elapsed) = fixture.run(Some("sess-ok"), "60");

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert!(
        fixture.message_text().contains("Tokens-In: 42"),
        "{}",
        fixture.message_text()
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "a prompt stamper must not be charged the 60s budget: {elapsed:?}"
    );
}

/// Why: a commit made outside a Claude Code session has nothing to report, and
/// the hook must not spawn a stamper — or a budget wait — for it.
/// Test: itself.
#[test]
fn no_session_id_means_no_stamper_at_all() {
    let fixture = Fixture::new();
    // The hook invokes `tm commit-trailers --message-file <path>`, so the
    // message file is the third argument.
    fixture.fake_tm("#!/bin/sh\nprintf '\\nTokens-In: 42\\n' >> \"$3\"\n");

    let (status, _) = fixture.run(None, "2");

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert_eq!(fixture.message_text(), "feat: a thing (Refs #7074)\n");
}

/// Why: the byte cap and the hook's budget are two halves of one guarantee, and
/// only running both together proves it. This is the regression fixture the
/// round-2 review asked for — a transcript larger than the cap, folded by the
/// real `tm` binary, through the real hook, inside the budget.
/// Test: itself.
#[test]
fn the_real_stamper_folds_an_oversized_transcript_inside_the_budget() {
    let fixture = Fixture::new();
    fixture.real_tm();

    let root = fixture.root.path().join("mpm-root");
    std::fs::create_dir_all(&root).expect("create root");
    let transcript = fixture.root.path().join("big.jsonl");
    write_oversized_transcript(&transcript);
    record_session_value(
        &root,
        KIND_TRANSCRIPT,
        "sess-big",
        &transcript.display().to_string(),
    );

    let (status, elapsed) = fixture.run_sourced(Some("sess-big"), "20", None, Some(&root));

    assert!(status.success(), "the hook must exit 0: {status:?}");
    let stamped = fixture.message_text();
    assert!(stamped.contains("Tokens-In: 10"), "{stamped}");
    assert!(stamped.contains("Tokens-Out: 7"), "{stamped}");
    assert!(
        stamped.contains("Tokens-Window: last 8 MiB of a larger transcript"),
        "the counts cover the tail window and must say so: {stamped}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the capped fold must finish well inside the budget: {elapsed:?}"
    );
}

/// Why (#7249): git passes the commit SOURCE as its second argument, and a
/// merge message is assembled from the merge's parents — the tokens of whoever
/// ran `git merge` describe none of it. The hook must not spawn the stamper at
/// all. A hook that ignored the argument stamps here, which is the bug.
/// Test: itself.
#[test]
fn a_merge_source_leaves_the_message_untouched() {
    let fixture = Fixture::new();
    // The hook invokes `tm commit-trailers --message-file <path>`, so the
    // message file is the third argument.
    fixture.fake_tm("#!/bin/sh\nprintf '\\nTokens-In: 42\\n' >> \"$3\"\n");

    let (status, _) = fixture.run_sourced(Some("sess-merge"), "60", Some("merge"), None);

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert_eq!(
        fixture.message_text(),
        "feat: a thing (Refs #7074)\n",
        "a merge commit's message belongs to no single session"
    );
}

/// Why (#7249): the same rule for `squash` — the message is assembled by git or
/// GitHub from a branch's commits, each with its own figures.
/// Test: itself.
#[test]
fn a_squash_source_leaves_the_message_untouched() {
    let fixture = Fixture::new();
    fixture.fake_tm("#!/bin/sh\nprintf '\\nTokens-In: 42\\n' >> \"$3\"\n");

    let (status, _) = fixture.run_sourced(Some("sess-squash"), "60", Some("squash"), None);

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert_eq!(fixture.message_text(), "feat: a thing (Refs #7074)\n");
}

/// Why (#7249): a cherry-pick hands the hook the ORIGINAL commit's message,
/// block included. Before this fix `already_stamped()` read that inherited
/// block as this session's own work and left the stale figures in place. The
/// whole path — hook, source argument, real `tm` — has to replace them.
/// Test: itself.
#[test]
fn a_commit_source_replaces_inherited_trailers() {
    let fixture = Fixture::new();
    fixture.real_tm();
    let root = fixture.seed_root("sess-pick");
    fixture.set_message(
        "feat: a thing (Refs #7249)\n\nTokens-In: 999999\nTokens-Out: 888888\nModel: some-other-model\n",
    );

    let (status, _) = fixture.run_sourced(Some("sess-pick"), "20", Some("commit"), Some(&root));

    assert!(status.success(), "the hook must exit 0: {status:?}");
    let stamped = fixture.message_text();
    assert!(
        !stamped.contains("999999") && !stamped.contains("some-other-model"),
        "the original commit's figures must be gone: {stamped}"
    );
    assert!(stamped.contains("Tokens-In: 10"), "{stamped}");
    assert!(stamped.contains("Tokens-Out: 7"), "{stamped}");
    assert_eq!(
        stamped.matches("Tokens-In:").count(),
        1,
        "exactly one stats block: {stamped}"
    );
}

/// Why (#7249): the strip is not conditional on having something to write in
/// its place. Cherry-picking outside a Claude Code session must still remove
/// the original session's figures — no block at all is correct, another
/// session's block is not.
/// Test: itself.
#[test]
fn a_commit_source_strips_even_with_no_session() {
    let fixture = Fixture::new();
    fixture.real_tm();
    let root = fixture.seed_root("sess-pick");
    fixture.set_message("feat: a thing (Refs #7249)\n\nTokens-In: 999999\nSavings: 91%\n");

    let (status, _) = fixture.run_sourced(None, "20", Some("commit"), Some(&root));

    assert!(status.success(), "the hook must exit 0: {status:?}");
    assert_eq!(
        fixture.message_text(),
        "feat: a thing (Refs #7249)\n",
        "the inherited block goes and nothing replaces it"
    );
}

/// A transcript past [`TRANSCRIPT_TAIL_BYTES`] whose only countable turn is its
/// last line, so a fold that respected the cap and one that did not report
/// different totals.
fn write_oversized_transcript(path: &Path) {
    let filler = format!("{{\"type\":\"user\",\"pad\":\"{}\"}}\n", "a".repeat(8192));
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).expect("create transcript"));
    let mut written: u64 = 0;
    while written <= TRANSCRIPT_TAIL_BYTES {
        file.write_all(filler.as_bytes()).expect("write filler");
        written += filler.len() as u64;
    }
    file.write_all(
        b"{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":7}}}\n",
    )
    .expect("write the countable turn");
    file.flush().expect("flush transcript");
}
