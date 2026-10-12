//! #9171 A2: the live prototype's `tests/test_self_ctx.py`, ported by name
//! against `trusty_architect::self_ctx`. Every directory is a tempdir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use trusty_architect::self_ctx::{
    CANDIDATE_LIMIT, USAGE_TAIL_START, default_dir, newest_transcript, projects_dir,
    self_ctx_tokens, tail_last_assistant_usage, transcript_dir_for,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn usage_line(usage: Value, msg_type: &str, entrypoint: &str) -> String {
    json!({"type": msg_type, "message": {"usage": usage}, "entrypoint": entrypoint}).to_string()
        + "\n"
}

fn cli(usage: Value) -> String {
    usage_line(usage, "assistant", "cli")
}

fn write(path: &Path, text: &str, mtime: u64) -> std::io::Result<PathBuf> {
    fs::write(path, text)?;
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(UNIX_EPOCH + Duration::from_secs(mtime))?;
    Ok(path.to_owned())
}

// --- newest_transcript: entrypoint filter + mtime ---------------------------

#[test]
fn test_newest_transcript_picks_the_highest_mtime_among_cli_files() -> TestResult {
    let d = tempfile::tempdir()?;
    write(
        &d.path().join("old.jsonl"),
        &cli(json!({"input_tokens": 1})),
        1000,
    )?;
    let new = write(
        &d.path().join("new.jsonl"),
        &cli(json!({"input_tokens": 2})),
        2000,
    )?;
    assert_eq!(newest_transcript(d.path(), CANDIDATE_LIMIT), Some(new));
    Ok(())
}

#[test]
fn test_newest_transcript_missing_or_empty_dir_returns_none() -> TestResult {
    let d = tempfile::tempdir()?;
    assert_eq!(
        newest_transcript(&d.path().join("does-not-exist"), CANDIDATE_LIMIT),
        None
    );
    assert_eq!(newest_transcript(d.path(), CANDIDATE_LIMIT), None); // exists, no *.jsonl
    fs::write(d.path().join("not-a-transcript.txt"), "x")?;
    assert_eq!(newest_transcript(d.path(), CANDIDATE_LIMIT), None); // wrong extension
    Ok(())
}

/// The question-collector regression (2026-09-27): a newer `claude -p`
/// transcript (entrypoint `sdk-cli`) must never be picked over an older,
/// genuinely interactive one.
#[test]
fn test_newest_transcript_skips_a_newer_sdk_cli_file_for_an_older_cli_one() -> TestResult {
    let d = tempfile::tempdir()?;
    let sup = write(
        &d.path().join("supervisor.jsonl"),
        &cli(json!({"input_tokens": 999_000})),
        1000,
    )?;
    let collector = usage_line(json!({"input_tokens": 1}), "assistant", "sdk-cli");
    write(&d.path().join("collector.jsonl"), &collector, 9000)?; // newer
    assert_eq!(newest_transcript(d.path(), CANDIDATE_LIMIT), Some(sup));
    assert_eq!(self_ctx_tokens(d.path())?, Some(999_000));
    Ok(())
}

/// Only the `limit` newest files are checked, even when that misses an older
/// cli file outside the window.
#[test]
fn test_newest_transcript_bounds_the_files_it_examines() -> TestResult {
    let d = tempfile::tempdir()?;
    let cli_path = write(
        &d.path().join("cli.jsonl"),
        &cli(json!({"input_tokens": 1})),
        1000,
    )?;
    for i in 0..5 {
        let sdk = usage_line(json!({"input_tokens": 1}), "assistant", "sdk-cli");
        write(&d.path().join(format!("sdk-{i}.jsonl")), &sdk, 2000 + i)?;
    }
    assert_eq!(newest_transcript(d.path(), 3), None); // cli.jsonl not in top 3
    assert_eq!(newest_transcript(d.path(), 10), Some(cli_path)); // in top 10
    Ok(())
}

// --- tail_last_assistant_usage -----------------------------------------------

#[test]
fn test_tail_last_assistant_usage_skips_non_assistant_lines() -> TestResult {
    let d = tempfile::tempdir()?;
    let f = d.path().join("t.jsonl");
    let text = usage_line(json!({"input_tokens": 1}), "user", "cli")
        + &cli(json!({"input_tokens": 10, "cache_read_input_tokens": 20}));
    fs::write(&f, text)?;
    let usage = tail_last_assistant_usage(&f, USAGE_TAIL_START)?;
    assert_eq!(
        usage,
        Some(json!({"input_tokens": 10, "cache_read_input_tokens": 20}))
    );
    Ok(())
}

#[test]
fn test_tail_last_assistant_usage_empty_file_returns_none() -> TestResult {
    let d = tempfile::tempdir()?;
    let f = d.path().join("empty.jsonl");
    fs::write(&f, "")?;
    assert_eq!(tail_last_assistant_usage(&f, USAGE_TAIL_START)?, None);
    Ok(())
}

/// The only usage line is the very first, followed by ~400KB of filler; the
/// reader must grow its window to the whole file rather than give up.
#[test]
fn test_tail_last_assistant_usage_grows_window_past_a_small_start() -> TestResult {
    let d = tempfile::tempdir()?;
    let f = d.path().join("big.jsonl");
    let filler =
        json!({"type": "user", "message": {"content": "x".repeat(200)}}).to_string() + "\n";
    let text =
        cli(json!({"input_tokens": 900_000, "cache_read_input_tokens": 0})) + &filler.repeat(2000);
    fs::write(&f, text)?;
    let usage = tail_last_assistant_usage(&f, 1024)?;
    assert_eq!(
        usage,
        Some(json!({"input_tokens": 900_000, "cache_read_input_tokens": 0}))
    );
    Ok(())
}

#[test]
fn test_tail_last_assistant_usage_returns_the_last_one_when_several_exist() -> TestResult {
    let d = tempfile::tempdir()?;
    let f = d.path().join("t.jsonl");
    let text: String = (1..=3).map(|n| cli(json!({"input_tokens": n}))).collect();
    fs::write(&f, text)?;
    assert_eq!(
        tail_last_assistant_usage(&f, USAGE_TAIL_START)?,
        Some(json!({"input_tokens": 3}))
    );
    Ok(())
}

// --- self_ctx_tokens ----------------------------------------------------------

#[test]
fn test_self_ctx_tokens_sums_the_three_usage_fields() -> TestResult {
    let d = tempfile::tempdir()?;
    let usage = json!({"input_tokens": 100, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 300});
    fs::write(d.path().join("t.jsonl"), cli(usage))?;
    assert_eq!(self_ctx_tokens(d.path())?, Some(600));
    Ok(())
}

#[test]
fn test_self_ctx_tokens_missing_fields_default_to_zero() -> TestResult {
    let d = tempfile::tempdir()?;
    fs::write(d.path().join("t.jsonl"), cli(json!({"input_tokens": 5})))?;
    assert_eq!(self_ctx_tokens(d.path())?, Some(5));
    Ok(())
}

#[test]
fn test_self_ctx_tokens_none_when_no_transcript() -> TestResult {
    let d = tempfile::tempdir()?;
    assert_eq!(self_ctx_tokens(d.path())?, None);
    Ok(())
}

#[test]
fn test_self_ctx_tokens_none_when_no_assistant_usage_line() -> TestResult {
    let d = tempfile::tempdir()?;
    fs::write(
        d.path().join("t.jsonl"),
        usage_line(json!({"input_tokens": 1}), "user", "cli"),
    )?;
    assert_eq!(self_ctx_tokens(d.path())?, None);
    Ok(())
}

#[test]
fn test_self_ctx_tokens_none_when_only_sdk_cli_transcripts_exist() -> TestResult {
    let d = tempfile::tempdir()?;
    let line = usage_line(json!({"input_tokens": 999_000}), "assistant", "sdk-cli");
    fs::write(d.path().join("collector.jsonl"), line)?;
    assert_eq!(self_ctx_tokens(d.path())?, None);
    Ok(())
}

/// The prototype derives REPO_ROOT from the script's own path; the port takes
/// the root and the home directory as arguments, so that half is the caller's.
#[test]
fn test_default_dir_derives_from_repo_root() {
    let home = Path::new("/home/u");
    let root = "/Users/x/trusty-mpm-projects/architect";
    let dir = default_dir(home, root);
    assert_eq!(dir, transcript_dir_for(&projects_dir(home), root));
    assert!(dir.starts_with(projects_dir(home)));
    let base = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    assert!(base.starts_with('-') && !base.contains('/'));
}

#[test]
fn test_transcript_dir_replaces_slash_and_dot() {
    let base = |root: &str| {
        transcript_dir_for(Path::new("/p"), root)
            .file_name()
            .map(|n| n.to_owned())
    };
    assert_eq!(
        base("/Users/x/p/.claude/wt"),
        Some("-Users-x-p--claude-wt".into())
    );
    assert_eq!(
        base("/Users/x/trusty-mpm-projects/architect"),
        Some("-Users-x-trusty-mpm-projects-architect".into())
    );
}
