//! Tests for the shared per-session record store (#6972, generalized #7074).

use super::*;

/// Why: the round trip is the whole contract — a value written by the render
/// must read back in another process, under the same explicit root.
/// Test: itself.
#[test]
fn a_recorded_value_reads_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_TRANSCRIPT, "sess-1", "/tmp/s.jsonl");
    assert_eq!(
        read_session_record(dir.path(), KIND_TRANSCRIPT, "sess-1").as_deref(),
        Some("/tmp/s.jsonl")
    );
}

/// Why: `session_id` comes from Claude Code's stdin JSON, so an unchecked value
/// would let a crafted id name any file on disk.
/// Test: itself.
#[test]
fn rejects_a_path_traversal_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(
        session_record_path_in(dir.path(), KIND_MODEL, "../../etc/passwd"),
        None
    );
    record_session_value(dir.path(), KIND_MODEL, "../../evil", "x");
    assert!(
        !dir.path().parent().expect("parent").join("evil").exists(),
        "nothing may be written outside the root"
    );
}

/// Why: this runs on the hot render path, so an unchanged value must cost one
/// read and no write at all.
/// Test: itself.
#[test]
fn an_unchanged_value_leaves_the_file_untouched() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    let path = session_record_path_in(dir.path(), KIND_MODEL, "sess-1").expect("path");
    let first = std::fs::metadata(&path).expect("metadata").modified().ok();

    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    let second = std::fs::metadata(&path).expect("metadata").modified().ok();
    assert_eq!(
        first, second,
        "an unchanged value must not rewrite the file"
    );
}

/// Why: a blank value states nothing, and writing it would replace a good
/// record with an empty one on a payload that happened to omit the field.
/// Test: itself.
#[test]
fn a_blank_value_is_never_recorded() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "   ");
    assert_eq!(read_session_record(dir.path(), KIND_MODEL, "sess-1"), None);
}

/// Why: an absent record is the ordinary state before the first render.
/// Test: itself.
#[test]
fn reading_an_absent_record_is_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(read_session_record(dir.path(), KIND_MODEL, "sess-x"), None);
}

/// Build a Claude-config-shaped directory holding one transcript file.
///
/// Why: every containment test needs the same two facts — a config directory
/// that canonicalizes, and a real file beneath it — and `canonicalize` refuses
/// a path that does not exist, so the fixture must touch the file.
fn config_dir_with_transcript(dir: &Path, session: &str) -> PathBuf {
    let projects = dir.join("projects").join("slug");
    std::fs::create_dir_all(&projects).expect("mkdir");
    let transcript = projects.join(format!("{session}.jsonl"));
    std::fs::write(&transcript, "{}\n").expect("write transcript");
    transcript
}

/// Why (#7250): the payload can name any absolute path on the machine, and the
/// process that later opens the record is not the one that wrote it. A path
/// outside the Claude config directory must be dropped, not recorded.
/// Test: itself.
#[test]
fn rejects_a_transcript_path_outside_the_config_dir() {
    let config = tempfile::tempdir().expect("temp dir");
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let outside = elsewhere.path().join("stolen.jsonl");
    std::fs::write(&outside, "{}\n").expect("write");

    assert_eq!(
        contained_transcript_path(config.path(), &outside.to_string_lossy()),
        None
    );
    assert_eq!(
        contained_transcript_path(config.path(), "/etc/passwd"),
        None
    );
}

/// Why (#7250): a `..` component is the shape that walks out of the config
/// directory while still LOOKING like it starts inside it, so it is rejected on
/// its own before any filesystem call.
/// Test: itself.
#[test]
fn rejects_a_traversing_transcript_path() {
    let config = tempfile::tempdir().expect("temp dir");
    config_dir_with_transcript(config.path(), "sess-1");
    let traversing = config
        .path()
        .join("projects")
        .join("..")
        .join("..")
        .join("etc")
        .join("passwd");

    assert_eq!(
        contained_transcript_path(config.path(), &traversing.to_string_lossy()),
        None
    );
    // A relative path is rejected on the same branch.
    assert_eq!(
        contained_transcript_path(config.path(), "projects/slug/sess-1.jsonl"),
        None
    );
}

/// Why (#7250): the screen must still accept the ordinary case, or the commit
/// footer silently loses its token counts. The accepted value is the CANONICAL
/// path, which on macOS differs from the temp directory's own spelling.
/// Test: itself.
#[test]
fn accepts_a_transcript_path_under_the_config_dir() {
    let config = tempfile::tempdir().expect("temp dir");
    let transcript = config_dir_with_transcript(config.path(), "sess-1");

    let accepted = contained_transcript_path(config.path(), &transcript.to_string_lossy())
        .expect("a transcript under the config dir must be accepted");
    assert_eq!(accepted, transcript.canonicalize().expect("canonicalize"));
    // Surrounding whitespace is trimmed, and a blank value says nothing.
    assert!(
        contained_transcript_path(config.path(), &format!("  {}  ", transcript.display()))
            .is_some()
    );
    assert_eq!(contained_transcript_path(config.path(), "   "), None);
}

/// Why (#7250): the screen resolves the CANDIDATE as well as the config
/// directory, and a symlink is the only shape that tells the two apart — a link
/// planted under the config directory spells a contained path while pointing at
/// a file outside it. Every other test here passes on lexical comparison alone,
/// so without this one a change that dropped the candidate's `canonicalize`
/// would leave the suite green.
/// Test: itself.
#[cfg(unix)]
#[test]
fn rejects_a_symlink_under_the_config_dir_aimed_outside_it() {
    let config = tempfile::tempdir().expect("temp dir");
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let outside = elsewhere.path().join("stolen.jsonl");
    std::fs::write(&outside, "{}\n").expect("write");

    // The link is planted under the CANONICAL config directory so its own
    // spelling already starts with the root the screen compares against; only
    // resolving the link separates it from a real transcript.
    let root = config.path().canonicalize().expect("canonicalize config");
    let link = root.join("linked.jsonl");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    assert!(
        link.starts_with(&root),
        "the link must look contained before it is resolved"
    );

    assert_eq!(
        contained_transcript_path(&root, &link.to_string_lossy()),
        None
    );
}

/// Why (#7074): the two kinds share one store, so a bug that dropped the kind
/// from the path would make the transcript path read back as the model id —
/// and the commit footer would then name a file as its model.
/// Test: itself.
#[test]
fn two_kinds_do_not_collide() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_session_value(dir.path(), KIND_MODEL, "sess-1", "claude-opus-4-1");
    record_session_value(dir.path(), KIND_TRANSCRIPT, "sess-1", "/tmp/s.jsonl");
    assert_eq!(
        read_session_record(dir.path(), KIND_MODEL, "sess-1").as_deref(),
        Some("claude-opus-4-1")
    );
    assert_eq!(
        read_session_record(dir.path(), KIND_TRANSCRIPT, "sess-1").as_deref(),
        Some("/tmp/s.jsonl")
    );
}

/// Why (#7278): the pm-guard cost evaluator DERIVES a subagent transcript path
/// and so screens a `Path`, not a `&str`. Both entry points must reach the same
/// verdict, or the guard and the statusline would disagree about the same file.
/// Test: itself.
#[test]
fn screens_a_path_valued_candidate_the_same_way() {
    let config = tempfile::tempdir().expect("temp dir");
    let outside_dir = tempfile::tempdir().expect("temp dir");

    let inside = config.path().join("in.jsonl");
    std::fs::write(&inside, "{}\n").expect("write");
    let outside = outside_dir.path().join("out.jsonl");
    std::fs::write(&outside, "{}\n").expect("write");

    for candidate in [&inside, &outside] {
        assert_eq!(
            contained_transcript_file(config.path(), candidate),
            contained_transcript_path(config.path(), &candidate.to_string_lossy()),
            "the two entry points must agree on {}",
            candidate.display()
        );
    }
    assert!(contained_transcript_file(config.path(), &inside).is_some());
    assert_eq!(contained_transcript_file(config.path(), &outside), None);
}

/// Why (#7290): `FrameworkPaths::home_base()` falls back to `"."` when there is
/// no home directory, and `FrameworkPaths::default()` inherits that fallback —
/// so a containment boundary built from either canonicalizes to the process's
/// working directory and silently re-scopes to `<cwd>/.claude` instead of
/// refusing. That is not a property a unit test can observe without mutating
/// the process's environment, which corrupts every sibling test in the binary.
/// Reading the source of the module that OWNS the screen is what makes the rule
/// mechanical instead of a comment nobody rereads.
/// Test: itself.
#[test]
fn the_containment_screen_never_leans_on_frameworkpaths_default() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/core/session_record.rs"
    ))
    .expect("read this module's own source");
    // The doc comment on `claude_config_dir` names both, explaining why they
    // are refused — so only CALLS count, not the prose.
    for banned in ["FrameworkPaths::default()", "FrameworkPaths::home_base()"] {
        assert!(
            !source
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains(banned)),
            "{banned} must never back the containment boundary (#7290)"
        );
    }
    assert!(
        source.contains("FrameworkPaths::under") && source.contains("dirs::home_dir()?"),
        "the boundary resolver must take an explicit, fallible home directory"
    );
}

/// Why (#7424): the doctor sample has no index — the ids ARE the file names —
/// so the listing is the only way back from the store to the sessions in it,
/// and a hand-dropped file must not become an id a later read joins onto a
/// path.
/// Test: itself.
#[test]
fn session_record_ids_lists_written_ids() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    record_session_value(root, KIND_STARTUP_CONTEXT, "sess-a", "{\"tokens\":1}");
    record_session_value(root, KIND_STARTUP_CONTEXT, "sess-b", "{\"tokens\":2}");
    std::fs::write(
        session_record_dir_in(root, KIND_STARTUP_CONTEXT).join("not a session id"),
        "x",
    )
    .expect("write stray file");

    let mut ids = session_record_ids(root, KIND_STARTUP_CONTEXT);
    ids.sort();
    assert_eq!(ids, vec!["sess-a".to_string(), "sess-b".to_string()]);
}

/// Why: a machine that has never recorded a startup reading has no directory at
/// all, and the doctor check must read that as "no sessions" rather than fail.
/// Test: itself.
#[test]
fn session_record_ids_of_a_missing_kind_is_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(session_record_ids(dir.path(), KIND_STARTUP_CONTEXT).is_empty());
}
