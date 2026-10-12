//! Unit tests for the #8625 kuzu-store rule (`pm_guard_kuzu_store.rs`).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

/// A scratch tree: a home with the import venv shaped like the host one
/// (`bin/python3` and `bin/python3.13` → `python` → a CPython outside the
/// venv), a project holding a store, and a CPython symlink planted elsewhere.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new(with_venv: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let project = root.join("p");
        std::fs::create_dir_all(project.join(".kuzu-memory")).expect("mkdir store");
        std::fs::write(project.join(".kuzu-memory/memories.db"), "kuzu").expect("store");
        let cpython = root.join("uv/cpython/bin");
        std::fs::create_dir_all(&cpython).expect("mkdir cpython");
        std::fs::write(cpython.join("python3.13"), "#!").expect("cpython");
        let bin = home.join(".trusty-tools/kuzu-import-venv/bin");
        std::fs::create_dir_all(&bin).expect("mkdir venv");
        if with_venv {
            symlink(cpython.join("python3.13"), bin.join("python")).expect("python");
            symlink("python", bin.join("python3")).expect("python3");
            symlink("python", bin.join("python3.13")).expect("python3.13");
        }
        for planted in ["elsewhere", "elsewhere/kuzu-import-venv/bin"] {
            std::fs::create_dir_all(root.join(planted)).expect("mkdir planted");
            symlink(
                cpython.join("python3.13"),
                root.join(planted).join("python"),
            )
            .expect("planted python");
        }
        Self {
            _dir: dir,
            root,
            home,
            project,
        }
    }

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
    bash_in(fx, &fx.project, command)
}

/// The verdict on a Bash `command` run from `cwd`.
fn bash_in(fx: &Fx, cwd: &Path, command: &str) -> Option<String> {
    let input = json!({ "command": command });
    evaluate_kuzu_store_access("Bash", Some(&input), cwd, Some(&fx.home))
}

/// The verdict on a `tool` call with `input`.
fn tool(fx: &Fx, tool: &str, input: Value) -> Option<String> {
    evaluate_kuzu_store_access(tool, Some(&input), &fx.project, Some(&fx.home))
}

fn assert_denied(fx: &Fx, commands: &[String], slug: &str) {
    for command in commands {
        let deny = bash(fx, command);
        assert!(
            deny.as_deref().is_some_and(|r| r.contains(slug)),
            "expected a `{slug}` deny for {command:?}, got {deny:?}"
        );
    }
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
    let fx = Fx::new(true);
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
    let fx = Fx::new(true);
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
    let fx = Fx::new(true);
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
    let fx = Fx::new(true);
    assert_denied(
        &fx,
        &s(&["cat \"memories.db", "cat '.kuzu-memory/x"]),
        "kuzu-store-read",
    );
}

#[test]
fn denies_a_glob_that_can_match_a_deny_name() {
    let fx = Fx::new(true);
    assert_denied(
        &fx,
        &s(&[
            "cat *.db",
            "cat mem*",
            "cat x/mem[o]ries.db",
            "cat .kuzu-m*/mem*.db",
            "cat .kuzu-memory/*",
            "cat /tmp/*kuzu-import-ab*/export.json",
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
        &s(&["cat *", "cat */*.md", "cat *.rs", "head -n1 *.json"]),
    );
    for (name, input) in [
        ("Grep", json!({ "pattern": "x", "glob": "*.{rs,toml}" })),
        ("Glob", json!({ "pattern": "**/*.json" })),
    ] {
        assert_eq!(tool(&fx, name, input.clone()), None, "{name} {input}");
    }
}

#[test]
fn allows_the_sanctioned_import() {
    let fx = Fx::new(true);
    let p = fx.project.display();
    assert_allowed(
        &fx,
        &[
            "trusty-memory import kuzu --discover --dry-run".into(),
            format!(
                "trusty-memory import kuzu --from {p}/.kuzu-memory --dry-run --export-refusals"
            ),
            format!(
                "env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --from {p}/.kuzu-memory/memories.db --palace x"
            ),
            "trusty-memory import kuzu review record --run R --key K --verdict keep-refused".into(),
            format!(
                "trusty-memory import kuzu --from {p}/.kuzu-memory --review-ledger ~/.trusty-tools/kuzu-review/R/ledger.jsonl"
            ),
            "trusty-memory import kuzu --dry-run 2>&1 | tee /tmp/report.txt".into(),
        ],
    );
    let masked = fx.home.join(".trusty-tools/kuzu-review/R/masked.jsonl");
    assert_eq!(tool(&fx, "Read", json!({ "file_path": masked })), None);
}

#[test]
fn allows_the_venv_interpreter() {
    let fx = Fx::new(true);
    let store = fx.store();
    let mut commands = Vec::new();
    for name in ["python", "python3", "python3.13"] {
        let py = fx.venv(name);
        commands.push(format!(
            "trusty-memory import kuzu --from {store} --python {py}"
        ));
        commands.push(format!(
            "trusty-memory import kuzu --discover --python={py}"
        ));
    }
    commands.push(
        "trusty-memory import kuzu --python ~/.trusty-tools/kuzu-import-venv/bin/python3".into(),
    );
    assert_allowed(&fx, &commands);
}

#[test]
fn denies_a_non_venv_python_program() {
    let fx = Fx::new(true);
    let store = fx.store();
    let cpython = fx.root.join("uv/cpython/bin/python3.13");
    let mut commands = Vec::new();
    for py in [
        "/bin/cat".to_string(),
        "/usr/bin/python3".into(),
        cpython.display().to_string(),
        fx.venv("pip"),
        fx.venv("python3.x"),
        fx.home
            .join(".trusty-tools/kuzu-import-venv/lib/python")
            .display()
            .to_string(),
    ] {
        commands.push(format!(
            "trusty-memory import kuzu --from {store} --python {py}"
        ));
        commands.push(format!(
            "trusty-memory import kuzu --discover --python={py}"
        ));
    }
    commands.push("trusty-memory import kuzu --discover --python".into());
    assert_denied(&fx, &commands, "kuzu-import-python");
}

#[test]
fn denies_a_planted_symlink_to_the_same_cpython() {
    let fx = Fx::new(true);
    let mut commands = Vec::new();
    for planted in ["elsewhere/python", "elsewhere/kuzu-import-venv/bin/python"] {
        let py = fx.root.join(planted).display().to_string();
        commands.push(format!(
            "trusty-memory import kuzu --discover --python {py}"
        ));
    }
    assert_denied(&fx, &commands, "kuzu-import-python");
}

#[test]
fn denies_when_the_venv_interpreter_cannot_be_resolved() {
    // Fail-Open Check: the lexical path is right, canonicalisation errors.
    let fx = Fx::new(false);
    let command = format!(
        "trusty-memory import kuzu --discover --python {}",
        fx.venv("python")
    );
    assert_denied(&fx, std::slice::from_ref(&command), "kuzu-import-python");
    let input = json!({ "command": command });
    let deny = evaluate_kuzu_store_access("Bash", Some(&input), &fx.project, None);
    assert!(
        deny.is_some_and(|r| r.contains("kuzu-import-python")),
        "no home"
    );
}

#[test]
fn denies_the_deprecated_migrate_aliases() {
    let fx = Fx::new(true);
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
fn the_exception_cannot_be_widened() {
    let fx = Fx::new(true);
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
    let fx = Fx::new(true);
    assert_allowed(
        &fx,
        &[
            "grep -rn memories.db docs/".into(),
            "rg kuzu-memory crates/".into(),
            "git commit -m \"fix memories.db import\"".into(),
            "echo kuzu-memory is retired".into(),
            "ls .kuzu-memory".into(),
            format!("stat {}", fx.store()),
            "cargo test -p trusty-memory kuzu".into(),
            "gh issue comment 1 --body \"the .kuzu-memory store\"".into(),
            "sed -n '/x.*y/p' notes.md".into(),
            "trusty-memory recall kuzu".into(),
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
    let fx = Fx::new(true);
    let link = fx.root.join("innocent.txt");
    symlink(fx.store(), &link).expect("link");
    let deny = tool(&fx, "Read", json!({ "file_path": link }));
    assert!(deny.is_some_and(|r| r.contains("kuzu-store-read")));
}

#[test]
fn denies_when_a_tool_path_cannot_be_resolved() {
    // Fail-Open Check: a symlink loop makes resolution error; the call denies.
    let fx = Fx::new(true);
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
    let fx = Fx::new(true);
    let store_dir = fx.project.join(".kuzu-memory");
    for command in ["cat *", "head -c 100 *"] {
        let deny = bash_in(&fx, &store_dir, command);
        assert!(
            deny.is_some_and(|r| r.contains("kuzu-store-read")),
            "{command}"
        );
    }
}
