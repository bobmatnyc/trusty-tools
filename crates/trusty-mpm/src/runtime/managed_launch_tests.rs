//! Tests for the parameterized managed launch (#8233).
//!
//! Why: two claims need holding. (1) The line typed into the pane is FIXED in
//! shape — its length does not grow with the cwd, the env, the flags or the
//! prompt — because that growth is what crossed `MAX_CANON` and killed session
//! `dd0e2fb8-…`. (2) The `claude` the OS ends up running has the same cwd,
//! environment and argv the shell line used to produce, asserted against the
//! resolved parameters rather than by matching a string.
//! What: hermetic and pure except the three `deliver_*` tests, which write into
//! a `TempDir` and a recording fake driver.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::super::launch_spec::LaunchSpec;
use super::*;
use crate::session_manager::{ManagedError, ManagedTmuxDriver};

/// A deliberately long path, as a deep managed worktree really produces.
fn long_path(prefix: &str, segments: usize) -> PathBuf {
    let mut p = PathBuf::from(prefix);
    for i in 0..segments {
        p.push(format!("an-uncomfortably-long-path-segment-{i:03}"));
    }
    p
}

/// The widest launch this crate can produce: a deep cwd, a deep prompt path
/// under a long `TMPDIR`, a relocated config dir, an OAuth token, both MCP pins
/// and a pinned `gh` identity — every optional piece present at once.
struct Worst {
    cwd: PathBuf,
    config_dir: PathBuf,
    prompt: PathBuf,
    mcp_env: Vec<(String, String)>,
    gh_env: Vec<(String, String)>,
    oauth_token: String,
}

impl Worst {
    fn new() -> Self {
        Self {
            cwd: long_path("/Users/an-operator-with-a-long-name/trusty-mpm-projects", 5),
            config_dir: long_path("/Users/an-operator-with-a-long-name/.trusty-tools", 3),
            prompt: long_path("/var/folders/qv/8k3p1x9n5cl7g2vy_0000gn/T", 4)
                .join("trusty-mpm-prompt.txt"),
            mcp_env: vec![
                (
                    "TRUSTY_MEMORY_PALACE".to_owned(),
                    long_path("/palaces", 3).display().to_string(),
                ),
                (
                    "TRUSTY_INDEX".to_owned(),
                    long_path("/indexes", 3).display().to_string(),
                ),
            ],
            gh_env: vec![
                ("GH_TOKEN".to_owned(), "gho_".to_owned() + &"x".repeat(60)),
                (
                    "GH_USER".to_owned(),
                    "an-operator-with-a-long-name".to_owned(),
                ),
                (
                    "GH_CONFIG_DIR".to_owned(),
                    long_path("/gh-configs", 3).display().to_string(),
                ),
            ],
            oauth_token: "sk-ant-oat01-".to_owned() + &"y".repeat(96),
        }
    }

    fn launch(&self) -> ManagedLaunch<'_> {
        ManagedLaunch {
            cwd: &self.cwd,
            claude_bin: "/Users/an-operator-with-a-long-name/.local/bin/claude",
            config_dir: Some(&self.config_dir),
            session_id: "dd0e2fb8-1111-2222-3333-444455556666",
            prompt_file: Some(&self.prompt),
            oauth_token: Some(&self.oauth_token),
            gh_env: &self.gh_env,
            mcp_env: &self.mcp_env,
            memory_reachable: true,
        }
    }
}

/// A minimal launch with every optional piece absent.
fn bare_launch<'a>(cwd: &'a Path, gh_env: &'a [(String, String)]) -> ManagedLaunch<'a> {
    ManagedLaunch {
        cwd,
        claude_bin: "/abs/claude",
        config_dir: None,
        session_id: "11111111-2222-3333-4444-555555555555",
        prompt_file: None,
        oauth_token: None,
        gh_env,
        mcp_env: &[],
        memory_reachable: false,
    }
}

/// Look up one assignment in an `env_set` list.
fn value_of<'a>(set: &'a [(String, String)], name: &str) -> Option<&'a str> {
    set.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------- environment

#[test]
fn env_unset_leads_with_the_api_key() {
    // DOC-34: with ANTHROPIC_API_KEY stripped the session falls back to OAuth.
    assert_eq!(managed_env_unset(&[])[0], "ANTHROPIC_API_KEY");
}

#[test]
fn env_unset_carries_every_inherited_session_marker() {
    // #4467: an inherited CLAUDE_CODE_CHILD_SESSION disables transcript saving,
    // costing the session its native --resume/--continue//rewind recovery.
    let unset = managed_env_unset(&[]);
    for marker in crate::core::claude_env_scrub::INHERITED_SESSION_MARKERS {
        assert!(
            unset.iter().any(|n| n == marker),
            "missing {marker}: {unset:?}"
        );
    }
}

#[test]
fn env_unset_clears_an_inherited_gh_token() {
    // #6668: gh reads an env token before a config dir, so a pinned identity
    // binds nothing while an inherited GH_TOKEN survives.
    let gh_env = vec![("GH_CONFIG_DIR".to_owned(), "/gh".to_owned())];
    let unset = managed_env_unset(&gh_env);
    assert!(
        unset.iter().any(|n| n == "GH_TOKEN"),
        "a pinned gh identity must clear an inherited token: {unset:?}"
    );
}

#[test]
fn env_unset_clears_nothing_without_a_gh_identity_binding() {
    // An unconfigured project must keep the pre-#6668 environment exactly.
    let unset = managed_env_unset(&[]);
    assert!(
        !unset.iter().any(|n| n == "GH_TOKEN"),
        "no identity binding must clear nothing: {unset:?}"
    );
}

#[test]
fn env_set_enables_todo_tools_unconditionally() {
    // #8066: nothing about the host decides this one.
    for reachable in [true, false] {
        let set = managed_env_set(None, None, &[], reachable, &[]);
        assert_eq!(value_of(&set, "CLAUDE_CODE_ENABLE_TODO_TOOLS"), Some("1"));
    }
}

#[test]
fn env_set_disables_auto_memory_when_trusty_memory_answers() {
    let set = managed_env_set(None, None, &[], true, &[]);
    assert_eq!(value_of(&set, "CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some("1"));
}

#[test]
fn env_set_keeps_auto_memory_when_trusty_memory_is_unreachable() {
    // #7685 / owner ruling 2026-09-12: auto memory is the FALLBACK, so a session
    // must not lose it while trusty-memory is down.
    let set = managed_env_set(None, None, &[], false, &[]);
    assert_eq!(value_of(&set, "CLAUDE_CODE_DISABLE_AUTO_MEMORY"), None);
}

#[test]
fn env_set_relocates_the_config_dir() {
    // #4451/#4455: this relocation is what puts the bundled agent roster in the
    // `user` settings tier the spawn's --setting-sources flag loads.
    let dir = PathBuf::from("/managed/claude-config");
    let set = managed_env_set(Some(&dir), None, &[], true, &[]);
    assert_eq!(
        value_of(&set, "CLAUDE_CONFIG_DIR"),
        Some("/managed/claude-config")
    );
}

#[test]
fn env_set_carries_a_non_empty_mcp_env() {
    // #4181: the per-project pins the shared user-scope declarations cannot
    // carry as arguments.
    let mcp = vec![("TRUSTY_INDEX".to_owned(), "idx".to_owned())];
    let set = managed_env_set(None, None, &mcp, true, &[]);
    assert_eq!(value_of(&set, "TRUSTY_INDEX"), Some("idx"));
}

#[test]
fn env_set_carries_the_oauth_token_when_available() {
    // #2246: bypasses the CLAUDE_CONFIG_DIR-keyed Keychain divergence.
    let set = managed_env_set(None, Some("tok"), &[], true, &[]);
    assert_eq!(
        value_of(&set, crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR),
        Some("tok")
    );
}

#[test]
fn env_set_omits_the_oauth_token_when_absent() {
    let set = managed_env_set(None, None, &[], true, &[]);
    assert_eq!(
        value_of(&set, crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR),
        None
    );
}

#[test]
fn env_set_carries_the_gh_identity() {
    // #3025: the pinned account must reach `claude`'s environment — it just no
    // longer travels through a sourced temp file to get there.
    let gh = vec![("GH_TOKEN".to_owned(), "gho_x".to_owned())];
    let set = managed_env_set(None, None, &[], true, &gh);
    assert_eq!(value_of(&set, "GH_TOKEN"), Some("gho_x"));
}

// ------------------------------------------------------------------ argv/spec

#[test]
fn spawn_argv_matches_the_shell_line_it_replaces() {
    // #8233 requirement 3: the argv must be the SAME token sequence the shell
    // line produced, read from the same production constants rather than
    // restated — prompt flag, setting sources, additive --mcp-config,
    // permission mode, in that order.
    let w = Worst::new();
    let launch = w.launch();
    let spec = spawn_spec(&launch);

    let mut expected = vec![
        "--append-system-prompt-file".to_owned(),
        w.prompt.display().to_string(),
    ];
    expected.extend(
        crate::core::model_inject::setting_sources_flag(Some(&w.config_dir))
            .split_whitespace()
            .map(str::to_owned),
    );
    expected.extend(crate::core::session_mcp_scope::mcp_config_argv(
        crate::core::session_mcp_scope::scoped_for(&w.cwd, Some(&w.config_dir)).as_deref(),
    ));
    expected.extend(
        crate::core::model_inject::PERMISSION_MODE_FLAG
            .split_whitespace()
            .map(str::to_owned),
    );
    assert_eq!(spec.args, expected);
}

#[test]
fn spawn_argv_quotes_nothing() {
    // These tokens reach execv with no shell in between, so a single-quoted path
    // would name a file `claude` cannot open.
    let w = Worst::new();
    let spec = spawn_spec(&w.launch());
    assert!(
        spec.args.iter().all(|a| !a.contains('\'')),
        "argv tokens must be bare, not shell-quoted: {:?}",
        spec.args
    );
}

#[test]
fn spawn_spec_roots_the_child_at_the_workspace() {
    // #2250: the launch must be rooted at the workspace regardless of where the
    // pane shell actually started. The `cd` prefix became the child's own cwd.
    let w = Worst::new();
    let spec = spawn_spec(&w.launch());
    assert_eq!(spec.cwd, w.cwd);
    assert_eq!(
        spec.program,
        "/Users/an-operator-with-a-long-name/.local/bin/claude"
    );
}

#[test]
fn resume_spec_appends_the_resume_flag() {
    let cwd = PathBuf::from("/w");
    let launch = bare_launch(&cwd, &[]);
    let spec = resume_spec(&launch, Some("conv-123"));
    let tail = &spec.args[spec.args.len() - 2..];
    assert_eq!(tail, ["--resume".to_owned(), "conv-123".to_owned()]);
}

#[test]
fn resume_spec_without_an_id_is_a_plain_spawn() {
    // #6765: no `--continue` fallback — anything but a usable id launches fresh.
    let cwd = PathBuf::from("/w");
    let launch = bare_launch(&cwd, &[]);
    assert_eq!(resume_spec(&launch, None).args, spawn_spec(&launch).args);
    assert!(
        !resume_spec(&launch, None)
            .args
            .iter()
            .any(|a| a == "--continue")
    );
}

#[test]
fn attach_spec_omits_flags_attach_cannot_take() {
    // #6863: `claude attach <id>` accepts no options; passing them makes claude
    // reject the invocation.
    let w = Worst::new();
    let spec = attach_spec(&w.launch(), "short1");
    assert_eq!(spec.args, ["attach".to_owned(), "short1".to_owned()]);
}

#[test]
fn attach_and_resume_share_an_identical_environment() {
    // #6863: an attached session must not lose anything a resumed one carries.
    let w = Worst::new();
    let launch = w.launch();
    let attach = attach_spec(&launch, "short1");
    let resume = resume_spec(&launch, Some("conv"));
    assert_eq!(attach.env_unset, resume.env_unset);
    assert_eq!(attach.env_set, resume.env_set);
    assert_eq!(attach.cwd, resume.cwd);
    assert_eq!(attach.program, resume.program);
    assert_eq!(attach.session_id, resume.session_id);
}

// ------------------------------------------------------------------ pane line

#[test]
fn pane_line_exports_the_managed_session_id() {
    // #2023 component B: the export lands in the PANE's own shell, so it
    // survives claude exiting and a bare `tm` there can identify the session.
    let line = pane_line("sess-id", "/usr/local/bin/tm", Path::new("/specs/a.json"));
    assert!(
        line.starts_with("export TM_MANAGED_SESSION_ID='sess-id'; "),
        "{line}"
    );
}

#[test]
fn pane_line_routes_through_the_disclaim_shim() {
    // #2997: claude must be spawned by the shim, never by tmux or the shell.
    let line = pane_line("s", "/usr/local/bin/tm", Path::new("/specs/a.json"));
    assert!(
        line.contains(
            "'/usr/local/bin/tm' internal-spawn-disclaimed --launch-spec '/specs/a.json'"
        ),
        "{line}"
    );
}

#[test]
fn pane_line_is_short_for_the_worst_case_launch() {
    // THE regression assertion (#8233). Build the widest launch this crate can
    // produce — a 5-segment cwd, a prompt under a long macOS TMPDIR, a relocated
    // config dir, an OAuth token, both MCP pins and a three-variable gh identity
    // — and prove the typed line is nowhere near the tty's canonical-input
    // limit. The spec is asserted to be LARGE in the same breath: that is the
    // material the line used to carry, and a line that stayed short because the
    // parameters vanished would be a different bug.
    let w = Worst::new();
    let spec = spawn_spec(&w.launch());
    let spec_json = serde_json::to_string(&spec).expect("spec serialises");
    assert!(
        spec_json.len() > 1_024,
        "the worst case must genuinely be large: {} bytes",
        spec_json.len()
    );

    // A deliberately long wrapper path and a spec path under the real root.
    let wrapper = long_path("/Users/an-operator-with-a-long-name/.cargo/bin", 2)
        .join("tm")
        .display()
        .to_string();
    let spec_path = LaunchSpec::root()
        .unwrap_or_else(|| {
            PathBuf::from(
                "/Users/an-operator-with-a-long-name/.trusty-tools/trusty-mpm/launch-specs",
            )
        })
        .join(format!("{}.json", uuid::Uuid::new_v4()));

    let line = pane_line(&spec.session_id, &wrapper, &spec_path);
    assert!(
        line.len() < 512,
        "the typed line must stay under 512 bytes; got {} for {line}",
        line.len()
    );
    assert!(
        line.len() < crate::core::tmux::MAX_PANE_COMMAND_BYTES,
        "the typed line must pass the pane-command guard: {} bytes",
        line.len()
    );

    // And it must not grow with the launch: the bare launch's line is the same
    // length, because the only variable parts are the UUID and the two paths.
    let bare = pane_line(&spec.session_id, &wrapper, &spec_path);
    assert_eq!(
        line.len(),
        bare.len(),
        "the typed line's length must be independent of the launch's contents"
    );
}

#[test]
fn abort_notice_is_short_enough_to_always_type() {
    // The abort notice exists for the case "the previous line was too long to
    // type", so it must be far under the guard.
    let notice = abort_notice("dd0e2fb8-1111-2222-3333-444455556666");
    assert!(
        notice.len() < 256,
        "the abort notice must always fit: {} bytes",
        notice.len()
    );
    assert!(notice.contains("aborted"), "{notice}");
}

// ------------------------------------------------------------------- delivery

/// A recording driver whose sends succeed, or fail on the first `n` attempts.
struct Recorder {
    sends: Mutex<Vec<String>>,
    fail_first: usize,
    /// #8233: every pane reset, as `(session, pane_id)` — the interrupt the
    /// handshake sends to flush a wedged parser.
    interrupts: Mutex<Vec<(String, Option<String>)>>,
    /// #8233: what this fake shell has PRINTED. `deliver` now confirms the pane
    /// executes what it is typed before it types a launch, so a double that
    /// prints nothing is an unresponsive shell and every delivery test would
    /// fail for a reason the double invented. Answering the probe — and only
    /// the probe — makes this a shell that works.
    pane_text: Mutex<String>,
    /// #8233: every reset and every typed line IN ORDER, as `reset:<pane>` and
    /// `line:<text>`. Two separate logs cannot show that the interrupt happened
    /// BEFORE the keystrokes, which is the whole property the reset has —
    /// flushing a wedged parser after typing into it protects nothing.
    events: Mutex<Vec<String>>,
    /// #8233: an interrupt does NOT free this pane — the refusal shape.
    stays_wedged: bool,
}

impl Recorder {
    fn new(fail_first: usize) -> Self {
        Self {
            sends: Mutex::new(Vec::new()),
            fail_first,
            interrupts: Mutex::new(Vec::new()),
            pane_text: Mutex::new("~ %".to_owned()),
            events: Mutex::new(Vec::new()),
            stays_wedged: false,
        }
    }

    /// A Recorder whose pane is WEDGED at a `quote>` continuation prompt until
    /// it is interrupted — the live `tm-apex-companion` state.
    fn wedged() -> Self {
        let r = Self::new(0);
        *r.pane_text.lock().expect("recorder mutex") = "quote>".to_owned();
        r
    }

    /// A Recorder whose pane stays wedged however often it is interrupted —
    /// the shape the handshake must REFUSE rather than type into (#8233).
    fn wedged_forever() -> Self {
        let mut r = Self::wedged();
        r.stays_wedged = true;
        r
    }

    /// Every reset and typed line, in the order they happened.
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("recorder mutex").clone()
    }

    /// Answer a probe line as a working shell would, without recording it, so
    /// `lines()` still holds exactly the launch lines it always did.
    fn answer_probe(&self, text: &str) -> bool {
        let Some(out) = super::super::pane_handshake::probe_reply(text) else {
            return false;
        };
        let mut pane = self.pane_text.lock().expect("recorder mutex");
        // A wedged parser swallows the probe as more of its open construct, so
        // nothing is printed until an interrupt has cleared it.
        if pane.trim_end().ends_with("quote>") {
            return true;
        }
        pane.push('\n');
        pane.push_str(&out);
        true
    }
    fn lines(&self) -> Vec<String> {
        self.sends.lock().expect("recorder mutex").clone()
    }
    fn resets(&self) -> Vec<(String, Option<String>)> {
        self.interrupts.lock().expect("recorder mutex").clone()
    }
}

impl ManagedTmuxDriver for Recorder {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, text: &str) -> Result<(), ManagedError> {
        if self.answer_probe(text) {
            return Ok(());
        }
        self.events
            .lock()
            .expect("recorder mutex")
            .push(format!("line:{text}"));
        let mut log = self.sends.lock().expect("recorder mutex");
        log.push(text.to_owned());
        if log.len() <= self.fail_first {
            return Err(ManagedError::TmuxUnavailable("pane is gone".into()));
        }
        Ok(())
    }
    fn send_line_to_pane(&self, _name: &str, _pane: &str, text: &str) -> Result<(), ManagedError> {
        if self.answer_probe(text) {
            return Ok(());
        }
        self.events
            .lock()
            .expect("recorder mutex")
            .push(format!("line:{text}"));
        let mut log = self.sends.lock().expect("recorder mutex");
        log.push(text.to_owned());
        if log.len() <= self.fail_first {
            return Err(ManagedError::TmuxUnavailable("pane is gone".into()));
        }
        Ok(())
    }
    fn send_interrupt(&self, name: &str) -> Result<(), ManagedError> {
        self.events
            .lock()
            .expect("recorder mutex")
            .push("reset:session".to_owned());
        if !self.stays_wedged {
            *self.pane_text.lock().expect("recorder mutex") = "~ %".to_owned();
        }
        self.interrupts
            .lock()
            .expect("recorder mutex")
            .push((name.to_owned(), None));
        Ok(())
    }
    fn send_interrupt_to_pane(&self, name: &str, pane: &str) -> Result<(), ManagedError> {
        self.events
            .lock()
            .expect("recorder mutex")
            .push(format!("reset:{pane}"));
        if !self.stays_wedged {
            *self.pane_text.lock().expect("recorder mutex") = "~ %".to_owned();
        }
        self.interrupts
            .lock()
            .expect("recorder mutex")
            .push((name.to_owned(), Some(pane.to_owned())));
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(self.pane_text.lock().expect("recorder mutex").clone())
    }
    fn capture_pane(&self, name: &str, _pane: &str, lines: usize) -> Result<String, ManagedError> {
        self.capture(name, lines)
    }
    /// #8233 round 3, finding 4: the pre-launch handshake asks observability
    /// first, so a double that models a live pane must name its own session —
    /// the one every `deliver_*` test below launches into.
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(vec!["tm-sess".to_owned()])
    }
}

#[test]
fn deliver_types_a_short_line_and_leaves_a_readable_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let w = Worst::new();
    let spec = spawn_spec(&w.launch());
    let tmux = Recorder::new(0);

    deliver_in(&tmux, "tm-sess", None, &spec, dir.path()).expect("deliver");

    let lines = tmux.lines();
    assert_eq!(lines.len(), 1, "exactly one line is typed: {lines:?}");
    assert!(
        lines[0].len() < 512,
        "typed {} bytes: {}",
        lines[0].len(),
        lines[0]
    );

    // The spec the line names must still be there for the shim to consume.
    // #8233: the directory also holds this session's `<id>.launch` pointer,
    // which names the launch the sentinel is keyed on. Specs are the `.json`.
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    assert_eq!(written.len(), 1, "one spec per launch: {written:?}");
    assert_eq!(LaunchSpec::verify(&written[0]).expect("verify"), spec);
}

/// #8233 review (defence in depth, independent of line length): the length
/// guard makes THIS line un-truncatable, but it cannot undo a pane an earlier
/// launch already wedged. A shell at a PS2 continuation prompt swallows
/// whatever is typed next as more of the open construct, so a short line would
/// vanish into it just as the long one did. The reset must happen BEFORE the
/// keystrokes, or it protects nothing — which is why this asserts the ORDER,
/// not merely that both happened.
#[test]
fn deliver_resets_the_pane_before_typing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    let tmux = Recorder::wedged();

    deliver_in(&tmux, "tm-sess", None, &spec, dir.path()).expect("deliver");

    assert_eq!(
        tmux.resets(),
        vec![("tm-sess".to_owned(), None)],
        "the wedged pane must be interrupted exactly once, session-scoped with no pane id"
    );
    let events = tmux.events();
    let reset_at = events
        .iter()
        .position(|e| e.starts_with("reset:"))
        .expect("the pane was reset");
    let line_at = events
        .iter()
        .position(|e| e.starts_with("line:"))
        .expect("the launch line was typed");
    assert!(
        reset_at < line_at,
        "the reset must precede the keystrokes, or the open construct swallows them: {events:?}"
    );
    assert_eq!(tmux.lines().len(), 1, "and then the launch line is typed");
}

/// #2456: the reset must land on the RECORD's pane, not on whichever pane tmux
/// currently considers active — a session-scoped `C-c` would interrupt a
/// sibling window's work.
#[test]
fn deliver_resets_the_named_pane_when_one_is_known() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    let tmux = Recorder::wedged();

    deliver_in(&tmux, "tm-sess", Some("%9"), &spec, dir.path()).expect("deliver");

    assert_eq!(
        tmux.resets(),
        vec![("tm-sess".to_owned(), Some("%9".to_owned()))]
    );
    let events = tmux.events();
    assert!(
        events.iter().any(|e| e == "reset:%9"),
        "the interrupt must name the pane, never the session: {events:?}"
    );
    let reset_at = events.iter().position(|e| e == "reset:%9").expect("reset");
    let line_at = events
        .iter()
        .position(|e| e.starts_with("line:"))
        .expect("launch line");
    assert!(reset_at < line_at, "{events:?}");
}

/// #8233 review round 2 (finding 3): the post-send handshake reads a sentinel
/// keyed on the LAUNCH. One left by an EARLIER launch of the same session used
/// to make a stuck pane look healthy; now it names a different launch id, so it
/// cannot satisfy this one no matter how long it survives.
#[test]
fn deliver_clears_a_stale_started_sentinel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    std::fs::create_dir_all(dir.path()).expect("mkdir");
    // An earlier launch of this same session ran and left its sentinel behind.
    let stale_launch_id = "0000staleaaaabbbbccccddddeeee1111";
    let stale = LaunchSpec::started_marker_in(dir.path(), stale_launch_id);
    std::fs::write(&stale, b"").expect("plant a stale sentinel");

    deliver_in(&Recorder::new(0), "tm-sess", None, &spec, dir.path()).expect("deliver");

    // The pointer the checker reads names THIS launch, not the stale one.
    let current = LaunchSpec::read_launch_pointer_in(dir.path(), &spec.session_id)
        .expect("delivery publishes the launch this checker must wait on");
    assert_eq!(current, spec.launch_id);
    assert_ne!(
        current, stale_launch_id,
        "a launch must never inherit an earlier launch's id"
    );
    assert!(
        !LaunchSpec::started_marker_in(dir.path(), &current).exists(),
        "the pane has not run this launch yet, so its own sentinel must be absent \
         even though a previous launch's survives"
    );
}

#[test]
fn deliver_announces_a_refused_line_in_the_pane() {
    // Fail-closed (#8233 requirement 7): a send that is refused must leave the
    // operator a loud line AND surface an error the caller marks the record
    // errored on — never a silent pane.
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    let tmux = Recorder::new(1);

    let err = deliver_in(&tmux, "tm-sess", None, &spec, dir.path())
        .expect_err("a refused send must error");
    assert!(
        matches!(err, crate::runtime::RuntimeError::TmuxUnavailable(_)),
        "{err}"
    );

    let lines = tmux.lines();
    assert_eq!(lines.len(), 2, "the abort notice must follow: {lines:?}");
    assert!(lines[1].contains("aborted"), "{:?}", lines[1]);
    assert!(lines[1].contains(&spec.session_id), "{:?}", lines[1]);

    // The unconsumed spec carries credentials; it must not be left behind. The
    // `<id>.launch` pointer holds only a uuid and is swept on its own TTL.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "an abandoned spec must be removed: {leftovers:?}"
    );
}

#[cfg(unix)]
#[test]
fn deliver_errors_and_cleans_up_when_the_spec_dir_is_unwritable() {
    // The carrier is the launch: if it cannot be written there is nothing to
    // type, and typing the old unbounded line instead is exactly the behaviour
    // #8233 removes.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).expect("mkdir");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o500)).expect("chmod");

    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    let tmux = Recorder::new(0);
    let err = deliver_in(&tmux, "tm-sess", None, &spec, &blocked.join("nested"))
        .expect_err("an unwritable spec dir must error");
    assert!(
        matches!(err, crate::runtime::RuntimeError::Spawn(_)),
        "{err}"
    );

    let lines = tmux.lines();
    assert_eq!(lines.len(), 1, "only the abort notice is typed: {lines:?}");
    assert!(lines[0].contains("aborted"), "{:?}", lines[0]);

    let _ = std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700));
}

/// #8233 item 9c: a pane the handshake refuses gets NO launch and leaves NO
/// spec on disk.
///
/// Why: the refusal arm of `deliver_inner` had no covering test. A launch spec
/// holds `GH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` in cleartext, so a refusal
/// that still wrote one would leave credentials behind for a line the pane was
/// never going to run — which is the whole reason the prompt is confirmed
/// BEFORE anything is written.
/// What: a `Recorder` whose pane stays wedged through every interrupt, so the
/// handshake exhausts its rounds and answers `Continuation`. Asserts the typed
/// error, the refusal wording, and an empty spec directory.
/// Test: this function IS the test.
#[test]
fn deliver_refuses_a_wedged_pane_and_writes_no_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = PathBuf::from("/w");
    let spec = spawn_spec(&bare_launch(&cwd, &[]));
    let tmux = Recorder::wedged_forever();

    let err = deliver_in(&tmux, "tm-sess", None, &spec, dir.path())
        .expect_err("a pane that never executes what it is typed must refuse the launch");

    let msg = err.to_string();
    assert!(
        msg.contains("refusing to launch into pane 'tm-sess'"),
        "the refusal must name the pane and the reason: {msg}"
    );
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert!(
        written.is_empty(),
        "a refused launch must leave no spec and no pointer behind — they carry \
         credentials in cleartext: {written:?}"
    );
    let lines = tmux.lines();
    assert_eq!(lines.len(), 1, "only the abort notice is typed: {lines:?}");
    assert!(lines[0].contains("aborted"), "{:?}", lines[0]);
}
