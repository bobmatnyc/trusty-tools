//! Tests for Claude Code's per-process session registry as #7771 evidence.
//! Temp-dir config roots; the process table and its process listing are
//! injected, so no test depends on which processes the host runs.

use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::session_manager::worktree_claude_processes::{Identity, ProcessInfo, identify};

const ID: &str = "0b7e1a55-7771-4000-8000-000000000001";
const NEWER: &str = "0b7e1a55-7771-4000-8000-000000000002";
const STAMP: &str = "Mon Sep 28 23:25:10 2026";
const START: i64 = 1_790_637_910; // 2026-09-28T23:25:10Z
/// The registered Claude Code process every complete read lists.
const CALLER_PID: u32 = 1;

/// What an injected process lister returns.
type Lister = Result<Vec<ProcessInfo>, String>;

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
    /// The calling Claude Code process, registered to an unrelated session.
    fn caller(&self) -> &Self {
        self.entry(CALLER_PID, NEWER, Some(STAMP))
    }
}

fn alive(pid: u32) -> Option<bool> {
    Some(pid < 1000)
}

fn started(_: u32) -> Result<i64, String> {
    Ok(START)
}

/// A process in the shape `sysinfo` listed on macOS on 2026-09-28.
fn native(pid: u32) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: "claude".into(),
        exe: Some("/Users/u/.local/bin/claude".into()),
        cmd: vec![
            "/Users/u/.local/bin/claude".into(),
            "--append-system-prompt-file".into(),
            "/tmp/p.txt".into(),
        ],
    }
}

/// A process that is not Claude Code.
fn shell(pid: u32) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: "zsh".into(),
        exe: Some("/bin/zsh".into()),
        cmd: vec!["-zsh".into()],
    }
}

/// Claude Code's binary running its embedded `ugrep`.
fn embedded_ugrep(pid: u32) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: "2.1.282".into(),
        exe: Some("/Users/u/.local/share/claude/versions/2.1.282".into()),
        cmd: vec!["ugrep".into(), "-r".into(), "x".into()],
    }
}

/// A process table whose Claude Code processes are `pids`, beside a shell and
/// an embedded `ugrep` that must not count.
fn listing(pids: &[u32]) -> impl Fn() -> Result<Vec<ProcessInfo>, String> + use<> {
    let pids = pids.to_vec();
    move || {
        let mut all: Vec<ProcessInfo> = pids.iter().map(|p| native(*p)).collect();
        all.extend([shell(900), embedded_ugrep(901)]);
        Ok(all)
    }
}

fn read_listing(
    roots: &[&Root],
    list: &dyn Fn() -> Result<Vec<ProcessInfo>, String>,
) -> ClaudeRegistry {
    let paths: Vec<PathBuf> = roots.iter().map(|r| r.path()).collect();
    let table = ProcessTable {
        pid_alive: &alive,
        start_of: &started,
        list,
    };
    ClaudeRegistry::read_with(&paths, &table)
}

/// A read whose only running Claude Code process is [`CALLER_PID`].
fn read(roots: &[&Root]) -> ClaudeRegistry {
    read_listing(roots, &listing(&[CALLER_PID]))
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
            "procStart":"{STAMP}","kind":"interactive","tmux":"tm-pm:@7.%7"}}"#
        ),
    );
    r.transcript(NEWER);
    let registry = read_listing(&[&r], &listing(&[79]));
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
    none.transcript(ID).caller();
    assert_eq!(read(&[&none]).session_end(ID), Some(SessionEnd::Ended));

    let dead = Root::new();
    dead.transcript(ID).caller().entry(5000, ID, Some(STAMP));
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
        read_listing(&[&home, &managed], &listing(&[7])).session_end(ID),
        Some(SessionEnd::Live)
    );
}

/// #7771: a running pid that started after the entry was written is a reused
/// pid, not the session's process.
#[test]
fn claude_registry_reused_pid_is_not_live() {
    let r = Root::new();
    r.transcript(ID)
        .caller()
        .entry(7, ID, Some("Mon Sep 28 23:20:10 2026"));
    assert_eq!(read(&[&r]).session_end(ID), Some(SessionEnd::Ended));
}

/// 🔴 #7771 critic (LOW): a running pid that started EARLIER than its entry
/// records is clock or format skew, never a reused pid, so it proves nothing.
/// Within the tolerance it is the entry's own process.
#[test]
fn claude_registry_an_earlier_start_is_skew_not_reuse() {
    let r = Root::new();
    r.transcript(ID)
        .caller()
        .entry(7, ID, Some("Mon Sep 28 23:30:10 2026"));
    assert!(undeterminable(&read_listing(&[&r], &listing(&[1, 7])), ID).contains("skew"));

    let near = Root::new();
    near.transcript(ID)
        .entry(7, ID, Some("Mon Sep 28 23:25:12 2026"));
    assert_eq!(
        read_listing(&[&near], &listing(&[7])).session_end(ID),
        Some(SessionEnd::Live)
    );
}

/// 🔴 #7771 fail-closed: every probe or read that cannot answer keeps the
/// session undeterminable.
#[test]
fn claude_registry_probe_failures_are_undeterminable() {
    let r = Root::new();
    r.transcript(ID).entry(7, ID, Some(STAMP));
    let paths = [r.path()];
    let list = listing(&[7]);
    let no_table = ProcessTable {
        pid_alive: &|_| None,
        start_of: &started,
        list: &list,
    };
    let no_table = ClaudeRegistry::read_with(&paths, &no_table);
    assert!(undeterminable(&no_table, ID).contains("process table"));
    let no_start = ProcessTable {
        pid_alive: &alive,
        start_of: &|_| Err("denied".into()),
        list: &list,
    };
    let no_start = ClaudeRegistry::read_with(&paths, &no_start);
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
    no_transcript.caller();
    assert!(undeterminable(&read(&[&no_transcript]), ID).contains("holds its transcript"));

    let bad_projects = Root::new();
    bad_projects.caller();
    std::fs::write(bad_projects.path().join("projects"), "").expect("a file");
    assert!(undeterminable(&read(&[&bad_projects]), ID).contains("transcripts"));

    assert!(undeterminable(&read(&[&r]), "../escape").contains("not shaped"));
}

/// 🔴 #7771 critic (HIGH): a running Claude Code process with no registry
/// entry — an older Claude Code, or a background worker — may run the
/// session, so no entry naming it proves nothing, and no replacement either.
/// Fails before the fix: `Some(Ended)`, and the tree was reclaimed.
#[test]
fn claude_registry_an_unregistered_claude_process_is_undeterminable() {
    let r = Root::new();
    r.transcript(ID).transcript(NEWER).caller();
    let registry = read_listing(&[&r], &listing(&[CALLER_PID, 42]));
    assert!(
        undeterminable(&registry, ID).contains("42 runs with no live entry"),
        "{registry:?}"
    );
    assert!(!registry.replaced_in(ID, "tm-pm"));

    // An npm install, run by node, is found by its script.
    let npm = || -> Lister {
        Ok(vec![
            native(CALLER_PID),
            ProcessInfo {
                pid: 43,
                name: "node".into(),
                exe: Some("/usr/local/bin/node".into()),
                cmd: vec!["node".into(), "/usr/local/bin/claude".into()],
            },
        ])
    };
    assert!(undeterminable(&read_listing(&[&r], &npm), ID).contains("43 runs"));

    let unlisted = || -> Lister { Err("EPERM".to_string()) };
    assert!(undeterminable(&read_listing(&[&r], &unlisted), ID).contains("could not be listed"));
}

/// 🔴 #7771 critic (HIGH) fail-closed: a table in which no Claude Code process
/// can be identified with confidence proves nothing.
#[test]
fn claude_registry_an_unidentified_table_is_undeterminable() {
    let bare = Root::new();
    bare.transcript(ID).registry();
    let why = undeterminable(&read_listing(&[&bare], &listing(&[])), ID);
    assert!(why.contains("no running Claude Code process"), "{why}");

    let r = Root::new();
    r.transcript(ID).caller();
    let unreadable = || -> Lister {
        Ok(vec![
            native(CALLER_PID),
            ProcessInfo {
                pid: 44,
                name: "2.1.284".into(),
                exe: None,
                cmd: Vec::new(),
            },
        ])
    };
    let why = undeterminable(&read_listing(&[&r], &unreadable), ID);
    assert!(why.contains("process 44 is named `2.1.284`"), "{why}");

    // The registered caller runs, yet the lister did not see it as Claude.
    let blind = || -> Lister { Ok(vec![shell(CALLER_PID), native(46)]) };
    let r46 = Root::new();
    r46.transcript(ID).caller().entry(46, NEWER, Some(STAMP));
    let why = undeterminable(&read_listing(&[&r46], &blind), ID);
    assert!(
        why.contains("registered process 1 runs but was not identified"),
        "{why}"
    );
}

/// 🔴 #7771 critic (MEDIUM): a newer session in the same tmux session does not
/// replace `id` while a process still runs it. Pins the `Ended` guard in
/// `replaced_in`; the positive control shows the same registry replaces an
/// `id` whose process is gone.
#[test]
fn claude_registry_a_live_id_is_not_replaced_by_a_newer_one() {
    let r = Root::new();
    r.transcript(ID)
        .transcript(NEWER)
        .entry(7, ID, Some(STAMP))
        .entry(8, NEWER, Some(STAMP));
    let registry = read_listing(&[&r], &listing(&[7, 8]));
    assert_eq!(registry.session_end(ID), Some(SessionEnd::Live));
    assert!(!registry.replaced_in(ID, "tm-pm"));

    let gone = Root::new();
    gone.transcript(ID)
        .transcript(NEWER)
        .entry(5000, ID, Some(STAMP))
        .entry(8, NEWER, Some(STAMP));
    assert!(read_listing(&[&gone], &listing(&[8])).replaced_in(ID, "tm-pm"));
}

/// 🔴 #7771 critic (MEDIUM): a Claude id the link history proves superseded
/// is still `Live` while a registered process runs it. Pins the registry's
/// `Live` arm in `SessionOwners::registry_end`; without the registry the same
/// store answers `Ended`.
#[test]
fn session_end_a_live_registry_entry_outranks_a_superseded_history() {
    use crate::session_manager::worktree_reclaim_claim::ClaimLiveness;
    use crate::session_manager::worktree_reclaim_ownership::{LinkHistory, SessionOwners};
    let history = LinkHistory::Read(
        [(ID.to_string(), vec!["managed-pm".to_string()])]
            .into_iter()
            .collect(),
    );
    let owners = || {
        SessionOwners::observed([("managed-pm".to_string(), ClaimLiveness::Live)])
            .with_history(history.clone())
    };
    assert_eq!(owners().session_end(ID), SessionEnd::Ended);

    let r = Root::new();
    r.transcript(ID).entry(7, ID, Some(STAMP));
    let registry = read_listing(&[&r], &listing(&[7]));
    assert_eq!(
        owners().with_claude(registry).session_end(ID),
        SessionEnd::Live
    );
}

/// #7771: a corrupt entry whose file-name pid is gone ran nothing, so it does
/// not block the proof.
#[test]
fn claude_registry_ignores_a_corrupt_entry_whose_pid_is_gone() {
    let r = Root::new();
    r.transcript(ID).caller().raw(5000, "{truncated");
    assert_eq!(read(&[&r]).session_end(ID), Some(SessionEnd::Ended));
}

/// #7771: an unconsulted registry is no evidence, so nothing changes.
#[test]
fn claude_registry_not_read_proves_nothing() {
    assert_eq!(ClaudeRegistry::NotRead.session_end(ID), None);
    assert_eq!(ClaudeRegistry::read(&[]), ClaudeRegistry::NotRead);
    assert!(!ClaudeRegistry::NotRead.replaced_in(ID, "tm-pm"));
}

/// 🔴 #7771 critic (HIGH): Claude Code is found by its executable or argv,
/// never by the kernel name alone, in every shape measured or published.
#[test]
fn claude_process_identify_reads_the_measured_shapes() {
    let p = |name: &str, exe: Option<&str>, cmd: &[&str]| ProcessInfo {
        pid: 9,
        name: name.into(),
        exe: exe.map(PathBuf::from),
        cmd: cmd.iter().map(|a| (*a).to_string()).collect(),
    };
    let versions = "/home/u/.local/share/claude/versions/2.1.284";
    let npm = "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js";
    let cases = [
        (native(9), Identity::Session),
        // Linux: the kernel names it by the version file.
        (p("2.1.284", Some(versions), &["claude"]), Identity::Session),
        (p("2.1.284", Some(versions), &[]), Identity::Session),
        (
            p("node", Some("/usr/bin/node"), &["node", npm]),
            Identity::Session,
        ),
        (embedded_ugrep(9), Identity::EmbeddedTool),
        (p("2.1.284", None, &[]), Identity::Unreadable),
        (p("node", None, &[]), Identity::Unreadable),
        (shell(9), Identity::Other),
        (p("zsh", None, &[]), Identity::Other),
        (
            p("grep", Some("/usr/bin/grep"), &["grep", "claude"]),
            Identity::Other,
        ),
        (
            p(
                "Claude",
                Some("/Applications/Claude.app/Contents/MacOS/Claude"),
                &["/Applications/Claude.app/Contents/MacOS/Claude"],
            ),
            Identity::Other,
        ),
        (
            p(
                "python3",
                Some("/w/.claude/worktrees/a/.venv/bin/python3"),
                &["python3"],
            ),
            Identity::Other,
        ),
    ];
    for (process, want) in cases {
        assert_eq!(identify(&process), want, "{process:?}");
    }
}
