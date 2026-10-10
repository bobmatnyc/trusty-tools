//! Tests for process ancestry (#9070).

use super::*;
use crate::server::grant_fakes::FakeProcs;

fn tree() -> FakeProcs {
    FakeProcs::sample_tree()
}

const CHILD: u32 = 20;
const CHILD_START: StartTime = StartTime(200);

#[test]
fn descendant_check_accepts_child_and_grandchild() {
    let procs = tree();
    assert_eq!(
        is_self_or_descendant(&procs, 20, CHILD, CHILD_START),
        Ok(true)
    );
    assert_eq!(
        is_self_or_descendant(&procs, 30, CHILD, CHILD_START),
        Ok(true)
    );
}

#[test]
fn descendant_check_refuses_sibling_parent_and_unrelated() {
    let procs = tree();
    for pid in [21, 10, 50, 1] {
        assert_eq!(
            is_self_or_descendant(&procs, pid, CHILD, CHILD_START),
            Ok(false),
            "pid {pid}"
        );
    }
}

#[test]
fn descendant_check_fails_closed_on_unreadable_table() {
    let procs = tree();
    procs.set_unreadable();
    assert_eq!(
        is_self_or_descendant(&procs, 30, CHILD, CHILD_START),
        Err(ProcessError::Unreadable { pid: 30 })
    );
    // A pid missing from the table is unreadable too, never "not related".
    let procs = tree();
    assert_eq!(
        is_self_or_descendant(&procs, 99, CHILD, CHILD_START),
        Err(ProcessError::Unreadable { pid: 99 })
    );
}

#[test]
fn descendant_check_refuses_a_parent_loop() {
    let procs = FakeProcs::default();
    procs.add(60, 61, 1).add(61, 60, 1);
    assert_eq!(
        is_self_or_descendant(&procs, 60, CHILD, CHILD_START),
        Err(ProcessError::ChainTooDeep)
    );
}

#[test]
fn linux_stat_parser_reads_parent_and_start_time() {
    // `comm` holds a space and a `)`, so the split must start after the last `)`.
    let mut line = String::from("4242 (we ird) name) S 77");
    for field in 5..=21 {
        line.push_str(&format!(" {field}"));
    }
    line.push_str(" 987654 rest");
    assert_eq!(stat_parent(4242, &line), Ok(77));
    assert_eq!(stat_start_time(4242, &line), Ok(StartTime(987654)));

    let short = "4242 (sh) S 77 1 1";
    assert_eq!(stat_parent(4242, short), Ok(77));
    assert_eq!(
        stat_start_time(4242, short),
        Err(ProcessError::StartTimeUnreadable { pid: 4242 })
    );
    assert_eq!(
        stat_parent(4242, "garbage"),
        Err(ProcessError::Unreadable { pid: 4242 })
    );
    // #9070: `comm` runs from the first `(` to the last `)`.
    assert_eq!(stat_comm(4242, &line), Ok("we ird) name"));
    assert_eq!(
        stat_comm(4242, "garbage"),
        Err(ProcessError::Unreadable { pid: 4242 })
    );
}

/// Why: #9070 AC 3 — Claude Code two hops above the registrar (through a
/// shell) still marks it; a tree with no agent does not.
/// Test: itself.
#[test]
fn agent_ancestor_is_found_above_a_shell() {
    let procs = tree();
    procs.set_agent(10);
    for pid in [10, 20, 30, 21] {
        assert_eq!(has_agent_ancestor(&procs, pid), Ok(true), "pid {pid}");
    }
    assert_eq!(has_agent_ancestor(&procs, 50), Ok(false));
    assert_eq!(has_agent_ancestor(&tree(), 30), Ok(false));
}

/// Why: #9070 — an ancestor that cannot be read is never skipped, and a
/// parent loop is not an answer.
/// Test: itself.
#[test]
fn agent_ancestor_check_fails_closed_on_unreadable_table() {
    let procs = tree();
    procs.set_unreadable();
    assert_eq!(
        has_agent_ancestor(&procs, 30),
        Err(ProcessError::Unreadable { pid: 30 })
    );
    assert_eq!(
        has_agent_ancestor(&tree(), 99),
        Err(ProcessError::Unreadable { pid: 99 })
    );
    let looped = FakeProcs::default();
    looped.add(60, 61, 1).add(61, 60, 1);
    assert_eq!(
        has_agent_ancestor(&looped, 60),
        Err(ProcessError::ChainTooDeep)
    );
}

/// A table that implements only the two required reads (#9070 slice 3).
struct ParentsOnly;

impl ProcessTable for ParentsOnly {
    fn parent(&self, _pid: u32) -> Result<u32, ProcessError> {
        Ok(1)
    }

    fn start_time(&self, _pid: u32) -> Result<StartTime, ProcessError> {
        Ok(StartTime::from_raw(1))
    }
}

/// Why: #9070 slice 3 — `is_agent`'s default body must never vouch that a
/// process is not an agent; a table that does not judge refuses.
/// Test: itself.
#[test]
fn agent_check_fails_closed_on_a_table_without_is_agent() {
    assert_eq!(
        has_agent_ancestor(&ParentsOnly, 42),
        Err(ProcessError::Unreadable { pid: 42 })
    );
}

/// Why: the native binary is `~/.local/share/claude/versions/<v>`; a
/// `.claude` dot-directory in a build path is not Claude Code.
/// Test: itself.
#[test]
fn agent_label_matches_name_and_versioned_path() {
    for label in [
        "claude",
        "Claude",
        "/Users/u/.local/share/claude/versions/2.1.295",
        "/opt/claude-code/bin/x",
    ] {
        assert!(names_agent(label), "{label}");
    }
    for label in [
        "2.1.295",
        "zsh",
        "/repo/.claude/worktrees/a/target/debug/tm",
        "notclaude",
    ] {
        assert!(!names_agent(label), "{label}");
    }
}

fn args(list: &[&str]) -> Result<Vec<String>, ProcessError> {
    Ok(list.iter().map(|a| (*a).to_owned()).collect())
}

/// Why: #9621, ruling Q2 — npm Claude Code is a `node` whose argv names the
/// `claude-code` package (or the `claude` bin link); a plain `node` is not an
/// agent, and a non-`node` process never has its argv read.
/// Red when the argv arm is removed (claude-code node answers `false`), or
/// when every `node` is judged an agent (plain node answers `true`).
/// Test: itself.
#[test]
fn agent_verdict_reads_argv_of_node_only() {
    let npm = "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js";
    assert_eq!(agent_verdict(&["node"], || args(&["node", npm])), Ok(true));
    assert_eq!(
        agent_verdict(&["node", "/opt/homebrew/bin/node"], || args(&[
            "node",
            "/opt/homebrew/bin/claude",
            "--resume"
        ])),
        Ok(true)
    );
    assert_eq!(
        agent_verdict(&["nodejs"], || args(&["nodejs", npm])),
        Ok(true)
    );
    assert_eq!(
        agent_verdict(&["node", "/usr/bin/node"], || args(&[
            "node",
            "/srv/app/server.js"
        ])),
        Ok(false),
        "plain node"
    );
    let never = || -> Result<Vec<String>, ProcessError> { panic!("argv read for non-node") };
    assert_eq!(agent_verdict(&["zsh", "/bin/zsh"], never), Ok(false));
    assert_eq!(agent_verdict(&["claude"], never), Ok(true));
    assert!(names_node("/usr/local/bin/node") && !names_node("nodemon"));
}

/// Why: #9621 fix round — only argv[0] and the script operand are judged,
/// and only a `claude-code` segment or a basename exactly `claude` matches,
/// so a plain `node` whose arguments merely mention Claude is not an agent.
/// Red against a338dca398's matcher (every argument through `names_agent`):
/// `CLAUDE.md` and `claude-mpm` were judged agents. The unlisted-option rows
/// (Architect ruling 16:41Z) were red against 005b97f240, where an unlisted
/// option's separate value (`.env`) was taken as the script.
/// Test: itself.
#[test]
fn agent_verdict_judges_only_argv0_and_the_script_operand() {
    let judge = |list: &[&str]| agent_verdict(&["node"], || args(list));
    for plain in [
        &["node", "lint.js", "CLAUDE.md"][..],
        &["node", "/x/claude-mpm/scripts/a.js"],
        &["node", "build.js", "--name", "claude"],
        &["node", "-e", "require('/x/claude-code/cli.js')"],
        &["node", "app.js", "/x/@anthropic-ai/claude-code/cli.js"],
        &["node", "--no-warnings", "server.js", "--port", "3000"],
    ] {
        assert_eq!(judge(plain), Ok(false), "{plain:?}");
    }
    for agent in [
        &[
            "node",
            "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ][..],
        &["node", "/usr/local/bin/claude"],
        &["claude"],
        &["claude", "--resume"],
        &[
            "node",
            "--max-old-space-size=4096",
            "/x/@anthropic-ai/claude-code/cli.js",
        ],
        &["node", "-r", "dotenv/config", "/usr/local/bin/claude"],
        &["node", "--", "/usr/local/bin/claude"],
        // #9621 Architect ruling 16:41Z: an unlisted option's separate value
        // must not hide the script that follows it.
        &["node", "--env-file", ".env", "/usr/local/bin/claude"],
        &[
            "node",
            "--inspect-port",
            "9229",
            "/x/@anthropic-ai/claude-code/cli.js",
        ],
        &[
            "node",
            "--some-future-flag",
            "value",
            "/x/claude-code/cli.js",
        ],
        &[
            "node",
            "--input-type",
            "module",
            "--env-file-if-exists",
            ".e",
            "/usr/local/bin/claude",
        ],
    ] {
        assert_eq!(judge(agent), Ok(true), "{agent:?}");
    }
}

/// Why: #9621 — a `node` whose argv cannot be read may be Claude Code, so
/// the verdict is the read error and the caller refuses (fail-closed).
/// Red when an unreadable argv answers `Ok(false)`.
/// Test: itself.
#[test]
fn agent_verdict_fails_closed_on_unreadable_node_argv() {
    let unreadable = ProcessError::Unreadable { pid: 77 };
    assert_eq!(
        agent_verdict(&["node"], || Err(unreadable)),
        Err(unreadable)
    );
}

/// Test: itself.
#[test]
fn cmdline_parser_splits_on_nul() {
    assert_eq!(
        parse_cmdline(b"node\0/x/claude-code/cli.js\0"),
        Some(vec!["node".to_owned(), "/x/claude-code/cli.js".to_owned()])
    );
    assert_eq!(parse_cmdline(b""), None, "a zombie has no argv");
}

/// Test: itself.
#[test]
fn procargs2_parser_reads_argc_arguments() {
    let mut buf = 2i32.to_ne_bytes().to_vec();
    buf.extend_from_slice(b"/usr/local/bin/node\0\0\0\0node\0/x/claude-code/cli.js\0HOME=/u\0");
    assert_eq!(
        parse_procargs2(&buf),
        Some(vec!["node".to_owned(), "/x/claude-code/cli.js".to_owned()])
    );
    let mut short = 3i32.to_ne_bytes().to_vec();
    short.extend_from_slice(b"/usr/local/bin/node\0node\0");
    assert_eq!(parse_procargs2(&short), None, "fewer than argc arguments");
    assert_eq!(parse_procargs2(b"\x01\0"), None, "no argc");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os_table {
    use super::super::*;

    #[test]
    fn os_table_reads_this_process() {
        let me = std::process::id();
        assert_eq!(
            OsProcessTable.parent(me),
            Ok(std::os::unix::process::parent_id())
        );
        let first = OsProcessTable.start_time(me).expect("own start time");
        assert_eq!(OsProcessTable.start_time(me), Ok(first));
    }

    #[test]
    fn os_table_sees_a_spawned_child_as_a_descendant() {
        let me = std::process::id();
        let my_start = OsProcessTable.start_time(me).expect("own start time");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id();
        let child_start = OsProcessTable.start_time(child_pid);
        // #9070: `sleep` is never judged a Claude Code process.
        let sleep_is_agent = OsProcessTable.is_agent(child_pid);
        let down = is_self_or_descendant(&OsProcessTable, child_pid, me, my_start);
        let up =
            child_start.map(|start| is_self_or_descendant(&OsProcessTable, me, child_pid, start));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(down, Ok(true));
        assert_eq!(up, Ok(Ok(false)));
        assert_eq!(sleep_is_agent, Ok(false));
    }

    #[test]
    fn os_table_refuses_a_pid_that_does_not_exist() {
        // Above every platform's pid_max, and above i32::MAX for macOS.
        let pid = u32::MAX - 1;
        assert_eq!(
            OsProcessTable.parent(pid),
            Err(ProcessError::Unreadable { pid })
        );
        assert_eq!(
            OsProcessTable.start_time(pid),
            Err(ProcessError::Unreadable { pid })
        );
        assert_eq!(
            OsProcessTable.is_agent(pid),
            Err(ProcessError::Unreadable { pid })
        );
    }

    /// Why: #9621 — the argv reader (`/proc/<pid>/cmdline` or
    /// `KERN_PROCARGS2`) returns this process's own arguments.
    /// Test: itself.
    #[test]
    fn os_argv_reads_this_process() {
        let me = std::process::id();
        let expected: Vec<String> = std::env::args().collect();
        assert_eq!(super::super::os::argv(me), Ok(expected));
    }

    /// Spawn `/bin/sh` through a link named `node`, so its Linux `comm` is
    /// `node`, with argv `node <dir>/<script>`: the script operand, a shell
    /// file whose `sleep; :` keeps the shell alive.
    #[cfg(target_os = "linux")]
    fn spawn_node(dir: &std::path::Path, script: &str) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let link = dir.join("node");
        if !link.exists() {
            std::os::unix::fs::symlink("/bin/sh", &link).expect("link sh as node");
        }
        let path = dir.join(script);
        std::fs::create_dir_all(path.parent().expect("script dir")).expect("mkdir");
        std::fs::write(&path, "sleep 30; :\n").expect("write script");
        std::process::Command::new(&link)
            .arg0("node")
            .arg(&path)
            .spawn()
            .expect("spawn node-named sh")
    }

    /// Why: #9621, ruling Q2, on the host table — a process named `node`
    /// whose argv carries a `claude-code` segment is an agent; one whose
    /// argv does not is not. Linux only: there `comm` is the exec'd link's
    /// name; macOS is covered by `procargs2_parser_reads_argc_arguments` and
    /// `os_argv_reads_this_process`.
    /// Red when `is_agent` ignores argv (the claude-code node answers
    /// `false`).
    /// Test: itself.
    #[cfg(target_os = "linux")]
    #[test]
    fn os_table_judges_a_node_process_on_its_argv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut npm = spawn_node(dir.path(), "x/@anthropic-ai/claude-code/cli.js");
        let mut plain = spawn_node(dir.path(), "srv/app/server.js");
        let npm_verdict = OsProcessTable.is_agent(npm.id());
        let plain_verdict = OsProcessTable.is_agent(plain.id());
        for child in [&mut npm, &mut plain] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(npm_verdict, Ok(true), "claude-code node");
        assert_eq!(plain_verdict, Ok(false), "plain node");
    }
}

/// Why: #9070 fix round — on macOS the real table must read the root-owned
/// `login` (and pid 1) across uids, so a grant from a Terminal or iTerm2
/// shell walks its whole parent chain. A live probe of the host, run by hand
/// on a Mac with `--include-ignored --nocapture`; it reads no Keychain, no
/// store and no HOME.
/// What: picks a uid-0 `login` from `ps` (skipped when there is none), reads
/// `parent` and `is_agent` for it and for pid 1, then walks from this
/// process to pid 1 printing every hop, and asserts every read is `Ok`.
/// Test: itself.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "live macOS probe of the host process table; run by hand on a Mac"]
fn macos_live_ancestor_walk_probe() {
    let table = OsProcessTable;
    let listing = std::process::Command::new("ps")
        .args(["-axo", "pid=,uid=,comm="])
        .output()
        .expect("run ps");
    let text = String::from_utf8_lossy(&listing.stdout);
    let login = text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let pid: u32 = fields.next()?.parse().ok()?;
        let uid: u32 = fields.next()?.parse().ok()?;
        let comm = fields.collect::<Vec<_>>().join(" ");
        (uid == 0 && comm.ends_with("login")).then_some(pid)
    });
    match login {
        Some(pid) => {
            let parent = table.parent(pid);
            println!(
                "login pid {pid}: parent {parent:?}, is_agent {:?}",
                table.is_agent(pid)
            );
            assert!(parent.is_ok(), "root login parent unreadable: {parent:?}");
        }
        None => println!("no root-owned login process; login probe skipped"),
    }
    let init_parent = table.parent(1);
    println!(
        "pid 1: parent {init_parent:?}, is_agent {:?}",
        table.is_agent(1)
    );
    assert_eq!(init_parent, Ok(0), "pid 1's parent is the walk's root");

    let me = std::process::id();
    let mut pid = me;
    for _ in 0..MAX_CHAIN_DEPTH {
        let parent = table.parent(pid);
        println!(
            "hop pid {pid}: parent {parent:?}, is_agent {:?}",
            table.is_agent(pid)
        );
        match parent {
            Ok(next) if next != pid && pid > 1 => pid = next,
            Ok(_) => break,
            Err(e) => panic!("walk stopped at pid {pid}: {e:?}"),
        }
    }
    let verdict = has_agent_ancestor(&table, me);
    println!("has_agent_ancestor({me}) = {verdict:?}");
    assert!(verdict.is_ok(), "the walk failed: {verdict:?}");
}
