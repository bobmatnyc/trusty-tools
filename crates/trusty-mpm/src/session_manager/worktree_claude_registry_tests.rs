//! Tests for Claude Code's per-process session registry as #7771 evidence.
//! Temp-dir config roots; the process table is injected, so no test depends on
//! which processes the host runs.

use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

const ID: &str = "a718f99c-37e8-408c-8bb3-1c8fa0a7f86c";
const NEWER: &str = "0b7e1a55-7771-4000-8000-000000000002";
const STAMP: &str = "Mon Sep 28 23:25:10 2026";
const START: i64 = 1_790_637_910; // 2026-09-28T23:25:10Z

/// One Claude config dir under a temp root.
struct Root(TempDir);

impl Root {
    fn new() -> Self {
        Self(TempDir::new().expect("config root"))
    }
    fn path(&self) -> PathBuf {
        self.0.path().to_path_buf()
    }
    /// `projects/<slug>/<id>.jsonl`.
    fn transcript(&self, id: &str) -> &Self {
        let dir = self.0.path().join("projects").join("-work-repo");
        std::fs::create_dir_all(&dir).expect("projects dir");
        std::fs::write(dir.join(format!("{id}.jsonl")), "{}\n").expect("transcript");
        self
    }
    /// An empty `sessions` registry.
    fn registry(&self) -> &Self {
        std::fs::create_dir_all(self.0.path().join("sessions")).expect("sessions dir");
        self
    }
    /// `sessions/<pid>.json` with `body`.
    fn raw(&self, pid: u32, body: &str) -> &Self {
        self.registry();
        let file = self.0.path().join("sessions").join(format!("{pid}.json"));
        std::fs::write(file, body).expect("entry");
        self
    }
    /// An entry in the measured shape; `stamp` `None` omits `procStart`.
    fn entry(&self, pid: u32, id: &str, stamp: Option<&str>) -> &Self {
        let start = stamp.map_or(String::new(), |s| format!(r#","procStart":"{s}""#));
        self.raw(
            pid,
            &format!(r#"{{"pid":{pid},"sessionId":"{id}","tmux":"tm-pm:@1.%1"{start}}}"#),
        )
    }
}

fn alive(pid: u32) -> Option<bool> {
    Some(pid < 1000)
}

fn started(_: u32) -> Result<i64, String> {
    Ok(START)
}

fn read(roots: &[&Root]) -> ClaudeRegistry {
    let paths: Vec<PathBuf> = roots.iter().map(|r| r.path()).collect();
    ClaudeRegistry::read_with(&paths, &alive, &started)
}

fn undeterminable(registry: &ClaudeRegistry, id: &str) -> String {
    match registry.session_end(id) {
        Some(SessionEnd::Undeterminable(why)) => why,
        other => panic!("{id} must be undeterminable: {other:?}"),
    }
}

/// The entry shape measured on the host on 2026-09-28, read as written.
#[test]
fn claude_registry_reads_the_measured_entry_shape() {
    assert_eq!(parse_start_stamp(STAMP), Some(START));
    let r = Root::new();
    r.transcript(ID).raw(
        79,
        &format!(
            r#"{{"pid":79,"sessionId":"{ID}","cwd":"/work","startedAt":1790637910311,
            "procStart":"{STAMP}","kind":"interactive","tmux":"tm-pm:@422.%422"}}"#
        ),
    );
    r.transcript(NEWER);
    let registry = read(&[&r]);
    assert_eq!(registry.session_end(ID), Some(SessionEnd::Live));
    assert!(
        registry.replaced_in(NEWER, "tm-pm"),
        "the tmux name is read"
    );
}

/// 🔴 #7771: a session whose transcript this host holds and that no running
/// process is registered to has ended — no entry, or only a dead pid's.
#[test]
fn claude_registry_ends_a_session_no_live_process_runs() {
    let none = Root::new();
    none.transcript(ID).registry().entry(1, NEWER, Some(STAMP));
    assert_eq!(read(&[&none]).session_end(ID), Some(SessionEnd::Ended));

    let dead = Root::new();
    dead.transcript(ID).entry(5000, ID, Some(STAMP));
    assert_eq!(read(&[&dead]).session_end(ID), Some(SessionEnd::Ended));
}

/// #7771: a running process registered to the session keeps it live, in any
/// consulted root.
#[test]
fn claude_registry_keeps_a_session_a_live_process_runs() {
    let home = Root::new();
    home.transcript(ID).registry();
    let managed = Root::new();
    managed.entry(7, ID, Some(STAMP));
    assert_eq!(
        read(&[&home, &managed]).session_end(ID),
        Some(SessionEnd::Live)
    );
}

/// #7771: a running pid whose start differs from the entry's is a reused pid.
#[test]
fn claude_registry_reused_pid_is_not_live() {
    let r = Root::new();
    r.transcript(ID)
        .entry(7, ID, Some("Mon Sep 28 23:30:10 2026"));
    assert_eq!(read(&[&r]).session_end(ID), Some(SessionEnd::Ended));
}

/// 🔴 #7771 fail-closed: every probe or read that cannot answer keeps the
/// session undeterminable.
#[test]
fn claude_registry_probe_failures_are_undeterminable() {
    let r = Root::new();
    r.transcript(ID).entry(7, ID, Some(STAMP));
    let paths = [r.path()];
    let no_table = ClaudeRegistry::read_with(&paths, &|_| None, &started);
    assert!(undeterminable(&no_table, ID).contains("process table"));
    let no_start = ClaudeRegistry::read_with(&paths, &alive, &|_| Err("denied".into()));
    assert!(undeterminable(&no_start, ID).contains("denied"));

    let unstamped = Root::new();
    unstamped.transcript(ID).entry(7, ID, None);
    assert!(undeterminable(&read(&[&unstamped]), ID).contains("no readable start"));

    let corrupt = Root::new();
    corrupt.transcript(ID).raw(7, "{not json");
    assert!(undeterminable(&read(&[&corrupt]), ID).contains("may still run"));

    let not_a_dir = Root::new();
    not_a_dir.transcript(ID);
    std::fs::write(not_a_dir.path().join("sessions"), "").expect("a file");
    assert!(undeterminable(&read(&[&not_a_dir]), ID).contains("cannot be read"));

    let no_registry = Root::new();
    no_registry.transcript(ID);
    assert!(undeterminable(&read(&[&no_registry]), ID).contains("no Claude session registry"));

    let no_transcript = Root::new();
    no_transcript.registry();
    assert!(undeterminable(&read(&[&no_transcript]), ID).contains("holds its transcript"));

    let bad_projects = Root::new();
    bad_projects.registry();
    std::fs::write(bad_projects.path().join("projects"), "").expect("a file");
    assert!(undeterminable(&read(&[&bad_projects]), ID).contains("transcripts"));

    assert!(undeterminable(&read(&[&r]), "../escape").contains("not shaped"));
}

/// #7771: a corrupt entry whose file-name pid is gone ran nothing, so it does
/// not block the proof.
#[test]
fn claude_registry_ignores_a_corrupt_entry_whose_pid_is_gone() {
    let r = Root::new();
    r.transcript(ID).raw(5000, "{truncated");
    assert_eq!(read(&[&r]).session_end(ID), Some(SessionEnd::Ended));
}

/// #7771: an unconsulted registry is no evidence, so nothing changes.
#[test]
fn claude_registry_not_read_proves_nothing() {
    assert_eq!(ClaudeRegistry::NotRead.session_end(ID), None);
    assert_eq!(ClaudeRegistry::read(&[]), ClaudeRegistry::NotRead);
    assert!(!ClaudeRegistry::NotRead.replaced_in(ID, "tm-pm"));
}
