//! Unit tests for the #8625 kuzu-store rule (`pm_guard_kuzu_store.rs`).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

/// A scratch tree: a home and a project holding a store.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let project = root.join("p");
        std::fs::create_dir_all(project.join(".kuzu-memory")).expect("mkdir store");
        std::fs::write(project.join(".kuzu-memory/memories.db"), "kuzu").expect("store");
        Self {
            _dir: dir,
            root,
            home,
            project,
        }
    }

    /// A path inside the import venv's `bin/`, which need not exist.
    fn venv(&self, name: &str) -> String {
        let bin = self.home.join(".trusty-tools/kuzu-import-venv/bin");
        bin.join(name).display().to_string()
    }

    fn store(&self) -> String {
        self.project
            .join(".kuzu-memory/memories.db")
            .display()
            .to_string()
    }
}

/// The verdict on a Bash `command` run from the project.
fn bash(fx: &Fx, command: &str) -> Option<String> {
    bash_in(&fx.project, command)
}

/// The verdict on a Bash `command` run from `cwd`.
fn bash_in(cwd: &Path, command: &str) -> Option<String> {
    let input = json!({ "command": command });
    evaluate_kuzu_store_access("Bash", Some(&input), cwd)
}

/// The verdict on a `tool` call with `input`.
fn tool(fx: &Fx, tool: &str, input: Value) -> Option<String> {
    evaluate_kuzu_store_access(tool, Some(&input), &fx.project)
}

/// Asserts every command denies with `slug`, listing every one that does not.
fn assert_denied(fx: &Fx, commands: &[String], slug: &str) {
    let missed: Vec<String> = commands
        .iter()
        .filter_map(|command| {
            let deny = bash(fx, command);
            let ok = deny.as_deref().is_some_and(|r| r.contains(slug));
            (!ok).then(|| format!("{command:?} -> {deny:?}"))
        })
        .collect();
    assert!(
        missed.is_empty(),
        "expected a `{slug}` deny for each of:\n{}",
        missed.join("\n")
    );
}

fn assert_allowed(fx: &Fx, commands: &[String]) {
    for command in commands {
        assert_eq!(bash(fx, command), None, "expected an allow for {command:?}");
    }
}

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|c| (*c).to_string()).collect()
}

#[test]
fn denies_every_tool_shape_naming_a_store() {
    let fx = Fx::new();
    let store = fx.store();
    let venv = fx.venv("");
    for name in ["Read", "Edit", "Write", "MultiEdit"] {
        let deny = tool(&fx, name, json!({ "file_path": store }));
        assert!(
            deny.is_some_and(|r| r.contains("kuzu-store-read")),
            "{name}"
        );
    }
    let tmp = "/tmp/trusty-kuzu-import-ab12CD/export.json";
    for (name, input) in [
        ("Read", json!({ "file_path": tmp })),
        ("Read", json!({ "file_path": format!("{venv}python") })),
        (
            "Grep",
            json!({ "pattern": "x", "path": fx.project.join(".kuzu-memory") }),
        ),
        ("Grep", json!({ "pattern": "x", "glob": "memories.db" })),
        ("Glob", json!({ "pattern": "**/memories.db" })),
        ("Glob", json!({ "pattern": "*", "path": venv })),
    ] {
        let deny = tool(&fx, name, input.clone());
        assert!(
            deny.is_some_and(|r| r.contains("kuzu-store-read")),
            "{name} {input}"
        );
    }
}

#[test]
fn denies_bash_reads_of_a_store() {
    let fx = Fx::new();
    let store = fx.store();
    let venv = fx.venv("");
    assert_denied(
        &fx,
        &[
            format!("cat {store}"),
            "sqlite3 .kuzu-memory/memories.db .dump".into(),
            "strings memories.db".into(),
            "xxd memories.db.wal".into(),
            "dd if=.kuzu-memory/memories.db of=/tmp/x".into(),
            "tar cf - .kuzu-memory".into(),
            "cp -r .kuzu-memory /tmp/x".into(),
            "mv .kuzu-memory /tmp/x".into(),
            "sed -i s/a/b/ .kuzu-memory/memories.db".into(),
            "python3 -c \"import kuzu; kuzu.Database('x/memories.db')\"".into(),
            "echo \"$(cat .kuzu-memory/memories.db)\"".into(),
            format!("python3 - <<'EOF'\nimport kuzu\nkuzu.Database(\"{store}\")\nEOF"),
            "sh -c 'cat memories.db'".into(),
            "cd .kuzu-memory && cat memories.db".into(),
            "cd .kuzu-memory".into(),
            "rm -rf .kuzu-memory".into(),
            "cat '.kuzu''-memory/memories.db'".into(),
            "cat \"${F:-memories.db}\"".into(),
            "cat x/{memories,other}.db".into(),
            "cat /tmp/trusty-kuzu-import-ab12CD/export.json".into(),
            "ls /tmp/trusty-kuzu-import-ab12CD && cat /tmp/trusty-kuzu-import-ab12CD/export.json"
                .into(),
            format!("source {venv}activate"),
            format!("{venv}python script.py"),
            "find . -path '*/.kuzu-memory/*' -exec cat {} +".into(),
        ],
        "kuzu-store-read",
    );
}

#[test]
fn denies_running_kuzu_memory() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "kuzu-memory memory export",
            "/usr/local/bin/kuzu-memory recall x",
            "uvx kuzu-memory export",
            "uvx kuzu-memory@latest export",
            "pipx run kuzu-memory export",
            "uv tool run kuzu-memory export",
            "env FOO=1 kuzu-memory export",
            "echo $(kuzu-memory export)",
            "python3 -m kuzu_memory export",
            "python3 -mkuzu_memory export",
            "python3 -c 'import kuzu_memory'",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn denies_an_unlexable_segment_naming_a_store() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&["cat \"memories.db", "cat '.kuzu-memory/x"]),
        "kuzu-store-read",
    );
}

#[test]
fn denies_a_glob_that_can_match_a_deny_name() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "cat *.db",
            "cat mem*",
            "cat x/mem[o]ries.db",
            "cat .kuzu-m*/mem*.db",
            "cat .kuzu-memory/*",
            "cat /tmp/*kuzu-import-ab*/export.json",
            "cat /tmp/trusty-k*/export.json",
            "cat ~/.trusty-tools/kuzu-imp*-venv/bin/python",
            "cat x/.{kuzu-memory,{a,b}}/y",
        ]),
        "kuzu-store-read",
    );
    for (name, input) in [
        ("Grep", json!({ "pattern": "x", "glob": "*.db" })),
        ("Glob", json!({ "pattern": "**/.kuzu-*/*" })),
    ] {
        assert!(tool(&fx, name, input.clone()).is_some(), "{name} {input}");
    }
    // Ruling Q3: a word of wildcards only is not a deny name.
    assert_allowed(
        &fx,
        &s(&[
            "cat *",
            "cat */*.md",
            "cat *.rs",
            "head -n1 *.json",
            // #7266, #7533: pinned allows the temp-directory glob must not take.
            "cat *test*",
            "echo *Q",
        ]),
    );
    for (name, input) in [
        ("Grep", json!({ "pattern": "x", "glob": "*.{rs,toml}" })),
        ("Glob", json!({ "pattern": "**/*.json" })),
    ] {
        assert_eq!(tool(&fx, name, input.clone()), None, "{name} {input}");
    }
}

// #8625 re-scope: every input the removed exception, or a bypass of it, once
// allowed now denies. Under D5b each one holds both words, whatever prefix,
// wrapper or path-qualified program surrounds them. #9794 moved the dry-run
// and review rows to `allows_the_dry_run_export_and_review_forms` and the
// `--python` rows to `denies_any_python_flag_with_its_own_slug`.
#[test]
fn denies_the_formerly_sanctioned_import() {
    let fx = Fx::new();
    let p = fx.project.display();
    let store = fx.store();
    let cargo = fx.home.join(".cargo/bin/trusty-memory");
    let commands: Vec<String> = vec![
        format!("trusty-memory import kuzu --from {p}/.kuzu-memory --dry-run --export-refusals"),
        format!(
            "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --from {p}/.kuzu-memory/memories.db --palace x"
        ),
        format!(
            "trusty-memory import kuzu --from {p}/.kuzu-memory --review-ledger ~/.trusty-tools/kuzu-review/R/ledger.jsonl"
        ),
        "trusty-memory import kuzu --dry-run 2>&1 | tee /tmp/report.txt".into(),
        format!("trusty-memory(){{ strings \"$4\"; }}; trusty-memory import kuzu --from {store}"),
        format!("PYTHONPATH=/tmp/x trusty-memory import kuzu --from {store} --dry-run"),
        format!("env PYTHONPATH=/tmp/x trusty-memory import kuzu --from {store}"),
        format!("PATH=/tmp/x:$PATH trusty-memory import kuzu --from {store}"),
        format!("/tmp/x/trusty-memory import kuzu --from {store}"),
        format!("./trusty-memory import kuzu --from {store}"),
        format!("{} import kuzu --from {store} --dry-run", cargo.display()),
        "trusty-memory import kuzu --discover --python \"~/.trusty-tools/kuzu-import-venv/bin/python\""
            .into(),
    ];
    assert_denied(&fx, &commands, "kuzu-store-read");
    // The reviewer's masked file stays readable.
    let masked = fx.home.join(".trusty-tools/kuzu-review/R/masked.jsonl");
    assert_eq!(tool(&fx, "Read", json!({ "file_path": masked })), None);
}

#[test]
fn denies_the_deprecated_migrate_aliases() {
    let fx = Fx::new();
    let p = fx.project.display();
    assert_denied(
        &fx,
        &[
            format!("trusty-memory migrate kuzu-data --from {p}/.kuzu-memory --palace x"),
            "trusty-memory migrate kuzu-data --from . --palace x".into(),
            "trusty-memory migrate kuzu-memory --dry-run".into(),
            "trusty-memory -v migrate kuzu-data --from .".into(),
        ],
        "kuzu-store-read",
    );
}

#[test]
fn denies_a_store_read_beside_an_import_call() {
    let fx = Fx::new();
    let venv_lib = fx
        .home
        .join(".trusty-tools/kuzu-import-venv/lib")
        .display()
        .to_string();
    assert_denied(
        &fx,
        &[
            "trusty-memory import kuzu --from x/memories.db $(cat x/memories.db)".into(),
            "trusty-memory import kuzu --from x; cat x/memories.db".into(),
            "trusty-memory import kuzu --from x | cat x/memories.db".into(),
            "trusty-memory recall foo --from x/memories.db".into(),
            "trusty-memory import kuzu --from x < x/memories.db".into(),
            format!("PYTHONPATH={venv_lib} trusty-memory import kuzu --discover"),
            "xargs trusty-memory import kuzu --from x/memories.db".into(),
        ],
        "kuzu-store-read",
    );
}

#[test]
fn keeps_ordinary_commands_allowed() {
    let fx = Fx::new();
    assert_allowed(
        &fx,
        &[
            "grep -rn memories.db docs/".into(),
            "rg kuzu-memory crates/".into(),
            "git commit -m \"fix memories.db import\"".into(),
            "echo kuzu-memory is retired".into(),
            "ls .kuzu-memory".into(),
            format!("stat {}", fx.store()),
            "cargo test -p trusty-memory".into(),
            "rg kuzu crates/trusty-search".into(),
            "gh issue comment 1 --body \"the .kuzu-memory store\"".into(),
            "sed -n '/x.*y/p' notes.md".into(),
        ],
    );
    for path in [
        "crates/trusty-memory/src/commands/kuzu_import/tests.rs",
        "crates/trusty-memory/src/commands/kuzu_import/export.py",
        "content/skills/tm-memory-import.md",
    ] {
        let target = fx.project.join(path);
        assert_eq!(
            tool(&fx, "Read", json!({ "file_path": target })),
            None,
            "{path}"
        );
    }
}

#[test]
fn denies_a_read_through_a_symlink_into_a_store() {
    let fx = Fx::new();
    let link = fx.root.join("innocent.txt");
    symlink(fx.store(), &link).expect("link");
    let deny = tool(&fx, "Read", json!({ "file_path": link }));
    assert!(deny.is_some_and(|r| r.contains("kuzu-store-read")));
}

#[test]
fn denies_when_a_tool_path_cannot_be_resolved() {
    // Fail-Open Check: a symlink loop makes resolution error; the call denies.
    let fx = Fx::new();
    symlink(fx.root.join("loop-b"), fx.root.join("loop-a")).expect("loop-a");
    symlink(fx.root.join("loop-a"), fx.root.join("loop-b")).expect("loop-b");
    for target in [fx.root.join("loop-a"), fx.root.join("loop-a/notes.md")] {
        let deny = tool(&fx, "Read", json!({ "file_path": target }));
        assert!(
            deny.is_some_and(|r| r.contains("kuzu-store-read")),
            "{target:?}"
        );
    }
}

#[test]
fn denies_a_bash_call_from_inside_a_store_directory() {
    let fx = Fx::new();
    let store_dir = fx.project.join(".kuzu-memory");
    for command in ["cat *", "head -c 100 *"] {
        let deny = bash_in(&store_dir, command);
        assert!(
            deny.is_some_and(|r| r.contains("kuzu-store-read")),
            "{command}"
        );
    }
}

#[test]
fn the_gated_decision_labels_a_kuzu_deny_and_binds_the_architect() {
    use crate::commands::pm_guard_architect_envfile::{
        SECRET_FILE_READ_RULE, evaluate_secret_file_read_gated,
    };
    use crate::commands::pm_guard_floor::ArchitectGate;
    use crate::commands::pm_guard_trust_anchor::tests::{
        allowlist, architect_env, fixture, payload,
    };
    let fx = fixture();
    let store = fx.project.join(".kuzu-memory/memories.db");
    std::fs::create_dir_all(fx.project.join(".kuzu-memory")).expect("mkdir store");
    std::fs::write(fx.project.join(".kuzu-memory/.env.local"), "K=v\n").expect("env file");
    for (call, rule) in [
        (
            payload(
                &fx,
                "Bash",
                json!({ "command": "cat .kuzu-memory/memories.db" }),
            ),
            KUZU_STORE_RULE,
        ),
        (
            payload(&fx, "Read", json!({ "file_path": store })),
            KUZU_STORE_RULE,
        ),
        (
            payload(&fx, "Bash", json!({ "command": "cat .env.local" })),
            SECRET_FILE_READ_RULE,
        ),
        // Both rules deny `./.*`; the #7266 deny keeps its label.
        (
            payload(&fx, "Bash", json!({ "command": "cat ./.*" })),
            SECRET_FILE_READ_RULE,
        ),
        // A `tm env` shape the Architect may run lifts the #7266 deny only.
        (
            payload(
                &fx,
                "Bash",
                json!({ "command": "tm env keys .kuzu-memory/.env.local" }),
            ),
            KUZU_STORE_RULE,
        ),
        // #8625 D5b: the text rule binds the Architect too.
        (
            payload(
                &fx,
                "Bash",
                json!({ "command": "$'trusty-memory' import kuzu>/tmp/o" }),
            ),
            KUZU_STORE_RULE,
        ),
        // #9794: a `--python` deny audits under its own slug.
        (
            payload(
                &fx,
                "Bash",
                json!({ "command": "trusty-memory import kuzu --dry-run --python /bin/cat" }),
            ),
            "kuzu-import-python",
        ),
        // #9794: an apply form keeps the D5b deny for the Architect too.
        (
            payload(
                &fx,
                "Bash",
                json!({ "command": "trusty-memory import kuzu --discover" }),
            ),
            KUZU_STORE_RULE,
        ),
    ] {
        // Ruling B10: the Architect's main thread is bound too.
        let gate = ArchitectGate::new(&call, architect_env(&fx), || allowlist(&fx));
        assert!(gate.is_architect(), "the fixture is the Architect");
        let tool = call["tool_name"].as_str().unwrap_or_default();
        let deny =
            evaluate_secret_file_read_gated(tool, call.get("tool_input"), &fx.project, &gate);
        assert_eq!(deny.map(|d| d.rule), Some(rule), "{call}");
    }
    // #9794 Q2: the narrowed allow applies to every caller, the Architect too.
    let call = payload(
        &fx,
        "Bash",
        json!({ "command": "trusty-memory import kuzu --discover --dry-run" }),
    );
    let gate = ArchitectGate::new(&call, architect_env(&fx), || allowlist(&fx));
    let deny = evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate);
    assert_eq!(deny, None, "the Architect runs the dry run");
    // The same `tm env` shape outside a store is exempt, so the case above
    // proves the kuzu deny outlives the exemption.
    std::fs::write(fx.project.join(".env.local"), "K=v\n").expect("env file");
    let call = payload(&fx, "Bash", json!({ "command": "tm env keys .env.local" }));
    let gate = ArchitectGate::new(&call, architect_env(&fx), || allowlist(&fx));
    let deny = evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate);
    assert_eq!(deny, None, "the exemption applies outside a store");
}

// #8625 fix round: `file -f` prints each line of its path, so it is off the safe verbs.

#[test]
fn denies_file_dash_f_on_a_store_path() {
    let fx = Fx::new();
    let store = fx.store();
    assert_denied(
        &fx,
        &[
            format!("file -f {store}"),
            "file -f $TMPDIR/trusty-kuzu-import-ab12/export.json".into(),
            "file -f ~/p/.kuzu-memory/memories.db".into(),
        ],
        "kuzu-store-read",
    );
}

// #8625 re-scope (Architect ruling 2026-10-12): every `trusty-memory import
// kuzu` denies under the guard, whatever surrounds it.

#[test]
fn denies_an_import_kuzu_after_an_environment_prefix() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "PYTHONPATH=/tmp/x trusty-memory import kuzu --discover",
            "PATH=/tmp/x:$PATH trusty-memory import kuzu --root ~/p",
            "export PYTHONPATH=/tmp/x; trusty-memory import kuzu --discover",
            "export PYTHONPATH=/tmp/x && trusty-memory import kuzu --discover",
            "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --discover",
            "nice --bogus trusty-memory import kuzu --discover",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn denies_an_import_kuzu_inside_a_substitution_or_a_shell_string() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "echo $(trusty-memory import kuzu --discover)",
            "echo `trusty-memory import kuzu --discover`",
            "bash -c 'trusty-memory import kuzu --discover'",
            "sh -c \"trusty-memory import kuzu --discover\"",
            "echo kuzu | xargs trusty-memory import",
            "echo import kuzu | xargs trusty-memory",
            "trusty-memory import kuzu --dry-run | tail -50",
            "trusty-memory import kuzu --dry-run 2>&1 | tee /tmp/report.txt",
            "python3 -c \"import subprocess; subprocess.run(['trusty-memory','import','kuzu'])\"",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn denies_an_import_kuzu_in_another_spelling() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "TRUSTY-MEMORY import kuzu --discover",
            "Trusty-Memory import kuzu --discover",
            "/usr/local/bin/TRUSTY-MEMORY import kuzu --discover",
            "~/.cargo/bin/trusty-memory import kuzu --discover",
            "\\trusty-memory import kuzu --discover",
            "trusty-memory import -v kuzu --discover",
            "trusty-memory import --verbose kuzu --discover",
            "trusty-memory -vv import kuzu --discover",
            "trusty-memory import KUZU --discover",
            "trusty-memory import ${X:-kuzu} --discover",
            "TRUSTY-MEMORY migrate kuzu-data",
            "trusty-memory migrate KUZU_MEMORY --dry-run",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn keeps_unrelated_trusty_memory_commands_allowed() {
    let fx = Fx::new();
    assert_allowed(
        &fx,
        &s(&[
            "trusty-memory doctor",
            "trusty-memory import other",
            "trusty-memory import --help",
            "ls ~/p",
            "git commit -m \"docs: trusty-memory import kuzu is denied under the guard\"",
            "gh issue comment 1 --body \"run trusty-memory import kuzu from your shell\"",
        ]),
    );
}

#[test]
fn denies_an_import_kuzu_split_by_a_redirection() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "trusty-memory import 2>/dev/null kuzu --discover",
            "trusty-memory import 2> /dev/null kuzu --discover",
            "trusty-memory import &>/dev/null kuzu --discover",
            "trusty-memory >/tmp/o import kuzu --discover",
            "trusty-memory import 2>&1 kuzu --discover",
            "cat <(trusty-memory import kuzu --discover)",
        ]),
        "kuzu-store-read",
    );
}

// #8625 D5b (owner ruling 2026-10-12): one normalised-text rule — a Bash call
// naming both `trusty-memory` and `kuzu` outside a quoted text slot denies.

#[test]
fn the_text_rule_denies_the_block_evasions() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            // A redirection glued to a word.
            "trusty-memory import kuzu>/tmp/o",
            "trusty-memory import kuzu&>/dev/null",
            "trusty-memory>/dev/null import kuzu",
            "trusty-memory import>/dev/null kuzu",
            "bash -c 'trusty-memory import kuzu>/tmp/o'",
            // Here-strings.
            "bash <<< 'trusty-memory import kuzu'",
            "sh<<<'trusty-memory import kuzu'",
            // An argument feeder with any placeholder.
            "echo kuzu | xargs -I % trusty-memory import %",
            "echo kuzu | xargs -J % trusty-memory import %",
            "parallel trusty-memory import ::: kuzu",
            // A `$`-quoted or `$`-prefixed program word.
            "$'trusty-memory' import kuzu",
            "$\"trusty-memory\" import kuzu",
            "${X}trusty-memory import kuzu",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn the_text_rule_folds_case() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "TRUSTY-MEMORY IMPORT KUZU",
            "TRUSTY-MEMORY IMPORT KUZU>/tmp/o",
            "Trusty-Memory import KuZu&>/dev/null",
            "echo KUZU | xargs -I % TRUSTY-MEMORY import %",
            "TRUSTY-memory MIGRATE KUZU-DATA",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn the_text_rule_strips_quotes_dollars_and_backslashes() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "$'trusty'-memory import kuzu",
            "t$'r'usty-memory import kuzu",
            "trusty-'memory' import k\"uz\"u",
            "tr\\usty-mem\\ory import ku\\zu",
            "trusty-memory import ku$''zu",
            "trusty-memory import $'kuzu'>/dev/null",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn the_text_rule_exempts_quoted_text_slots() {
    let fx = Fx::new();
    assert_allowed(
        &fx,
        &s(&[
            "gh issue comment 8625 --body \"the guard denies trusty-memory import kuzu\"",
            "gh issue comment 8625 --body 'trusty-memory import kuzu runs outside the guard'",
            "gh issue comment 8625 --body=\"trusty-memory import kuzu\"",
            "gh pr create --title \"fix(trusty-memory): kuzu deny\" --body \"Refs #8625\"",
            "git commit -m \"docs: trusty-memory import kuzu is denied\"",
            "git commit -m \"fix(trusty-memory): guard\" -m \"the kuzu import\"",
            "git commit -m'trusty-memory kuzu'",
        ]),
    );
    // A slot that runs code, an unclosed slot, an unquoted value, or a call
    // beside a slot still denies.
    assert_denied(
        &fx,
        &s(&[
            "gh issue comment 1 --body \"$(trusty-memory import kuzu)\"",
            "gh issue comment 1 --body \"`trusty-memory import kuzu`\"",
            "gh issue comment 1 --body \"x\"; trusty-memory import kuzu",
            "git commit -m \"trusty-memory\" && trusty-memory import kuzu",
            "gh issue comment 1 --body \"trusty-memory import kuzu",
            "git commit -m trusty-memory-kuzu",
        ]),
        "kuzu-store-read",
    );
}

#[test]
fn the_text_rule_denies_a_call_naming_both_outside_a_slot() {
    let fx = Fx::new();
    assert_denied(
        &fx,
        &s(&[
            "trusty-memory recall kuzu",
            "trusty-memory -v recall kuzu",
            "cargo test -p trusty-memory kuzu",
            "grep -rn \"trusty-memory import kuzu\" content/",
            "echo trusty-memory import kuzu is denied > notes.md",
            "cat crates/trusty-memory/src/commands/kuzu_import/mod.rs",
            "gh issue comment 1 --body-file - <<'EOF'\ntrusty-memory import kuzu\nEOF",
        ]),
        "kuzu-store-read",
    );
    // A call naming only one of the two words is not this rule's business.
    assert_allowed(
        &fx,
        &s(&[
            "trusty-memory import other",
            "trusty-memory doctor",
            "cargo test -p trusty-memory",
            "rg kuzu crates/trusty-search",
        ]),
    );
}

// #9794: the narrowed exception. Answers Q1-Q5 (Architect, 2026-10-12): the
// dry-run, export-refusals and review forms allow; `--python` denies with its
// own slug; the apply forms, and everything else, keep the D5b deny.

/// A session scratchpad path, in the macOS `/private/tmp` spelling.
const SCRATCH: &str = "/private/tmp/claude-502/-Users-masa-p/f6b52bfc-73b3/scratchpad";

#[test]
fn allows_the_dry_run_export_and_review_forms() {
    let fx = Fx::new();
    let review = "~/.trusty-tools/kuzu-review";
    let home = dirs::home_dir().expect("home").display().to_string();
    let env = "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu";
    assert_allowed(
        &fx,
        &[
            "trusty-memory import kuzu --discover --dry-run".into(),
            format!("{env} --dry-run --discover --root ~/p --max-depth 3"),
            format!("{env} --dry-run --export-refusals --discover"),
            format!("trusty-memory import kuzu --dry-run --discover 2>&1 | tee {review}/r.txt"),
            format!("{env} --dry-run --discover | tee {review}/R/report.txt"),
            "trusty-memory import kuzu --dry-run --discover 2>&1".into(),
            format!("{env} --dry-run --discover 2>&1 | tee {SCRATCH}/r.txt"),
            format!(
                "{env} --dry-run --discover | tee /tmp/claude-502/-Users-masa-p/f6b52bfc/scratchpad/a/r.txt"
            ),
            format!("{env} --dry-run --discover | tee {home}/.trusty-tools/kuzu-review/r.txt"),
            format!("{env} --dry-run --discover --root ~/a --root ~/b --bridge-timeout-secs 600"),
            format!(
                "{env} --dry-run --discover --review-ledger {review}/20261012T000000Z-ab12cd34/ledger.jsonl"
            ),
            format!("{env} --dry-run --update --discover"),
            format!("{env} --dry-run --from ~/p --palace x"),
            "trusty-memory import kuzu review record --run R --key 0a1b --verdict keep-refused"
                .into(),
            "trusty-memory import kuzu review record --run R --key 0a1b --verdict admit".into(),
            "trusty-memory import kuzu review record --run R --key 0a1b --verdict admit-redacted \
             --op redact_span:0 --op delete_span:1 --op drop_line:2"
                .into(),
            "trusty-memory import kuzu review status --run 20261012T000000Z-ab12cd34".into(),
            format!("{env} review finish --run R 2>&1 | tee {review}/R/finish.txt"),
        ],
    );
}

#[test]
fn denies_any_python_flag_with_its_own_slug() {
    let fx = Fx::new();
    let store = fx.store();
    let mut commands: Vec<String> = vec![
        "trusty-memory import kuzu --discover --dry-run --python ~/.trusty-tools/kuzu-import-venv/bin/python".into(),
        "trusty-memory import kuzu --discover --python /usr/bin/python3".into(),
        "trusty-memory import kuzu --discover --python=/bin/cat".into(),
        "trusty-memory import kuzu --discover --python".into(),
        "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --dry-run --discover --python /bin/cat"
            .into(),
        "trusty-memory import kuzu --discover --python=~/.trusty-tools/kuzu-import-venv/bin/python"
            .into(),
        "trusty-memory import kuzu review status --run R --python /bin/cat".into(),
    ];
    for py in [
        fx.venv("python"),
        fx.venv("python3"),
        fx.venv("python3.13"),
        format!("{}link/../python", fx.venv("")),
        fx.venv("cat"),
        "/Users/x/.trusty-tools/kuzu-import-venv/bin/python3".into(),
        "/bin/cat".into(),
        fx.root.join("elsewhere/python").display().to_string(),
    ] {
        commands.push(format!(
            "trusty-memory import kuzu --from {store} --python {py}"
        ));
        commands.push(format!(
            "trusty-memory import kuzu --dry-run --discover --python={py}"
        ));
    }
    assert_denied(&fx, &commands, "kuzu-import-python");
    let relative = bash_in(
        &fx.home,
        "trusty-memory import kuzu --discover --python .trusty-tools/kuzu-import-venv/bin/python",
    );
    assert!(
        relative.is_some_and(|r| r.contains("kuzu-import-python")),
        "relative --python"
    );
}

#[test]
fn the_allow_keeps_the_by_name_store_deny() {
    let fx = Fx::new();
    let store = fx.store();
    let p = fx.project.display();
    let env = "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --dry-run";
    assert_denied(
        &fx,
        &[
            format!("{env} --from {store}"),
            format!("{env} --from {p}/.kuzu-memory --export-refusals"),
            format!("{env} --discover --root {p}/.kuzu-memory"),
            format!("{env} --discover --review-ledger {p}/.kuzu-memory/ledger.jsonl"),
            format!("{env} --discover --root ~/.trusty-tools/kuzu-import-venv"),
            format!("{env} --discover | tee ~/.trusty-tools/kuzu-review/memories.db"),
            format!("{env} --discover --root /tmp/trusty-kuzu-import-ab12CD"),
        ],
        "kuzu-store-read",
    );
}

#[test]
fn the_exception_cannot_widen() {
    let fx = Fx::new();
    let ok = "trusty-memory import kuzu --dry-run --discover";
    let review = "~/.trusty-tools/kuzu-review";
    let rows: Vec<String> = vec![
        // Another import, subcommand or word order.
        "trusty-memory import kuzux --dry-run".into(),
        "trusty-memory import kuzu-data --dry-run".into(),
        "trusty-memory import other --dry-run --root ~/kuzu".into(),
        "trusty-memory migrate kuzu-data --dry-run".into(),
        "trusty-memory migrate kuzu-memory --dry-run".into(),
        "trusty-memory import kuzu foo --dry-run".into(),
        "trusty-memory import --dry-run kuzu".into(),
        "trusty-memory -v import kuzu --dry-run".into(),
        "trusty-memory recall kuzu --dry-run".into(),
        "trusty-memory import kuzu review --dry-run".into(),
        "trusty-memory import kuzu review delete --run R".into(),
        "trusty-memory import kuzu review record --run R --dry-run".into(),
        "trusty-memory import kuzu review status --run R --key K".into(),
        "trusty-memory import kuzu --dry-run --run R".into(),
        "trusty-memory import kuzu --dry-run --discover --root=~/p".into(),
        // A chained or substituted second command.
        format!("{ok}; ls"),
        format!("{ok} --root ~/p;ls"),
        format!("{ok} --max-depth 3;cat x"),
        format!("{ok} && cat x"),
        format!("{ok} || true"),
        format!("{ok} &"),
        format!("{ok} | cat"),
        format!("{ok} | tail -50"),
        format!("{ok}\nls"),
        format!("{ok}\r\nls"),
        format!("{ok}\t--root ~/p"),
        format!("{ok} --root $(id)"),
        format!("{ok} --root `id`"),
        format!("{ok} --root ${{HOME}}"),
        format!("{ok} --root ~/{{a,b}}"),
        format!("{ok} --root ~/p*"),
        format!("{ok} --root ~/p?"),
        format!("{ok} --root ~/[p]"),
        format!("{ok} --root 'a b'"),
        format!("{ok} --root \"a\""),
        format!("{ok} --root a\\ b"),
        format!("{ok} --root a#b"),
        // Each of `|`, `&`, `<`, `>` and a newline glued into an allowed value.
        format!("{ok} --root ~/p|sh"),
        format!("{ok} --root ~/p&&ls"),
        format!("{ok} --root ~/p&"),
        format!("{ok} --root ~/p>~/.zshrc"),
        format!("{ok} --root ~/p<x"),
        format!("{ok} --root ~/p\nls"),
        format!("{ok} --root !!"),
        format!("{ok} | tee {review}/r.txt | sh"),
        format!("{ok} | tee {review}/r.txt; rm -rf ~"),
        format!("{ok} | tee {review}/r.txt && ls"),
        format!("{ok} | tee {review}/$(id).txt"),
        format!("echo $({ok})"),
        format!("echo `{ok}`"),
        format!("bash -c '{ok}'"),
        format!("eval {ok}"),
        format!("cat <({ok})"),
        format!("({ok})"),
        format!("{{ {ok}; }}"),
        format!("xargs {ok}"),
        format!("{ok} <<< x"),
        format!(" {ok}"),
        format!("{ok} "),
        "trusty-memory  import kuzu --dry-run".into(),
        // A wrapper, a path or an uppercase program.
        "TRUSTY-MEMORY import kuzu --dry-run".into(),
        "trusty-memory IMPORT kuzu --dry-run".into(),
        "trusty-memory import KUZU --dry-run".into(),
        "trusty-memory import kuzu --DRY-RUN".into(),
        "/usr/local/bin/trusty-memory import kuzu --dry-run".into(),
        "./trusty-memory import kuzu --dry-run".into(),
        "~/.cargo/bin/trusty-memory import kuzu --dry-run".into(),
        "\\trusty-memory import kuzu --dry-run".into(),
        format!("nice {ok}"),
        format!("sudo {ok}"),
        format!("command {ok}"),
        format!("exec {ok}"),
        format!("time {ok}"),
        format!("env {ok}"),
        format!("env -u FOO {ok}"),
        format!("env -u TRUSTY_MEMORY_PALACE env -u TRUSTY_MEMORY_PALACE {ok}"),
        format!("env -i {ok}"),
        format!("PYTHONPATH=/tmp/x {ok}"),
        format!("env -u TRUSTY_MEMORY_PALACE PYTHONPATH=/tmp/x {ok}"),
        format!("trusty-memory(){{ cat x; }}; {ok}"),
        // A tee path outside the review root and the scratchpad.
        format!("{ok} | tee /tmp/r.txt"),
        format!("{ok} | tee ~/.cargo/bin/trusty-memory"),
        format!("{ok} 2>&1 | tee ~/.local/bin/kuzu-memory"),
        format!("{ok} | tee ~/.zshrc"),
        format!("{ok} | tee ~/.bashrc"),
        format!("{ok} | tee {review}/../../.zshrc"),
        format!("{ok} | tee {review}/./r.txt"),
        format!("{ok} | tee {review}//r.txt"),
        // A tee that would truncate a run's own artifacts (#9794).
        format!(
            "trusty-memory import kuzu review status --run R 2>&1 | tee {review}/R/ledger.jsonl"
        ),
        format!("trusty-memory import kuzu review status --run R | tee {review}/R/masked.jsonl"),
        format!("trusty-memory import kuzu review status --run R | tee {review}/R/ledger.seal"),
        format!(
            "trusty-memory import kuzu review status --run R | tee {review}/R/.masked.jsonl.partial"
        ),
        format!("{ok} | tee {review}/ledger.jsonl"),
        format!("{ok} | tee {SCRATCH}/R/ledger.jsonl"),
        format!("{ok} | tee {review}/"),
        format!("{ok} | tee {review}"),
        format!("{ok} | tee ~/.trusty-tools/kuzu-reviewx/r.txt"),
        format!("{ok} | tee ~root/.trusty-tools/kuzu-review/r.txt"),
        format!("{ok} | tee -a {review}/r.txt"),
        format!("{ok} | tee {review}/a {review}/b"),
        format!("{ok} | tee {review}/a ~/.zshrc"),
        format!("{ok} | tee"),
        format!("{ok} | tee r.txt"),
        format!("{ok} | tee crates/trusty-mpm/src/main.rs"),
        format!("{ok} | tee {SCRATCH}/../../../../../etc/r"),
        format!("{ok} | tee {SCRATCH}"),
        format!("{ok} | tee {SCRATCH}/"),
        format!("{ok} | tee /private/tmp/claude-x/a/b/scratchpad/r.txt"),
        format!("{ok} | tee /private/tmp/claude-/a/b/scratchpad/r.txt"),
        format!("{ok} | tee /private/tmp/claude-502/a/scratchpad/r.txt"),
        format!("{ok} | tee /tmp/claude-502/a/b/c/scratchpad/r.txt"),
        format!("{ok} | tee /tmp/claude-502/../b/scratchpad/r.txt"),
        format!("{ok} | tee /var/tmp/claude-502/a/b/scratchpad/r.txt"),
        format!("{ok} | tee /private/tmp/claude-502/a/b/notes/r.txt"),
        format!("{ok} > {review}/r.txt"),
        format!("{ok} >{review}/r.txt"),
        format!("{ok} 2>/dev/null"),
        format!("{ok} >/tmp/o"),
        format!("{ok} 2>&1 2>&1"),
        format!("{ok} 2>&1|tee {review}/r.txt"),
        format!("{ok}|tee {review}/r.txt"),
        format!("{ok} | tee {review}/r.txt 2>&1"),
        // An apply form: no `--dry-run`, or a flag swallowing it as a value.
        "trusty-memory import kuzu --discover".into(),
        "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --discover --root ~/p".into(),
        "trusty-memory import kuzu --discover --update".into(),
        "trusty-memory import kuzu --from ~/p --palace x".into(),
        format!(
            "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --discover --review-ledger {review}/R/ledger.jsonl"
        ),
        "trusty-memory import kuzu --export-refusals --discover".into(),
        format!("trusty-memory import kuzu --discover 2>&1 | tee {review}/r.txt"),
        "trusty-memory import kuzu --discover --palace --dry-run".into(),
        "trusty-memory import kuzu --discover --root --dry-run".into(),
        "trusty-memory import kuzu --from ~/p --review-ledger --dry-run".into(),
        "trusty-memory import kuzu --discover --dry-run=false".into(),
    ];
    assert_denied(&fx, &rows, "kuzu-store-read");
}

/// The `tm-memory-import` skill, embedded at compile time.
const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../content/skills/tm-memory-import.md"
));

/// Every line of a ```bash fence in the skill's steps 1-4 (the steps a PM
/// runs), with the `<RUN>`, `<KEY>` and `<i>` placeholders filled in.
fn skill_pm_commands() -> Vec<String> {
    let (pm_steps, _operator) = SKILL
        .split_once("## 5. The Import")
        .expect("step 5 heading");
    let mut in_fence = false;
    let mut commands = Vec::new();
    for line in pm_steps.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
        } else if in_fence {
            commands.push(
                line.replace("<RUN>", "R")
                    .replace("<KEY>", "K")
                    .replace("<i>", "0"),
            );
        }
    }
    commands
}

#[test]
fn every_command_the_skill_tells_a_pm_to_run_is_allowed() {
    let fx = Fx::new();
    let commands = skill_pm_commands();
    // Steps 1, 2 and 4 (one line each), three `review record`, status, finish.
    assert_eq!(commands.len(), 8, "{commands:#?}");
    for command in &commands {
        assert!(
            !command.ends_with('\\') && !command.contains('['),
            "a continuation or optional part in {command:?}"
        );
        assert_eq!(
            kuzu_import_exception(command, Some(&fx.home)),
            ImportException::Allow,
            "{command:?}"
        );
        assert_eq!(bash(&fx, command), None, "{command:?}");
    }
    assert!(SKILL.contains("Do not pass `--python`"));
}
