//! The bounded clipboard read behind `tm secrets set` (#7524 P2-L6).
//!
//! No test reads the real clipboard: every tool is `/bin/sh` running a script
//! in a temp dir, or a directory that holds no paste tool. A script runs
//! through `/bin/sh` rather than being executed itself, so a sibling test's
//! fork cannot hold it open for writing (`ETXTBSY`).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::paste::{CLIPBOARD_MAX_BYTES, CLIPBOARD_READ_TIMEOUT, ClipboardError};
use super::value::{PASTE_TOOLS, SystemClipboard, ValueSource};
use crate::test_support::hermetic_temp_dir;

/// The limit a test gives a tool it expects to hang.
const SHORT: Duration = Duration::from_millis(200);

/// The limits a hung-tool test tries in turn, so a slow fork on a loaded
/// host retries with a longer limit instead of failing (#7524 P2-L6).
const HUNG_LIMITS: [Duration; 3] = [SHORT, Duration::from_secs(1), Duration::from_secs(5)];

/// How long a test waits for `read` before calling the wait unbounded.
const UNBOUNDED: Duration = Duration::from_secs(10);

/// A `/bin/sh` tool running `body`, written to `dir/name`.
fn script(dir: &Path, name: &str, body: &str) -> (PathBuf, Vec<String>) {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write the script");
    (PathBuf::from("/bin/sh"), vec![path.display().to_string()])
}

/// `clipboard.read()` on its own thread; panics when it does not return.
fn read_within(clipboard: SystemClipboard) -> anyhow::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(clipboard.read());
    });
    rx.recv_timeout(UNBOUNDED).unwrap_or_else(|_| {
        panic!("read did not return within {UNBOUNDED:?}: the wait is unbounded")
    })
}

/// Whether `pid` is gone (`ESRCH`) within three seconds. An orphaned
/// grandchild may briefly be a zombie until init reaps it.
fn gone(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: signal 0 only probes for the process; nothing is delivered.
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Why: #7524 P2-L6, the Fail-Open Check — a hung paste tool blocked
/// `tm secrets set` forever and was never killed.
/// What: two scripts that background a grandchild holding stdout: one waits
/// on it, one exits and leaves it behind. Each runs with a second tool
/// behind it. The read must end in `Timeout`, never `Ok` and never the second
/// tool's answer, and the script and grandchild PIDs must both be gone. The
/// script publishes its PIDs with a rename; a run killed before the rename
/// retries with the next of [`HUNG_LIMITS`].
/// Test: itself.
#[test]
fn a_hung_paste_tool_times_out_and_its_whole_process_group_is_killed() {
    for (case, tail) in [("waits", "wait\n"), ("exits", "exit 0\n")] {
        let dir = hermetic_temp_dir();
        let pids = dir.path().join("pids");
        let next_ran = dir.path().join("next-ran");
        let body = format!(
            "sleep 20 &\necho \"$$ $!\" > '{p}.tmp'\nmv '{p}.tmp' '{p}'\n{tail}",
            p = pids.display()
        );
        let hung = script(dir.path(), "hung.sh", &body);
        let next = script(
            dir.path(),
            "next.sh",
            &format!(": > '{}'\nprintf next\n", next_ran.display()),
        );

        let mut published = None;
        for limit in HUNG_LIMITS {
            let tools = vec![hung.clone(), next.clone()];
            let err = read_within(SystemClipboard::with_tools(tools, limit))
                .expect_err("a hung tool must be an error, never Ok");
            assert!(
                matches!(
                    err.downcast_ref::<ClipboardError>(),
                    Some(ClipboardError::Timeout { .. })
                ),
                "{case}: expected ClipboardError::Timeout, got: {err:#}"
            );
            assert!(
                !next_ran.exists(),
                "{case}: a timeout must stop the read, not fall through to the next tool"
            );
            if let Ok(text) = std::fs::read_to_string(&pids) {
                published = Some(text);
                break;
            }
        }
        let text = published.unwrap_or_else(|| {
            panic!("{case}: the script never published its PIDs within {HUNG_LIMITS:?}")
        });
        let pids: Vec<libc::pid_t> = text
            .split_whitespace()
            .map(|p| p.parse().expect("a PID"))
            .collect();
        assert_eq!(
            pids.len(),
            2,
            "{case}: the script and its grandchild: {text:?}"
        );
        for pid in pids {
            assert!(gone(pid), "{case}: PID {pid} survived the timeout");
        }
    }
}

/// Why: #7524 P2-L6, critic finding — only the no-reader path of
/// [`SystemClipboard::in_dirs`] was covered, so a wrong join or a bare name
/// would fail every production read while the suite stayed green.
/// What: installs an executable script under the platform's first paste
/// tool name in a temp dir and reads through `in_dirs`. The read must be
/// `Ok` with the script's output for that tool's arguments. A child `cp`
/// writes the executable, so no thread of this process ever holds it open
/// for writing (`ETXTBSY`). Not a `/bin/echo` symlink: a multicall
/// coreutils dispatches on the link's name.
/// Test: itself.
#[test]
fn in_dirs_runs_the_tool_it_finds_in_a_search_directory() {
    let dir = hermetic_temp_dir();
    let (name, args) = PASTE_TOOLS[0];
    let staged = dir.path().join("tool.src");
    std::fs::write(&staged, "#!/bin/sh\nprintf 'found %s' \"$*\"\n").expect("stage the tool");
    let search = dir.path().join("search");
    std::fs::create_dir(&search).expect("search dir");
    let installed = Command::new("/bin/sh")
        .args(["-c", "cp \"$1\" \"$2\" && chmod 755 \"$2\"", "sh"])
        .arg(&staged)
        .arg(search.join(name))
        .status()
        .expect("install the tool");
    assert!(installed.success(), "install the tool: {installed}");

    let clipboard = SystemClipboard::in_dirs(std::slice::from_ref(&search), CLIPBOARD_READ_TIMEOUT);
    let text = read_within(clipboard).expect("the tool found in the search directory answers");
    assert_eq!(text, format!("found {}", args.join(" ")));
}

/// The env var that turns [`a_paste_tool_planted_on_path_never_runs`] into
/// its own child run; it holds the directory the child searches.
const CHILD_SEARCH_DIR: &str = "TM_TEST_7524_PASTE_SEARCH_DIR";

/// Why: #7524 P2-L6 — the paste tool was found through `PATH`, so a planted
/// `pbpaste` or `xclip` ran and its output became the secret.
/// What: re-runs this test in a child process whose `PATH` holds only fake
/// paste tools and whose search directory is empty. The child must report no
/// reader, naming the directory and `--value -`; no fake may run.
/// Test: itself.
#[test]
fn a_paste_tool_planted_on_path_never_runs() {
    if let Some(search) = std::env::var_os(CHILD_SEARCH_DIR) {
        let search = PathBuf::from(search);
        let err = SystemClipboard::in_dirs(std::slice::from_ref(&search), SHORT)
            .read()
            .expect_err("an empty search directory holds no reader");
        let text = format!("{err:#}");
        assert!(
            matches!(
                err.downcast_ref::<ClipboardError>(),
                Some(ClipboardError::NoReader { .. })
            ),
            "expected ClipboardError::NoReader, got: {text}"
        );
        assert!(
            text.contains(&search.display().to_string()) && text.contains("--value -"),
            "the error must name the directory searched and `--value -`: {text}"
        );
        return;
    }

    let dir = hermetic_temp_dir();
    let fakes = dir.path().join("fakes");
    let search = dir.path().join("search");
    let ran = dir.path().join("fake-ran");
    std::fs::create_dir(&fakes).expect("fakes dir");
    std::fs::create_dir(&search).expect("search dir");
    for name in ["pbpaste", "wl-paste", "xclip", "xsel"] {
        let path = fakes.join(name);
        let body = format!("#!/bin/sh\n: > '{}'\nprintf planted\n", ran.display());
        std::fs::write(&path, body).expect("write a fake");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let name = format!("{module}::a_paste_tool_planted_on_path_never_runs");

    let out = Command::new(std::env::current_exe().expect("the test binary"))
        .args([name.as_str(), "--exact", "--test-threads=1"])
        .env("PATH", &fakes)
        .env(CHILD_SEARCH_DIR, &search)
        .output()
        .expect("run the child test");

    assert!(!ran.exists(), "a paste tool planted on PATH ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the child run must run and pass exactly this test:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Why: #7524 P2-L6 — an empty clipboard is `Ok("")`, which `set` reports
/// as empty; it must stay distinct from a timeout.
/// What: a tool that prints `v`, and one that prints nothing, each under the
/// production time limit.
/// Test: itself.
#[test]
fn a_fast_tool_answers_and_an_empty_one_is_ok_empty_not_a_timeout() {
    let dir = hermetic_temp_dir();
    let value = script(dir.path(), "value.sh", "printf v\n");
    let empty = script(dir.path(), "empty.sh", "exit 0\n");

    let read = |tool| {
        read_within(SystemClipboard::with_tools(
            vec![tool],
            CLIPBOARD_READ_TIMEOUT,
        ))
    };
    assert_eq!(read(value).expect("a fast tool answers"), "v");
    assert_eq!(read(empty).expect("an empty clipboard is Ok"), "");
}

/// Why: #7524 P2-L6 — the output had no size bound, and a cut-off value
/// stored as a secret would be silently wrong.
/// What: exactly [`CLIPBOARD_MAX_BYTES`] is read whole; one byte more is
/// `TooLarge`.
/// Test: itself.
#[test]
fn output_over_the_cap_is_an_error_not_a_truncated_value() {
    let dir = hermetic_temp_dir();
    let at_cap = script(
        dir.path(),
        "at-cap.sh",
        &format!("head -c {CLIPBOARD_MAX_BYTES} /dev/zero\n"),
    );
    let over = script(
        dir.path(),
        "over.sh",
        &format!("head -c {} /dev/zero\n", CLIPBOARD_MAX_BYTES + 1),
    );

    let read = |tool| {
        read_within(SystemClipboard::with_tools(
            vec![tool],
            CLIPBOARD_READ_TIMEOUT,
        ))
    };
    let whole = read(at_cap).expect("exactly the cap is accepted");
    assert_eq!(whole.len(), CLIPBOARD_MAX_BYTES);
    let err = read(over).expect_err("over the cap must be an error, never a truncated value");
    assert!(
        matches!(
            err.downcast_ref::<ClipboardError>(),
            Some(ClipboardError::TooLarge { .. })
        ),
        "expected ClipboardError::TooLarge, got: {err:#}"
    );
}
