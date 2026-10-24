//! #8454 S6 prep: the CTO tripwire arm, judged in-process.

use std::cell::RefCell;

use serde_json::json;

use super::*;

/// A canonical temp root: `home`, the listed project `proj`, and `out`.
struct Root {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
    out: PathBuf,
}

fn root() -> Root {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).expect("canonical");
    let (home, proj, out) = (base.join("home"), base.join("proj"), base.join("out"));
    for dir in [&home, &proj, &out] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    Root {
        _tmp: tmp,
        home,
        proj,
        out,
    }
}

fn call(tool: &str, input: Value, cwd: Option<&Path>) -> Value {
    let mut payload = json!({"session_id": "s-1", "tool_name": tool, "tool_input": input});
    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd.display().to_string());
    }
    payload
}

/// The variant name of a touch, for the table.
fn kind(touch: &Touch) -> &'static str {
    match touch {
        Touch::Clear => "clear",
        Touch::Inside(_) => "inside",
        Touch::Unplaceable(_) => "unplaceable",
        Touch::ProjectsUnknown(_) => "unknown",
        Touch::Panicked => "panicked",
    }
}

/// 🔴 #8454 S6: each tool's paths land where the kernel would open them;
/// what the guard cannot place, or cannot judge, trips.
#[test]
fn tripwire_places_each_tool_path() {
    let r = root();
    let p = r.proj.display().to_string();
    let o = r.out.display().to_string();
    let parent = r.proj.parent().expect("parent").display().to_string();
    let (out, proj) = (Some(r.out.as_path()), Some(r.proj.as_path()));
    let cases: Vec<(&str, &str, Value, Option<&Path>, &str)> = vec![
        (
            "read inside",
            "Read",
            json!({"file_path": format!("{p}/a")}),
            out,
            "inside",
        ),
        (
            "read outside",
            "Read",
            json!({"file_path": format!("{o}/a")}),
            out,
            "clear",
        ),
        (
            "read relative",
            "Read",
            json!({"file_path": "a"}),
            proj,
            "inside",
        ),
        (
            "read no cwd",
            "Read",
            json!({"file_path": "a"}),
            None,
            "unplaceable",
        ),
        ("read no path", "Read", json!({}), out, "unplaceable"),
        (
            "read above",
            "Read",
            json!({"file_path": parent.clone()}),
            out,
            "inside",
        ),
        (
            "write dotdot",
            "Write",
            json!({"file_path": format!("{o}/../proj/x")}),
            out,
            "inside",
        ),
        (
            "notebook",
            "NotebookEdit",
            json!({"notebook_path": format!("{p}/n.ipynb")}),
            out,
            "inside",
        ),
        (
            "multiedit case",
            "MultiEdit",
            json!({"file_path": format!("{}/A", p.to_uppercase())}),
            out,
            "inside",
        ),
        (
            "glob root",
            "Glob",
            json!({"pattern": "*", "path": p}),
            out,
            "inside",
        ),
        (
            "glob cwd root",
            "Glob",
            json!({"pattern": "*"}),
            proj,
            "inside",
        ),
        (
            "glob pattern",
            "Glob",
            json!({"pattern": format!("{p}/**/*.rs")}),
            out,
            "inside",
        ),
        (
            "glob outside",
            "Glob",
            json!({"pattern": "**/*.rs", "path": o}),
            out,
            "clear",
        ),
        (
            "glob no root",
            "Glob",
            json!({"pattern": "*"}),
            None,
            "unplaceable",
        ),
        (
            "grep above",
            "Grep",
            json!({"pattern": "x", "path": parent}),
            out,
            "inside",
        ),
        (
            "grep regex",
            "Grep",
            json!({"pattern": "a/b", "path": o}),
            out,
            "clear",
        ),
        (
            "grep path pattern",
            "Grep",
            json!({"pattern": format!("{p}/a")}),
            out,
            "inside",
        ),
        (
            "bash outside",
            "Bash",
            json!({"command": "ls -la a/b"}),
            out,
            "clear",
        ),
        ("bash cwd", "Bash", json!({"command": "ls"}), proj, "inside"),
        (
            "bash read",
            "Bash",
            json!({"command": format!("wc -l {p}/a")}),
            out,
            "inside",
        ),
        (
            "bash redirect in",
            "Bash",
            json!({"command": format!("wc -l <{p}/a")}),
            out,
            "inside",
        ),
        (
            "bash option value",
            "Bash",
            json!({"command": format!("tool --file={p}/a")}),
            out,
            "inside",
        ),
        (
            "bash glob parent",
            "Bash",
            json!({"command": "ls ../proj/*.rs"}),
            out,
            "inside",
        ),
        (
            "bash one-liner",
            "Bash",
            json!({"command": format!("python3 -c \"open('{p}/a')\"")}),
            out,
            "inside",
        ),
        (
            "bash heredoc",
            "Bash",
            json!({"command": format!("python3 - <<'EOF'\nopen('{p}/a')\nEOF")}),
            out,
            "inside",
        ),
        (
            "bash write",
            "Bash",
            json!({"command": "echo x > \"$T/a\""}),
            out,
            "unplaceable",
        ),
        (
            "bash cd write",
            "Bash",
            json!({"command": "cd /tmp && echo x > a"}),
            out,
            "unplaceable",
        ),
        (
            "bash unlexable",
            "Bash",
            json!({"command": "cat 'a"}),
            out,
            "unplaceable",
        ),
        // #8454 fix round: each directory a command moves to is judged.
        (
            "bash cd up",
            "Bash",
            json!({"command": "cd .. && cat .env"}),
            out,
            "inside",
        ),
        (
            "bash cd clear",
            "Bash",
            json!({"command": "cd /tmp && cat .env"}),
            out,
            "clear",
        ),
        (
            "bash bare cd",
            "Bash",
            json!({"command": "cd && cat .env"}),
            out,
            "unplaceable",
        ),
        (
            "bash pushd",
            "Bash",
            json!({"command": "pushd +1 && cat .env"}),
            out,
            "unplaceable",
        ),
        (
            "bash cd $HOME",
            "Bash",
            json!({"command": "cd $HOME/../proj && cat .env"}),
            out,
            "inside",
        ),
        (
            "bash expansion",
            "Bash",
            json!({"command": "echo $F"}),
            out,
            "unplaceable",
        ),
        (
            "bash no cwd",
            "Bash",
            json!({"command": "ls"}),
            None,
            "unplaceable",
        ),
    ];
    let listed = vec![r.proj.clone()];
    for (case, tool, input, cwd, want) in cases {
        let payload = call(tool, input, cwd);
        let touch = judge(&payload, Some(&r.home), |_| Ok(listed.clone()));
        assert_eq!(kind(&touch), want, "{case}: {touch:?}");
    }
    // A mention keeps the project dir, never the command text.
    let secret = call(
        "Bash",
        json!({"command": format!("curl -H 'k: s3' {p}/a")}),
        out,
    );
    let mention = judge(&secret, Some(&r.home), |_| Ok(listed.clone()));
    assert_eq!(mention, Touch::Inside(r.proj.clone()));
    let read = call("Read", json!({"file_path": format!("{p}/a")}), out);
    let unknown = judge(&read, Some(&r.home), |_| Err("x".into()));
    assert_eq!(kind(&unknown), "unknown", "projects unreadable");
    assert_eq!(
        kind(&judge(&read, None, |_| Ok(vec![]))),
        "unknown",
        "no home"
    );
    assert_eq!(judge(&read, Some(&r.home), |_| Ok(vec![])), Touch::Clear);
    // The production list: no `config.yaml` lists nothing; a directory or
    // an unparseable file where it belongs is an error, which trips.
    assert_eq!(host_project_dirs(&r.home), Ok(Vec::new()));
    let host = trusty_common::crate_config::crate_config_path_at(&r.home, "trusty-mpm");
    std::fs::create_dir_all(&host).expect("a dir at config.yaml");
    assert!(host_project_dirs(&r.home).is_err(), "a directory");
    std::fs::remove_dir(&host).expect("rmdir");
    std::fs::write(&host, "channels: [").expect("write");
    assert!(host_project_dirs(&r.home).is_err(), "unparseable");
    let text = format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: \
         {{ bot_ref: slack }}\n    projects: [\"{p}\"]\n"
    );
    std::fs::write(&host, text).expect("write");
    assert_eq!(host_project_dirs(&r.home), Ok(vec![r.proj.clone()]));
}

/// 🔴 #8454 S6 (fail closed): a panic while judging trips and is recorded; a
/// record that fails or panics leaves the deny; a clear call keeps the taint
/// deny unchanged.
#[test]
fn tripwire_arm_panic_denies_and_records() {
    let r = root();
    let read = call("Read", json!({"file_path": "/x"}), Some(&r.out));
    let lines = RefCell::new(Vec::new());
    let panics = |_: &Path| -> Result<Vec<PathBuf>, String> { panic!("injected") };
    let verdict = tripwire_verdict_with(&read, Some(&r.home), "taint".into(), panics, |_, l| {
        lines.borrow_mut().push(l.clone());
        Ok(())
    });
    let Verdict::Deny { check, reason } = verdict else {
        panic!("a panicking arm allowed");
    };
    assert_eq!(check, SENSITIVE_CHECK);
    assert!(
        reason.contains("#8454") && reason.contains("taint"),
        "{reason}"
    );
    let lines = lines.into_inner();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].cause, "panicked");
    assert_eq!(lines[0].session_id.as_deref(), Some("s-1"));

    let inside = call(
        "Read",
        json!({"file_path": format!("{}/a", r.proj.display())}),
        None,
    );
    let listed = || Ok(vec![r.proj.clone()]);
    for (case, record) in [
        (
            "record error",
            Box::new(|_: &Path, _: &TripLine| Err(std::io::Error::other("full")))
                as Box<dyn FnOnce(&Path, &TripLine) -> std::io::Result<()>>,
        ),
        (
            "record panic",
            Box::new(|_: &Path, _: &TripLine| panic!("injected")),
        ),
    ] {
        let verdict =
            tripwire_verdict_with(&inside, Some(&r.home), "t".into(), |_| listed(), record);
        assert!(
            matches!(verdict, Verdict::Deny { check, .. } if check == SENSITIVE_CHECK),
            "{case}: {verdict:?}"
        );
    }
    let outside = call(
        "Read",
        json!({"file_path": format!("{}/a", r.out.display())}),
        None,
    );
    let verdict = tripwire_verdict_with(
        &outside,
        Some(&r.home),
        "t".into(),
        |_| listed(),
        |_, _| panic!("a clear call must not record"),
    );
    assert_eq!(
        verdict,
        Verdict::Deny {
            check: TAINT_CHECK,
            reason: "t".into()
        }
    );
}

/// 🔴 #8454 S6 (plan §4 audit items 1, 2, 10): the arm reads no bypass
/// variable, no environment at all, no `agent_id`, and never calls
/// `check_egress`. Checked on the code lines of both arm files.
#[test]
fn tripwire_arm_reads_no_bypass_and_never_calls_check_egress() {
    let banned = [
        "check_egress",
        "std::env",
        "env::var",
        "TRUSTY_MPM_",
        "CLAUDE_MPM_SUB_AGENT",
        "agent_id",
    ];
    for (file, text) in [
        (
            "egress_guard_tripwire.rs",
            include_str!("egress_guard_tripwire.rs"),
        ),
        (
            "egress_guard_tripwire_paths.rs",
            include_str!("egress_guard_tripwire_paths.rs"),
        ),
    ] {
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            for token in banned {
                assert!(!line.contains(token), "{file}: `{token}` in `{line}`");
            }
        }
    }
}
