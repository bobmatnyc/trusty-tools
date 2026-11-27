//! Unit tests for the #11463 trust anchors: `~/.ssh/authorized_keys` and
//! `~/.trusty-tools/trusty-common/config.yaml`, which `trusty-secrets
//! remote-gate` trusts to tell EVO from a same-user agent.

use super::tests::{Fixture, fixture, payload, pm_env};
use super::*;
use serde_json::json;

/// The two #11463 anchors, relative to the home directory.
const ANCHORS: [&str; 2] = [
    ".ssh/authorized_keys",
    ".trusty-tools/trusty-common/config.yaml",
];

/// [`fixture`] with both #11463 anchors and an ssh sibling written.
fn remote_fixture() -> Fixture {
    let fx = fixture();
    for rel in ANCHORS {
        let path = fx.home.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "x\n").expect("write anchor");
    }
    std::fs::write(fx.home.join(".ssh/known_hosts"), "h\n").expect("write known_hosts");
    fx
}

/// A subagent of a PM session.
fn agent_env(fx: &Fixture) -> HookEnv {
    HookEnv {
        sub_agent: true,
        ..pm_env(fx)
    }
}

fn verdict(fx: &Fixture, env: &HookEnv, tool: &str, input: Value) -> Option<String> {
    evaluate(&payload(fx, tool, input), env, MpmConfig::default)
}

fn bash(fx: &Fixture, env: &HookEnv, command: &str) -> Option<String> {
    verdict(fx, env, "Bash", json!({ "command": command }))
}

/// Every Bash write vector, with `P` standing for the target spelling.
const BASH_VECTORS: [&str; 15] = [
    "echo k > P",
    "echo k >> P",
    "cat a.txt | tee P",
    "cat a.txt | tee -a P",
    "cp a.txt P",
    "mv a.txt P",
    "sed -i s/x/y/ P",
    "ln -sf a.txt P",
    "ln P hard.txt",
    "mv P moved.txt",
    "rm -f P",
    "truncate -s 0 P",
    "chmod 600 P",
    "chown me P",
    "printf k 1>>P",
];

/// The `~`, `$HOME`, `${HOME}` and absolute spellings of one anchor, plus
/// upper-case and mixed-case variants, which APFS opens as the same file.
fn spellings(fx: &Fixture, rel: &str) -> [String; 7] {
    let upper = rel.to_uppercase();
    // `.Trusty-Tools/trusty-common/Config.yaml`, `.Ssh/Authorized_keys`.
    let mixed: String = rel
        .split('/')
        .enumerate()
        .map(|(i, part)| {
            let mut chars = part.chars();
            let head: String = chars.by_ref().take(2).collect();
            let head = if i == 1 { head } else { head.to_uppercase() };
            format!("{head}{}", chars.as_str())
        })
        .collect::<Vec<_>>()
        .join("/");
    [
        format!("~/{rel}"),
        format!("$HOME/{rel}"),
        format!("\"${{HOME}}/{rel}\""),
        fx.home.join(rel).display().to_string(),
        format!("~/{upper}"),
        format!("$HOME/{mixed}"),
        fx.home.join(&upper).display().to_string(),
    ]
}

/// 🔴 #11463: every edit tool and every Bash write vector onto either anchor,
/// in every spelling, is denied for the PM and for a subagent.
#[test]
fn remote_gate_anchor_writes_are_denied_for_pm_and_agent() {
    let fx = remote_fixture();
    for env in [pm_env(&fx), agent_env(&fx)] {
        for rel in ANCHORS {
            let upper = rel.to_uppercase();
            for spelled in [
                format!("~/{rel}"),
                fx.home.join(rel).display().to_string(),
                format!("~/{upper}"),
                fx.home.join(&upper).display().to_string(),
            ] {
                for tool in EDIT_TOOLS {
                    let input = json!({ "file_path": spelled, "notebook_path": spelled,
                        "content": "k", "old_string": "x", "new_string": "k" });
                    let reason = verdict(&fx, &env, tool, input)
                        .unwrap_or_else(|| panic!("{tool} {spelled} must be denied"));
                    assert!(reason.contains("#11463"), "{tool}: {reason}");
                }
            }
            for spelled in spellings(&fx, rel) {
                for vector in BASH_VECTORS {
                    let command = vector.replace('P', &spelled);
                    let reason = bash(&fx, &env, &command)
                        .unwrap_or_else(|| panic!("`{command}` must be denied"));
                    assert!(reason.contains("trust anchor"), "`{command}`: {reason}");
                }
            }
        }
    }
}

/// 🔴 #11463: moving, deleting or globbing the directory holding an anchor is
/// a write to it.
#[test]
fn remote_gate_anchor_directory_writes_are_denied() {
    let fx = remote_fixture();
    for command in [
        "rm -rf ~/.ssh",
        "mv ~/.ssh /tmp/ssh-old",
        "rm ~/.ssh/*",
        "chmod 777 ~/.ssh/*",
        "rm -rf $HOME/.trusty-tools/trusty-common", // path-literal: allow (guard input)
        "mv ~/.trusty-tools /tmp/tt-old",           // path-literal: allow (guard input)
    ] {
        assert!(
            bash(&fx, &pm_env(&fx), command).is_some(),
            "`{command}` must be denied"
        );
    }
}

/// 🔴 #11463 FAIL-CLOSED: a write the guard cannot place, or an anchor that
/// does not resolve, is denied.
#[test]
fn remote_gate_unplaceable_or_unresolvable_writes_are_denied() {
    let fx = remote_fixture();
    for command in [
        "cd ~/.ssh && echo k >> authorized_keys",
        "echo k >> \"$D/authorized_keys\"",
        "cd ~/.trusty-tools/trusty-common && cp a.txt config.yaml", // path-literal: allow (guard input)
    ] {
        assert!(
            bash(&fx, &pm_env(&fx), command).is_some(),
            "`{command}` must be denied"
        );
    }
    let keys = fx.home.join(ANCHORS[0]);
    std::fs::remove_file(&keys).expect("rm keys");
    std::os::unix::fs::symlink(&keys, &keys).expect("self loop");
    let reason = bash(&fx, &pm_env(&fx), "echo k > ok.txt").expect("loop denies");
    assert!(reason.contains("does not resolve"), "{reason}");
}

/// #11463: reads of the anchors, and writes to their siblings, stay allowed.
#[test]
fn remote_gate_reads_and_siblings_stay_allowed() {
    let fx = remote_fixture();
    let env = pm_env(&fx);
    for command in [
        "cat ~/.ssh/authorized_keys",
        "grep ssh-ed25519 $HOME/.ssh/authorized_keys",
        "cp ~/.ssh/authorized_keys copy.txt",
        "head ~/.trusty-tools/trusty-common/config.yaml", // path-literal: allow (guard input)
        "chmod +x scripts/*.sh",
    ] {
        assert_eq!(bash(&fx, &env, command), None, "`{command}`");
    }
    for rel in ANCHORS {
        let path = fx.home.join(rel).display().to_string();
        assert_eq!(
            verdict(&fx, &env, "Read", json!({ "file_path": path })),
            None
        );
    }
    for sibling in [
        ".ssh/known_hosts",
        ".ssh/config",
        ".trusty-tools/trusty-search/config.yaml",
        ".trusty-tools/trusty-common/other.yaml",
    ] {
        for spelled in spellings(&fx, sibling) {
            // `ln -sf SRC EXISTING` reads a dest symlink as a directory and
            // fails closed for every existing file, anchor or not (#8878).
            for vector in BASH_VECTORS.iter().filter(|v| !v.starts_with("ln -sf")) {
                let command = vector.replace('P', &spelled);
                assert_eq!(bash(&fx, &env, &command), None, "`{command}`");
            }
        }
        let input = json!({ "file_path": fx.home.join(sibling).display().to_string(),
            "content": "k" });
        assert_eq!(verdict(&fx, &env, "Write", input), None, "{sibling}");
    }
}
