//! Tests for `pm_guard_secret_env_files` (#8523). Every fixture value is an
//! obviously fake placeholder.

use super::*;

/// A plist whose environment carries one credential-keyed placeholder.
const WITH_CREDENTIAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Label</key><string>com.example.fake</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/bin</string>
    <key>FAKE_API_KEY</key><string>placeholder-not-a-secret</string>
  </dict>
</dict>
</plist>
"#;

/// A plist whose environment holds only ordinary keys.
const WITHOUT_CREDENTIAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/bin</string>
    <key>HOME</key><string>/Users/example</string>
  </dict>
</dict>
</plist>
"#;

fn bash(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_env_plist_read("Bash", Some(&input), cwd)
}

fn tool(name: &str, path: &Path, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "file_path": path.display().to_string() });
    evaluate_env_plist_read(name, Some(&input), cwd)
}

/// A temp dir holding `Library/LaunchAgents/<name>` with `body`.
fn fixture(name: &str, body: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let agents = dir.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    let path = agents.join(name);
    std::fs::write(&path, body).expect("write");
    (dir, path)
}

#[test]
fn names_a_pm2_dump_and_a_glob_over_its_home() {
    for path in [
        "~/.pm2/dump.pm2",
        "/Users/example/.pm2/dump.pm2.bak",
        "DUMP.PM2",
        "~/.pm2/*",
        "~/.pm2/dump.*",
    ] {
        assert!(names_a_process_manager_dump(path), "{path}");
    }
    for path in [
        "~/.pm2/logs/app-out.log",
        "~/.pm2/pm2.log",
        "src/dump.rs",
        "*",
    ] {
        assert!(!names_a_process_manager_dump(path), "{path}");
    }
}

/// 🔴 REGRESSION (#8523): Bash, `Read`, `Edit` and `Write` of the plist.
#[test]
fn refuses_a_plist_whose_environment_carries_a_credential() {
    let (dir, path) = fixture("com.example.fake.plist", WITH_CREDENTIAL.as_bytes());
    let cwd = dir.path();
    for command in [
        format!("cat {}", path.display()),
        format!("plutil -p '{}'", path.display()),
        format!(
            "python3 -c 'import plistlib; print(plistlib.load(open(\"{}\", \"rb\")))'",
            path.display()
        ),
        "cat Library/LaunchAgents/com.example.fake.plist".to_string(),
        format!(
            "defaults read {}",
            path.display().to_string().trim_end_matches(".plist")
        ),
    ] {
        let reason = bash(&command, cwd).unwrap_or_else(|| panic!("`{command}` must deny"));
        assert!(
            reason.contains("#8523") && reason.contains("FAKE_API_KEY"),
            "{reason}"
        );
        assert!(
            !reason.contains("placeholder-not-a-secret"),
            "never the value: {reason}"
        );
    }
    for name in ["Read", "Edit", "MultiEdit", "Write"] {
        assert!(tool(name, &path, cwd).is_some(), "{name} must deny");
    }
    let grep = serde_json::json!({ "pattern": ".", "path": path.display().to_string() });
    assert!(evaluate_env_plist_read("Grep", Some(&grep), cwd).is_some());
}

#[test]
fn allows_a_plist_with_no_credential_and_a_safe_verb() {
    let (dir, clean) = fixture("com.example.clean.plist", WITHOUT_CREDENTIAL.as_bytes());
    let cwd = dir.path();
    assert_eq!(bash(&format!("cat {}", clean.display()), cwd), None);
    assert_eq!(tool("Read", &clean, cwd), None);
    let secret = clean.with_file_name("com.example.fake.plist");
    std::fs::write(&secret, WITH_CREDENTIAL).expect("write");
    for command in [
        format!("ls -la {}", secret.display()),
        format!("rm {}", secret.display()),
        format!("ls {}", clean.parent().expect("dir").display()),
        "cat /nonexistent/Info.plist".to_string(),
        "echo no plist here".to_string(),
    ] {
        assert_eq!(bash(&command, cwd), None, "`{command}` must allow");
    }
    // A non-plist file is never read for content.
    assert_eq!(tool("Read", &cwd.join("notes.md"), cwd), None);
}

/// Error arms: every plist the guard cannot read, parse or locate — when it
/// could hold an `EnvironmentVariables` dict — fails CLOSED.
#[test]
fn fails_closed_on_a_plist_it_cannot_judge() {
    // A binary plist carrying the key is not parsed here.
    let mut binary = b"bplist00".to_vec();
    binary.extend_from_slice(b"EnvironmentVariables\x00FAKE");
    let (dir, path) = fixture("com.example.bin.plist", &binary);
    assert!(bash(&format!("cat {}", path.display()), dir.path()).is_some());
    // XML that names the key but does not parse.
    let broken = "<plist><dict><key>EnvironmentVariables</key><dict><key>X";
    let (dir, path) = fixture("com.example.broken.plist", broken.as_bytes());
    assert!(tool("Read", &path, dir.path()).is_some());
    // Not UTF-8.
    let (dir, path) = fixture("com.example.latin.plist", b"EnvironmentVariables\xff\xfe");
    assert!(tool("Read", &path, dir.path()).is_some());
    // A glob over a launchd directory, a relative plist the hook cannot find,
    // and a relative plist behind a `cd`.
    let (dir, _path) = fixture("com.example.clean.plist", WITHOUT_CREDENTIAL.as_bytes());
    for command in [
        "cat ~/Library/LaunchAgents/*.plist",
        "cat missing/com.example.plist",
        "cd /tmp && cat Library/LaunchAgents/com.example.clean.plist",
    ] {
        assert!(bash(command, dir.path()).is_some(), "`{command}` must deny");
    }
    // A plist larger than the bound.
    let big = format!(
        "{WITHOUT_CREDENTIAL}{}",
        " ".repeat(MAX_PLIST_BYTES as usize)
    );
    let (dir, path) = fixture("com.example.big.plist", big.as_bytes());
    assert!(tool("Read", &path, dir.path()).is_some());
}

/// 🔴 REGRESSION (#8523 critic CRITICAL 1): a content `Grep` over a launchd
/// directory prints every plist's `EnvironmentVariables` values. ALLOWED on
/// `8dfcf2e1e`. A listing of the same directory stays allowed.
#[test]
fn refuses_a_content_grep_over_a_launchd_directory() {
    let (dir, path) = fixture("com.example.fake.plist", WITH_CREDENTIAL.as_bytes());
    let agents = path.parent().expect("dir").to_path_buf();
    for target in [agents.clone(), agents.join("sub")] {
        std::fs::create_dir_all(&target).expect("mkdir");
        let grep = serde_json::json!({
            "pattern": ".",
            "path": target.display().to_string(),
            "output_mode": "content",
        });
        assert!(
            evaluate_env_plist_read("Grep", Some(&grep), dir.path()).is_some(),
            "Grep over {} must deny",
            target.display()
        );
    }
    assert_eq!(
        bash(&format!("ls -la {}", agents.display()), dir.path()),
        None
    );
    assert_eq!(bash("ls -la ~/Library/LaunchAgents", dir.path()), None);
}

/// 🔴 REGRESSION (#8523 round-3 critic CRITICAL): a Bash content search or
/// recursive read over a launchd directory, or one below it, prints every
/// plist's `EnvironmentVariables` values. ALLOWED on `bec394551`, where only a
/// `Grep` call judged a directory. A listing stays allowed.
#[test]
fn refuses_a_bash_content_search_over_a_launchd_directory() {
    let (dir, path) = fixture("com.example.fake.plist", WITH_CREDENTIAL.as_bytes());
    let agents = path.parent().expect("dir").to_path_buf();
    let sub = agents.join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir");
    for target in [&agents, &sub] {
        let t = target.display();
        for command in [
            format!("grep -r . {t}"),
            format!("rg . {t}"),
            format!("rg -uu KEY '{t}'"),
            format!("find {t} -type f -exec cat {{}} +"),
            format!("tar -cf - {t} | tar -xOf -"),
            format!("cd {t} && grep -r . ."),
        ] {
            assert!(
                bash(&command, dir.path()).is_some(),
                "`{command}` must deny"
            );
        }
    }
    for command in [
        format!("ls -la {}", agents.display()),
        format!("stat {}", sub.display()),
        "ls -la ~/Library/LaunchAgents".to_string(),
    ] {
        assert_eq!(bash(&command, dir.path()), None, "`{command}` must allow");
    }
}

/// 🔴 REGRESSION (#8523 round 4 MEDIUM): `mkdir -p ~/Library/LaunchAgents
/// ~/.trusty-mpm/logs` — the documented supervisor install step
/// (`crates/trusty-mpm/deploy/supervisor/README.md`) — was refused whenever
/// the directory already existed, because round 3 made Bash a
/// directory-searching caller for every verb. `touch`ing a new plist and
/// `cp`/`mv`ing a clean plist into the directory must allow too; judging the
/// destination DIRECTORY is what changes, not the source FILE. DENIED on
/// `1ff961175`.
#[test]
fn mkdir_touch_and_cp_into_a_launchd_directory_are_allowed() {
    let (dir, path) = fixture("com.example.fake.plist", WITH_CREDENTIAL.as_bytes());
    let agents = path.parent().expect("dir").to_path_buf();
    let cwd = dir.path();
    let template = cwd.join("template.plist");
    std::fs::write(&template, WITHOUT_CREDENTIAL).expect("write");
    for command in [
        format!(
            "mkdir -p {} {}",
            agents.display(),
            cwd.join(".trusty-mpm/logs").display()
        ),
        format!("touch {}/new.plist", agents.display()),
        format!("cp {} {}/", template.display(), agents.display()),
        format!(
            "mv {} {}/renamed.plist",
            template.display(),
            agents.display()
        ),
    ] {
        assert_eq!(bash(&command, cwd), None, "`{command}` must allow");
    }
}

/// 🔴 REGRESSION (#8523 round 4): a `cp`/`mv` into a launchd directory must
/// still judge its SOURCE by content — the write-verb allowance in
/// [`mkdir_touch_and_cp_into_a_launchd_directory_are_allowed`] covers the
/// destination directory only.
#[test]
fn cp_or_mv_of_a_credential_plist_into_a_launchd_directory_is_still_denied() {
    let (dir, secret) = fixture("com.example.other.plist", WITH_CREDENTIAL.as_bytes());
    let cwd = dir.path();
    let agents = secret.parent().expect("dir").to_path_buf();
    // A source outside the launchd directory that still carries a credential.
    let outside = cwd.join("template.plist");
    std::fs::write(&outside, WITH_CREDENTIAL).expect("write");
    for command in [
        format!("cp {} {}/", outside.display(), agents.display()),
        format!("mv {} {}/", outside.display(), agents.display()),
    ] {
        let reason = bash(&command, cwd).unwrap_or_else(|| panic!("`{command}` must deny"));
        assert!(
            reason.contains("#8523") && reason.contains("FAKE_API_KEY"),
            "{reason}"
        );
    }
    // A content search over the directory stays denied regardless of #8523
    // round 4's write-verb allowance.
    for command in [
        format!("grep -r . {}", agents.display()),
        format!("cat {}", secret.display()),
    ] {
        assert!(bash(&command, cwd).is_some(), "`{command}` must deny");
    }
}

/// 🔴 REGRESSION (#8523 critic HIGH 2): a FIFO or device reports length 0, so
/// the size check passed and `fs::read` blocked the hook forever. On
/// `8dfcf2e1e` this test times out. It must deny, and promptly.
#[cfg(unix)]
#[test]
fn refuses_a_non_regular_plist_without_blocking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("x.plist");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");
    let (tx, rx) = std::sync::mpsc::channel();
    let (cwd, target) = (dir.path().to_path_buf(), fifo.clone());
    std::thread::spawn(move || {
        let _ = tx.send(tool("Read", &target, &cwd));
    });
    let verdict = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the guard must answer a FIFO without blocking");
    assert!(verdict.is_some(), "a FIFO plist must deny");
}
