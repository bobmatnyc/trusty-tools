//! #12028 P1i: plain runs of the shipped `trusty-architect-ruling-pack`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! these prove the shipped binary finds the Architect checkout (marked by
//! `ARCHITECT.md`) from a working directory below it, reads the default key
//! map `<checkout>/local/project-keys.json`, talks to real `trusty-memory`,
//! `trusty-search` and `rg` executables on `PATH` (stubs here), starts the
//! search without `TRUSTY_INDEX`, and writes the pack under the checkout's
//! `local/packs` only. Without a checkout it needs `--keys` and `--no-write`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-ruling-pack");

const MEMORY: &str = r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$(dirname "$0")/memory-requests"
  id=$(printf '%s\n' "$line" | sed -n 's/^{"jsonrpc": "2.0", "id": \([0-9]*\).*/\1/p')
  case $id in
  '') ;;
  1) echo '{"jsonrpc": "2.0", "id": 1, "result": {}}' ;;
  *) echo '{"jsonrpc": "2.0", "id": '"$id"', "result": {"content": [{"type": "text", "text": "{\"results\": [{\"content\": \"prior ruling S1\"}]}"}]}}' ;;
  esac
done
"#;

const SEARCH: &str = r#"#!/bin/sh
env > "$(dirname "$0")/search-env"
echo 'warming'
echo '{"results": [{"path": "src/a.rs", "start_line": 2, "compact_snippet": "fn sample_fn()"}], "meta": {"last_indexed": "T1"}}'
"#;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A checkout with `ARCHITECT.md`, a sample key map whose root is the
/// checkout's `proj/`, and `sub/` to run from; plus the three stubs.
fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let co = tempfile::tempdir().unwrap();
    let root = co.path();
    std::fs::write(root.join("ARCHITECT.md"), "# Architect\n").unwrap();
    std::fs::create_dir_all(root.join("local")).unwrap();
    std::fs::create_dir_all(root.join("proj/src")).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let keys = format!(
        r#"{{"projects": {{"sample-app": {{"palace": "sample-app", "index": "sample-idx",
        "repo": "example-org/sample-app", "gh_config": "gh-example", "root": "{}/proj"}}}}}}"#,
        root.display()
    );
    std::fs::write(root.join("local/project-keys.json"), keys).unwrap();
    let stubs = tempfile::tempdir().unwrap();
    script(stubs.path(), "trusty-memory", MEMORY);
    script(stubs.path(), "trusty-search", SEARCH);
    let rg = format!(
        "#!/bin/sh\necho \"$*\" >> '{}/rg-argv'\necho \"{}/proj/src/a.rs:2:fn sample_fn() {{}}\"\n",
        stubs.path().display(),
        root.display()
    );
    script(stubs.path(), "rg", &rg);
    (co, stubs)
}

fn run_bin(cwd: &Path, stubs: &Path, args: &[&str]) -> Output {
    // A cleared environment: only PATH and a TRUSTY_INDEX the search must not see.
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", stubs.display()))
        .env("TRUSTY_INDEX", "wrong-index")
        .output()
        .expect("the binary runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn shipped_ruling_pack_runs_all_three_backends() {
    let (co, stubs) = setup();
    let args = ["sample-app", "does sample_fn hold?", "--checked"];
    let out = run_bin(&co.path().join("sub"), stubs.path(), &args);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    for want in [
        "- [sample-app] prior ruling S1\n",
        "- [supervisor] prior ruling S1\n",
        "- src/a.rs:2 fn sample_fn()\n",
        "- grep `sample_fn`: src/a.rs:2:fn sample_fn() {}\n",
        "Index: sample-idx (last_indexed T1).",
        "Confidence: HIGH.",
    ] {
        assert!(stdout.contains(want), "missing {want:?} in {stdout}");
    }
    let requests = std::fs::read_to_string(stubs.path().join("memory-requests")).unwrap();
    assert_eq!(
        requests.lines().count(),
        4,
        "initialize, notice, two recalls"
    );
    let env = std::fs::read_to_string(stubs.path().join("search-env")).unwrap();
    assert!(!env.contains("TRUSTY_INDEX"), "{env}");
    // The pack lands under the checkout's local/packs, and nowhere else.
    let packs: Vec<_> = std::fs::read_dir(co.path().join("local/packs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(packs.len(), 1, "{packs:?}");
    let name = packs[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("Z-sample-app.md"), "{name}");
    let body = std::fs::read_to_string(&packs[0]).unwrap();
    // The checkout comes from the working directory, so the path is canonical
    // (macOS `/var` is `/private/var`).
    let canonical = std::fs::canonicalize(&packs[0]).unwrap();
    let printed = format!("\npack file: {}\n", canonical.display());
    assert_eq!(stdout, format!("{body}{printed}"));
    assert!(!co.path().join("sub/local").exists());
}

#[test]
fn shipped_ruling_pack_without_a_checkout_needs_keys_and_no_write() {
    let (co, stubs) = setup();
    let away = tempfile::tempdir().unwrap();
    let out = run_bin(away.path(), stubs.path(), &["sample-app", "q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no Architect checkout"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !stubs.path().join("memory-requests").exists(),
        "no backend call"
    );
    let keys = co.path().join("local/project-keys.json");
    let args = [
        "sample-app",
        "q",
        "--keys",
        keys.to_str().unwrap(),
        "--no-write",
    ];
    let out = run_bin(away.path(), stubs.path(), &args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("# Ruling pack: sample-app @ "));
    assert_eq!(
        std::fs::read_dir(away.path()).unwrap().count(),
        0,
        "nothing written"
    );
}
