//! `start-architect-questions.sh <item-id>...`: queue ids for the questions
//! session, then prompt it or start it (#12029 P2f).
//!
//! Why: an interactive dialog in the Architect's own session blocks its
//! supervision loop; the owner answers in a separate session instead.
//! What: [`run`] validates the ids (`^[A-Za-z][0-9a-km-np-z]{2}$`), appends
//! each one the queue file lacks, then: when the session runs, toggles Remote
//! Control ([`super::rc`], or `python3 $ARCH_Q_RC`), reads the input box
//! (`python3 <input-state.py> =<session>:`, second field) and prompts the
//! session through `relay.sh --no-task` only when the box is empty or a
//! suggestion; when it does not, starts it (`claude --remote-control`) and
//! appends the Remote Control URL to `relaunch.log` once the pane shows it.
//! `--dry-run` prints the actions and queues, starts and sends nothing (the
//! input-state read still runs). Every variable keeps the script's
//! `${NAME:-default}` reading: unset or empty takes the default.
//! Test: `start_recordings_replay_clean_against_the_port`,
//! `start_launch_runs_only_on_the_private_socket_and_logs_the_url`,
//! `start_exits_1_before_started_with_no_relaunch_log_on_a_failed_new_session`,
//! `start_treats_a_nan_rc_sleep_as_no_pause`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use super::{QuestionsError, TmuxCli, env_or, run_command};
use crate::clock::Clock;
use crate::spec_drift::Outcome;

/// Bound on `python3 <questions_rc>` and `python3 <input-state.py>`: the poller's 300 s.
const HELPER_TIMEOUT: Duration = Duration::from_secs(300);
/// Bound on `relay.sh`, which waits on the pane itself.
const RELAY_TIMEOUT: Duration = Duration::from_secs(300);

/// What the launcher reads besides its environment.
pub struct Options<'a> {
    /// `$0`, as the usage line names it.
    pub prog: String,
    /// The script's `ROOT`: the Architect checkout.
    pub root: PathBuf,
    /// The environment the script reads.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The tmux runner.
    pub tmux: TmuxCli,
    /// Wall clock and sleep.
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// The in-process `questions_rc` pass, run when `ARCH_Q_RC` is unset.
    pub rc: &'a dyn Fn() -> Outcome,
}

/// `[[ "$a" =~ ^[A-Za-z][0-9a-km-np-z]{2}$ ]]`.
pub fn valid_id(a: &str) -> bool {
    let tail = |c: char| c.is_ascii_digit() || (c.is_ascii_lowercase() && c != 'l' && c != 'o');
    let c: Vec<char> = a.chars().collect();
    c.len() == 3 && c[0].is_ascii_alphabetic() && tail(c[1]) && tail(c[2])
}

/// `$(cmd | awk '{print $2}')`: each line's second blank-separated field,
/// joined by newlines, trailing newlines removed.
fn second_fields(stdout: &str) -> String {
    if stdout.is_empty() {
        return String::new();
    }
    let fields: Vec<&str> = stdout
        .strip_suffix('\n')
        .unwrap_or(stdout)
        .split('\n')
        .map(|l| {
            l.split([' ', '\t'])
                .filter(|f| !f.is_empty())
                .nth(1)
                .unwrap_or("")
        })
        .collect();
    fields.join("\n").trim_end_matches('\n').to_owned()
}

/// `tmux capture-pane ... | grep -o 'https://claude.ai/code/session_[A-Za-z0-9]*' | tail -1`:
/// the last match on any line, `.` standing for any one character.
pub fn grep_url(pane: &str) -> String {
    let (head, tail) = ("https://claude", "ai/code/session_");
    let mut last = String::new();
    for line in pane.split('\n') {
        let mut at = 0;
        while let Some(i) = line[at..].find(head).map(|i| at + i) {
            let rest = &line[i + head.len()..];
            let Some(dot) = rest.chars().next() else {
                break;
            };
            let after = &rest[dot.len_utf8()..];
            if let Some(alnum) = after.strip_prefix(tail) {
                let n = alnum
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .unwrap_or(alnum.len());
                let end = i + head.len() + dot.len_utf8() + tail.len() + n;
                last = line[i..end].to_owned();
                at = end;
            } else {
                at = i + 1;
            }
        }
    }
    last
}

/// Appends `text` to `path`, as `>>` does.
fn append(path: &Path, text: &str) -> Result<(), QuestionsError> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|source| QuestionsError::Write {
            path: path.to_string_lossy().into_owned(),
            source,
        })
}

/// `HH:MMZ` (or `HH:MM:SSZ` with `secs`) of the clock, in UTC.
fn utc_clock(clock: &dyn Clock, secs: bool) -> String {
    let iso = crate::classify::alerts::iso(clock.now_unix().floor());
    format!("{}Z", &iso[11..if secs { 19 } else { 16 }])
}

/// The variables the script reads, defaults applied.
struct Vars {
    session: String,
    queue: String,
    input_state: String,
    relay: String,
}

impl Vars {
    fn read(o: &Options<'_>) -> Self {
        let root = o.root.to_string_lossy();
        let or = |name: &str, default: String| env_or(o.env, name).unwrap_or(default);
        Self {
            session: or("ARCH_Q_SESSION", super::rc::DEFAULT_SESSION.into()),
            queue: or(
                "ARCH_Q_QUEUE",
                format!("{root}/inbox/interactive-queue.txt"),
            ),
            input_state: or(
                "ARCH_Q_INPUT_STATE",
                format!("{root}/scripts/input-state.py"),
            ),
            relay: or("ARCH_Q_RELAY", format!("{root}/scripts/relay.sh")),
        }
    }
}

/// `mkdir -p "$(dirname "$Q")"`, then each id the queue lacks as a whole
/// line, appended one at a time (`grep -qx "$id" "$Q" || echo "$id" >> "$Q"`).
fn enqueue(queue: &str, ids: &[String]) -> Result<(), QuestionsError> {
    let q = Path::new(queue);
    let dir = q.parent().filter(|d| !d.as_os_str().is_empty());
    std::fs::create_dir_all(dir.unwrap_or(Path::new("."))).map_err(|source| {
        QuestionsError::Write {
            path: queue.to_owned(),
            source,
        }
    })?;
    let mut content = std::fs::read(q).unwrap_or_default();
    for id in ids {
        if content.split(|b| *b == b'\n').any(|l| l == id.as_bytes()) {
            continue;
        }
        let line = format!("{id}\n");
        append(q, &line)?;
        content.extend_from_slice(line.as_bytes());
    }
    Ok(())
}

/// The running-session branch: Remote Control first, then maybe the prompt.
fn prompt(o: &Options<'_>, v: &Vars, dry: bool, out: &mut Outcome) {
    // RC goes on before the prompt: the prompt opens the dialog, which blocks rc-restore.
    if !dry {
        let rc = match env_or(o.env, "ARCH_Q_RC") {
            Some(script) => helper(&script, &[], out),
            None => (o.rc)(),
        };
        // `|| true`: a failed toggle does not stop the prompt.
        out.stdout.push_str(&rc.stdout);
        out.stderr.push_str(&rc.stderr);
    }
    let target = format!("={}:", v.session);
    let mut sink = Outcome {
        stdout: String::new(),
        stderr: String::new(),
        code: 0,
    };
    let st = second_fields(&helper(&v.input_state, &[&target], &mut sink).stdout);
    let (s, q) = (&v.session, &v.queue);
    let line = match st.as_str() {
        "empty" | "suggestion" if dry => {
            format!("running: {s} input={st}; would prompt it to re-read the queue")
        }
        "empty" | "suggestion" => {
            let txt = format!(
                "[Architect {}] The queue {q} has new ids. Re-read it now and ask Bob about each \
                 id still in status interactive; after each answered dialog re-read it again \
                 until it is empty.",
                utc_clock(o.clock.as_ref(), false)
            );
            let mut cmd = Command::new(&v.relay);
            cmd.args(["--no-task", s, &txt]);
            match run_command(cmd, RELAY_TIMEOUT) {
                Ok((0, stdout, stderr)) => {
                    out.stdout.push_str(&stdout);
                    out.stderr.push_str(&stderr);
                    return;
                }
                Ok((_, stdout, stderr)) => {
                    out.stdout.push_str(&stdout);
                    out.stderr.push_str(&stderr);
                    format!("running: {s} prompt not sent; ids stay queued in {q}")
                }
                Err(e) => {
                    out.stderr.push_str(&format!("{e}\n"));
                    format!("running: {s} prompt not sent; ids stay queued in {q}")
                }
            }
        }
        _ => {
            let st = if st.is_empty() {
                "unknown"
            } else {
                st.as_str()
            };
            format!(
                "running: {s} input={st}; no prompt sent, ids stay queued in {q} (it re-reads \
                 the queue after each answered dialog)"
            )
        }
    };
    out.stdout.push_str(&line);
    out.stdout.push('\n');
}

/// `python3 <script> <args>`: its streams; a start failure is an empty
/// answer with its message on stderr (the script's `2>/dev/null` and `|| true`).
fn helper(script: &str, args: &[&str], out: &mut Outcome) -> Outcome {
    let mut cmd = Command::new("python3");
    cmd.arg(script).args(args);
    match run_command(cmd, HELPER_TIMEOUT) {
        Ok((code, stdout, stderr)) => Outcome {
            stdout,
            stderr,
            code,
        },
        Err(e) => {
            out.stderr.push_str(&format!("{e}\n"));
            Outcome {
                stdout: String::new(),
                stderr: String::new(),
                code: 127,
            }
        }
    }
}

/// The stopped-session branch: start it, then log its Remote Control URL.
fn launch(o: &Options<'_>, v: &Vars, out: &mut Outcome) -> Result<(), QuestionsError> {
    let home = (o.env)("HOME").unwrap_or_default();
    let cfg = env_or(o.env, "CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| format!("{home}/.trusty-tools/trusty-mpm/claude-config"));
    if !Path::new(&cfg).is_dir() {
        out.stderr
            .push_str(&format!("no Claude config dir: {cfg}\n"));
        out.code = 1;
        return Ok(());
    }
    let s = &v.session;
    let prompt = format!(
        "You are tm-architect-questions, not the Architect. Read \
         templates/architect-questions-brief.md now and follow it exactly. The queue file is {}.",
        v.queue
    );
    let command = format!("claude --remote-control '{s}' '{prompt}'");
    o.tmux
        .new_session(s, &o.root, &format!("CLAUDE_CONFIG_DIR={cfg}"), &command)?;
    out.stdout.push_str(&format!("started: {s}\n"));
    let root = o.root.to_string_lossy();
    let log = env_or(o.env, "RC_URL_LOG").unwrap_or_else(|| format!("{root}/inbox/relaunch.log"));
    let tries: u64 = env_or(o.env, "ARCH_Q_RC_TRIES").map_or(30, |t| t.parse().unwrap_or(0));
    // #12029: `sleep nan` fails and the loop runs on unpaused; NaN is 0 s, never a panic.
    let pause: f64 = env_or(o.env, "ARCH_Q_RC_SLEEP").map_or(1.0, |t| {
        t.parse().ok().filter(|p: &f64| !p.is_nan()).unwrap_or(0.0)
    });
    for _ in 0..tries {
        o.clock
            .sleep(Duration::from_secs_f64(pause.clamp(0.0, 3600.0)));
        let url = grep_url(&o.tmux.capture(s)?);
        if !url.is_empty() {
            let at = utc_clock(o.clock.as_ref(), true);
            append(Path::new(&log), &format!("{at} {s} rc: {url}\n"))?;
            out.stdout.push_str(&format!("rc: {url}\n"));
            break;
        }
    }
    Ok(())
}

/// Why: the drop-in for `start-architect-questions.sh [--dry-run] <item-id>...`,
/// which the Architect runs for an INTERACTIVE pending line and
/// `questions_rc` runs for its `start` action.
/// What: see the module doc. Exit 2 on a bad argument, 1 when the config dir
/// is missing or a tmux call or write fails (the message on stderr, nothing
/// after it run), else 0.
/// Test: `start_recordings_replay_clean_against_the_port`,
/// `start_exits_1_with_no_tmux_call_when_the_queue_dir_cannot_be_made`,
/// `start_exits_1_with_no_tmux_call_when_the_queue_append_fails`,
/// `start_exits_1_before_started_with_no_relaunch_log_on_a_failed_new_session`,
/// `start_exits_1_with_no_rc_line_on_a_failed_capture_pane`,
/// `start_exits_1_with_no_rc_line_when_the_relaunch_log_append_fails`.
pub fn run(args: &[String], o: &Options<'_>) -> Outcome {
    let mut out = Outcome {
        stdout: String::new(),
        stderr: String::new(),
        code: 0,
    };
    let usage = format!("usage: {} [--dry-run] <item-id>...\n", o.prog);
    let (mut dry, mut ids) = (false, Vec::new());
    for a in args {
        if a == "--dry-run" {
            dry = true;
        } else if a.starts_with('-') {
            return fail(out, usage, 2);
        } else if valid_id(a) {
            ids.push(a.clone());
        } else {
            return fail(out, format!("bad item id: {a}\n"), 2);
        }
    }
    if ids.is_empty() {
        return fail(out, usage, 2);
    }
    let v = Vars::read(o);
    if let Err(e) = steps(o, &v, dry, &ids, &mut out) {
        // Output already printed stays; the error ends the run.
        out.stderr
            .push_str(&format!("start-architect-questions: {e}\n"));
        out.code = 1;
    }
    out
}

/// Queue, then prompt or launch, printing into `out` as the script does.
fn steps(
    o: &Options<'_>,
    v: &Vars,
    dry: bool,
    ids: &[String],
    out: &mut Outcome,
) -> Result<(), QuestionsError> {
    let listed = ids.join(" ");
    if dry {
        out.stdout
            .push_str(&format!("would queue: {listed} -> {}\n", v.queue));
    } else {
        enqueue(&v.queue, ids)?;
        out.stdout.push_str(&format!("queued: {listed}\n"));
    }
    if o.tmux.has_session(&v.session)? {
        prompt(o, v, dry, out);
    } else if dry {
        out.stdout
            .push_str(&format!("would start: {}\n", v.session));
    } else {
        launch(o, v, out)?;
    }
    Ok(())
}

fn fail(mut out: Outcome, msg: String, code: i32) -> Outcome {
    out.stderr = msg;
    out.code = code;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_script_pattern() {
        for good in ["D01", "Bz9", "a2k", "Zpn"] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "D0", "D012", "1ab", "D0l", "Doo", "D0A", "D-1", "Dé1"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn awk_second_field_and_grep_url_follow_the_pipelines() {
        assert_eq!(second_fields("%3 empty | text\n"), "empty");
        assert_eq!(second_fields("a\n%1 typed x\n"), "\ntyped");
        assert_eq!(second_fields(""), "");
        let pane = "x https://claude.ai/code/session_A1 y\nhttps://claudeXai/code/session_\nz";
        assert_eq!(grep_url(pane), "https://claudeXai/code/session_");
        assert_eq!(grep_url("none"), "");
    }
}
