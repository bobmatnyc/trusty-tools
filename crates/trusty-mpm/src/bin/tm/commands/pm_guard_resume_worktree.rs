//! `tm hook --pm-guard` — the PM's `SendMessage` resume of an agent whose
//! isolated worktree is gone, refused (#8004).
//!
//! Why: Claude Code removes an agent's worktree when the agent stops with no
//! changes in it. A later `SendMessage` resumes that agent in the shared main
//! checkout, where it cannot commit (ADR-0061, #5649). Four recurrences
//! (2026-09-15 to 2026-09-26) showed the prose check in `tm-delegation-patterns`
//! does not hold, so the owner ruled on 2026-09-24 that the resume is denied
//! with "re-dispatch fresh". trusty-mpm cannot give the resumed agent a new
//! tree: worktree creation belongs to the harness (ADR-0044 decision 4), and a
//! `SendMessage` has no `isolation` parameter to rewrite. Refusing is the only
//! enforceable half.
//!
//! What: [`evaluate_resume_worktree`] reads the harness's own record of the
//! recipient, `<session>/subagents/agent-<id>.meta.json` beside the payload's
//! `transcript_path`. That record carries `worktreePath` while the tree is
//! granted, and is rewritten to `worktreeCleanlyRemoved: true` with no path
//! once the harness removes it. It is exact, survives a daemon restart and
//! outlives the daemon's one-hour terminal-delegation retention, which is why
//! it is read in place of the delegation map.
//!
//! **Fail direction.** A recorded tree that is gone, cannot be probed, is not
//! a linked worktree, or whose git registration is missing DENIES; so does a
//! record that exists but cannot be read or parsed (ADR-0045: undeterminable
//! is not absent). Three cases allow: a recipient with no record (a teammate
//! name, or an agent this session did not dispatch), a record showing no
//! worktree, and a subagent caller, whose `SendMessage` is its report-back
//! channel. A payload with no `transcript_path` ALLOWS with a stderr warning
//! naming the agent — the owner ruling's "daemon unreachable" arm, applied to
//! the record source this rule actually uses.
//!
//! Test: the `tests` module below;
//! `pm_guard_refuses_resuming_an_agent_whose_worktree_was_reclaimed_8004` and
//! siblings in `tests/tm_hook_pm_guard.rs` run it through the real binary.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::project_aliases::worktree_root;

/// The harness tool that resumes a stopped agent.
const SEND_MESSAGE_TOOL: &str = "SendMessage";

/// `tool_input` keys that may name the recipient, in precedence order.
const RECIPIENT_KEYS: [&str; 3] = ["to", "agent_id", "agentId"];

/// Upper bound on bytes read from any harness file, so a hook stays bounded.
const READ_CAP: u64 = 1024 * 1024;

/// What the #8004 rule decided for one `PreToolUse` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeVerdict {
    /// Not this rule's case, or the agent's tree is live.
    Allow,
    /// No record could be located; allowed, and the caller warns on stderr.
    Unchecked { agent: String, why: String },
    /// Refused, with the reason the PM reads.
    Deny(String),
}

/// What the harness record says about an agent's worktree.
#[derive(Debug, PartialEq, Eq)]
enum Record {
    NotIsolated,
    Removed,
    Tree {
        path: PathBuf,
        branch: Option<String>,
    },
    Undeterminable(String),
}

/// Classify a `PreToolUse` call for the #8004 resume rule.
///
/// Why: see the module doc — the one enforceable half of "a resumed agent
/// never lands in the main checkout".
/// What: [`ResumeVerdict::Allow`] unless `tool_name` is `SendMessage`, the
/// caller is the PM, and the recipient's harness record shows a worktree that
/// is gone or cannot be confirmed; then [`ResumeVerdict::Deny`].
/// Test: `denies_a_resume_after_the_harness_removed_the_tree`,
/// `denies_when_the_tree_probe_errors`, `allows_a_live_linked_worktree`.
pub(crate) fn evaluate_resume_worktree(
    tool_name: &str,
    payload: &Value,
    caller_is_subagent: bool,
) -> ResumeVerdict {
    if tool_name != SEND_MESSAGE_TOOL || caller_is_subagent {
        return ResumeVerdict::Allow;
    }
    let Some(agent) = recipient_agent_id(payload.get("tool_input")) else {
        return ResumeVerdict::Allow;
    };
    let transcript = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(record) = transcript.and_then(|t| agent_record_path(Path::new(t), &agent)) else {
        let why = "the hook payload names no usable transcript_path".to_string();
        return ResumeVerdict::Unchecked { agent, why };
    };
    let bytes = match read_capped(&record) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ResumeVerdict::Allow,
        Err(e) => {
            let why = format!(
                "its harness record `{}` could not be read ({e})",
                record.display()
            );
            return ResumeVerdict::Deny(deny_reason(&agent, None, None, &unconfirmed(&why)));
        }
    };
    match classify_record(&bytes) {
        Record::NotIsolated => ResumeVerdict::Allow,
        Record::Removed => {
            let tree = tree_from_transcript(&record, &agent);
            let branch = tree.as_deref().and_then(harness_branch_name);
            let state =
                "was removed by the harness when the agent stopped, because it held no changes";
            ResumeVerdict::Deny(deny_reason(
                &agent,
                tree.as_deref(),
                branch.as_deref(),
                state,
            ))
        }
        Record::Tree { path, branch } => match probe_tree(&path) {
            Ok(()) => ResumeVerdict::Allow,
            Err(state) => {
                ResumeVerdict::Deny(deny_reason(&agent, Some(&path), branch.as_deref(), &state))
            }
        },
        Record::Undeterminable(why) => {
            let why = format!("its harness record `{}` {why}", record.display());
            ResumeVerdict::Deny(deny_reason(&agent, None, None, &unconfirmed(&why)))
        }
    }
}

/// The recipient as a harness agent id, or `None` when it cannot be one.
///
/// Why: the id becomes a file name, so anything path-shaped is rejected rather
/// than joined. The harness names an agent `a<hex>`; an `agent-` prefix is
/// tolerated because the worktree and record carry it.
fn recipient_agent_id(tool_input: Option<&Value>) -> Option<String> {
    let input = tool_input?;
    let raw = RECIPIENT_KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))?
        .trim();
    let id = raw.strip_prefix("agent-").unwrap_or(raw);
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| id.to_string())
}

/// `<dir>/<session>/subagents/agent-<id>.meta.json` for `<dir>/<session>.jsonl`.
fn agent_record_path(transcript: &Path, agent: &str) -> Option<PathBuf> {
    let session_dir = transcript.parent()?.join(transcript.file_stem()?);
    Some(
        session_dir
            .join("subagents")
            .join(format!("agent-{agent}.meta.json")),
    )
}

/// Read at most [`READ_CAP`] bytes of `path`.
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(READ_CAP)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Classify the harness record's JSON (see the module doc for the two shapes).
fn classify_record(bytes: &[u8]) -> Record {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(bytes) else {
        return Record::Undeterminable("is not a JSON object".into());
    };
    if obj.get("worktreeCleanlyRemoved").and_then(Value::as_bool) == Some(true) {
        return Record::Removed;
    }
    match obj.get("worktreePath") {
        Some(Value::String(p)) if !p.is_empty() => Record::Tree {
            path: PathBuf::from(p),
            branch: obj
                .get("worktreeBranch")
                .and_then(Value::as_str)
                .map(String::from),
        },
        Some(_) => Record::Undeterminable("carries a `worktreePath` that is not a path".into()),
        None if obj.get("spawnedWithWorktree").and_then(Value::as_bool) == Some(true)
            || obj.contains_key("worktreeCleanlyRemoved") =>
        {
            Record::Undeterminable("records a worktree but no path for it".into())
        }
        None => Record::NotIsolated,
    }
}

/// `Ok` only when `tree` is a directory holding a linked-worktree `.git`
/// pointer whose git directory exists; otherwise the state, as a phrase.
///
/// Why: every failure to confirm the tree reads as missing (#8004 fail-closed).
fn probe_tree(tree: &Path) -> Result<(), String> {
    match std::fs::metadata(tree) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err("is no longer a directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("is gone".into()),
        Err(e) => return Err(unconfirmed(&format!("probing it failed ({e})"))),
    }
    let dotgit = tree.join(".git");
    let pointer = match std::fs::symlink_metadata(&dotgit) {
        Ok(m) if m.is_dir() => return Err("is a main checkout, not a linked worktree".into()),
        Ok(m) if m.is_file() => read_capped(&dotgit)
            .map_err(|e| unconfirmed(&format!("its `.git` pointer could not be read ({e})")))?,
        Ok(_) => return Err(unconfirmed("its `.git` is neither a file nor a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("is no longer a git worktree (no `.git`)".into());
        }
        Err(e) => return Err(unconfirmed(&format!("probing its `.git` failed ({e})"))),
    };
    let text = String::from_utf8_lossy(&pointer);
    let Some(gitdir) = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(unconfirmed("its `.git` pointer names no gitdir"));
    };
    match std::fs::metadata(tree.join(gitdir)) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(unconfirmed("its gitdir is not a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("is no longer registered with git".into())
        }
        Err(e) => Err(unconfirmed(&format!("probing its gitdir failed ({e})"))),
    }
}

/// The state phrase for a tree that could not be confirmed to exist.
fn unconfirmed(why: &str) -> String {
    format!("cannot be confirmed to exist, so it is treated as gone: {why}")
}

/// The removed tree's path, from the first line of the agent's transcript.
///
/// Why: the harness drops `worktreePath` when it removes the tree, so the
/// transcript's recorded `cwd` is the only place the path survives. Used for
/// the deny text only; the verdict never depends on it.
fn tree_from_transcript(record: &Path, agent: &str) -> Option<PathBuf> {
    let transcript = record.with_file_name(format!("agent-{agent}.jsonl"));
    let file = std::fs::File::open(transcript).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file.take(READ_CAP))
        .read_line(&mut line)
        .ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    worktree_root(Path::new(v.get("cwd")?.as_str()?))
}

/// The branch the harness creates for `tree`: `worktree-<tree dir name>`.
fn harness_branch_name(tree: &Path) -> Option<String> {
    Some(format!("worktree-{}", tree.file_name()?.to_str()?))
}

/// The deny text: the agent, the missing tree and branch, and the remedy.
fn deny_reason(agent: &str, tree: Option<&Path>, branch: Option<&str>, state: &str) -> String {
    let tree = tree.map_or_else(String::new, |p| format!(" `{}`", p.display()));
    let branch = branch.map_or_else(String::new, |b| {
        format!(" Its branch was `{b}`; name it in the brief if it holds commits to keep.")
    });
    format!(
        "Resume refused (#8004): agent `{agent}` was dispatched with `isolation: \"worktree\"`, \
         and its worktree{tree} {state}. A SendMessage now would resume it in the main \
         checkout, where it cannot commit (ADR-0061). Do not resume it: re-dispatch fresh with \
         `isolation: \"worktree\"` and restate the base commit and the task in the brief.{branch}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AGENT: &str = "a30fc1be077e90fa4";

    /// A session directory with the transcript path the payload names.
    struct Session {
        _dir: tempfile::TempDir,
        root: PathBuf,
        transcript: PathBuf,
    }

    impl Session {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let transcript = root.join("sess-1.jsonl");
            std::fs::create_dir_all(root.join("sess-1/subagents")).expect("mkdir");
            Self {
                _dir: dir,
                root,
                transcript,
            }
        }

        fn record(&self, meta: &str) {
            let path = self
                .root
                .join(format!("sess-1/subagents/agent-{AGENT}.meta.json"));
            std::fs::write(path, meta).expect("write record");
        }

        fn payload(&self) -> Value {
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "SendMessage",
                "transcript_path": self.transcript,
                "tool_input": {"to": AGENT, "message": "continue"},
            })
        }

        fn verdict(&self) -> ResumeVerdict {
            evaluate_resume_worktree("SendMessage", &self.payload(), false)
        }

        /// A linked worktree at `<root>/.claude/worktrees/agent-<id>`.
        fn linked_tree(&self) -> PathBuf {
            let tree = self.root.join(format!(".claude/worktrees/agent-{AGENT}"));
            let gitdir = self.root.join(format!(".git/worktrees/agent-{AGENT}"));
            std::fs::create_dir_all(&tree).expect("mkdir tree");
            std::fs::create_dir_all(&gitdir).expect("mkdir gitdir");
            std::fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display()))
                .expect("write .git");
            tree
        }
    }

    fn denial(v: ResumeVerdict) -> String {
        match v {
            ResumeVerdict::Deny(reason) => reason,
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    /// #8004: the reported shape — the harness removed the clean tree.
    #[test]
    fn denies_a_resume_after_the_harness_removed_the_tree() {
        let s = Session::new();
        s.record(r#"{"agentType":"rust-engineer","worktreeCleanlyRemoved":true}"#);
        let tree = s.root.join(format!(".claude/worktrees/agent-{AGENT}"));
        let first = json!({"agentId": AGENT, "cwd": tree});
        let transcript = s.root.join(format!("sess-1/subagents/agent-{AGENT}.jsonl"));
        std::fs::write(transcript, format!("{first}\n")).expect("write transcript");
        let reason = denial(s.verdict());
        assert!(reason.contains(&tree.display().to_string()), "{reason}");
        assert!(
            reason.contains(&format!("worktree-agent-{AGENT}")),
            "{reason}"
        );
        assert!(reason.contains("re-dispatch fresh"), "{reason}");
    }

    #[test]
    fn denies_a_resume_whose_recorded_tree_is_gone() {
        let s = Session::new();
        let gone = s.root.join("gone-tree");
        s.record(&json!({"worktreePath": gone, "worktreeBranch": "worktree-x"}).to_string());
        let reason = denial(s.verdict());
        assert!(
            reason.contains("is gone") && reason.contains("worktree-x"),
            "{reason}"
        );
    }

    /// #8004 fail-closed: a probe error is not "present".
    #[test]
    fn denies_when_the_tree_probe_errors() {
        let s = Session::new();
        let file = s.root.join("plain-file");
        std::fs::write(&file, "x").expect("write file");
        s.record(&json!({"worktreePath": file.join("child")}).to_string());
        let reason = denial(s.verdict());
        assert!(reason.contains("cannot be confirmed"), "{reason}");
    }

    #[test]
    fn denies_a_recorded_tree_that_is_a_main_checkout() {
        let s = Session::new();
        let tree = s.root.join("checkout");
        std::fs::create_dir_all(tree.join(".git")).expect("mkdir .git");
        s.record(&json!({"worktreePath": tree}).to_string());
        assert!(denial(s.verdict()).contains("main checkout, not a linked worktree"));
    }

    #[test]
    fn denies_a_tree_git_no_longer_registers() {
        let s = Session::new();
        let tree = s.linked_tree();
        std::fs::remove_dir_all(s.root.join(".git")).expect("drop gitdir");
        s.record(&json!({"worktreePath": tree}).to_string());
        assert!(denial(s.verdict()).contains("no longer registered with git"));
    }

    #[test]
    fn denies_an_unreadable_or_incomplete_record() {
        for meta in [
            "not json",
            r#"{"spawnedWithWorktree":true}"#,
            r#"{"worktreePath":7}"#,
        ] {
            let s = Session::new();
            s.record(meta);
            assert!(
                denial(s.verdict()).contains("cannot be confirmed"),
                "{meta}"
            );
        }
    }

    #[test]
    fn allows_a_live_linked_worktree() {
        let s = Session::new();
        let tree = s.linked_tree();
        s.record(&json!({"worktreePath": tree, "spawnedWithWorktree": true}).to_string());
        assert_eq!(s.verdict(), ResumeVerdict::Allow);
    }

    #[test]
    fn allows_an_agent_never_given_a_worktree_or_never_recorded() {
        let s = Session::new();
        assert_eq!(s.verdict(), ResumeVerdict::Allow, "no record");
        s.record(r#"{"agentType":"research","spawnDepth":1}"#);
        assert_eq!(s.verdict(), ResumeVerdict::Allow, "not isolated");
    }

    #[test]
    fn allows_a_subagent_caller_and_every_other_tool() {
        let s = Session::new();
        s.record(r#"{"worktreeCleanlyRemoved":true}"#);
        let p = s.payload();
        assert_eq!(
            evaluate_resume_worktree("SendMessage", &p, true),
            ResumeVerdict::Allow
        );
        assert_eq!(
            evaluate_resume_worktree("Agent", &p, false),
            ResumeVerdict::Allow
        );
    }

    #[test]
    fn unchecked_when_the_payload_names_no_transcript() {
        let p = json!({"tool_name": "SendMessage", "tool_input": {"to": AGENT}});
        assert!(matches!(
            evaluate_resume_worktree("SendMessage", &p, false),
            ResumeVerdict::Unchecked { agent, .. } if agent == AGENT
        ));
    }

    #[test]
    fn recipient_rejects_path_shaped_names() {
        for to in ["../x", "a/b", "", "a b"] {
            assert_eq!(recipient_agent_id(Some(&json!({"to": to}))), None, "{to}");
        }
        let id = recipient_agent_id(Some(&json!({"agent_id": format!("agent-{AGENT}")})));
        assert_eq!(id.as_deref(), Some(AGENT));
    }
}
