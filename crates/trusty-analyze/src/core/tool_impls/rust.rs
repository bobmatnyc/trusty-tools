//! `ClippyTool` — Rust diagnostics via `cargo clippy`, scoped to the project.
//!
//! Why: clippy is the canonical Rust linter; running it on demand surfaces
//! lints tree-sitter heuristics never catch. It is a BUILD, not a per-file
//! check: it needs a `Cargo.toml` and it compiles the whole crate graph either
//! way. #6018 found the previous per-file dispatch failed on both counts — it
//! ran clippy in a manifest-less scratch dir, so every invocation errored with
//! "could not find Cargo.toml" and returned `Ok(vec![])`, while still costing
//! ~0.155 s per file (10+ minutes on a 4097-file index) for zero diagnostics.
//! What: `ClippyTool` reports `is_project_scoped() == true`, so the dispatcher
//! hands it real on-disk paths and calls `run_project` ONCE per request.
//! `run_project` groups the files by their enclosing cargo root (workspace root
//! when one exists, else the nearest package), runs one `cargo clippy` per
//! root, parses that output once, and keeps the diagnostics belonging to the
//! requested files.
//! Test: `parse_clippy_diagnostics_extracts_warning` parses a captured message
//! line; `run_project_invokes_cargo_once_per_root_not_per_file` proves the
//! fan-out collapse.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{build_tool_timeout, run_command_with_timeout};
use crate::core::tools::{Severity, StaticTool, ToolDiagnostic};

/// Highest number of parent directories walked while looking for a cargo root.
///
/// Why: bounds the walk so a path outside any project cannot climb to `/` on
/// every file. 64 is far past any realistic source-tree depth.
const MAX_MANIFEST_WALK: usize = 64;

/// Rust static-analysis tool backed by `cargo clippy`.
pub struct ClippyTool;

impl StaticTool for ClippyTool {
    fn name(&self) -> &str {
        "clippy"
    }

    fn language(&self) -> &str {
        "rust"
    }

    fn is_available(&self) -> bool {
        which::which("cargo").is_ok()
    }

    /// Analyze one file by running its project once.
    ///
    /// Why: the dispatcher never takes this path for clippy (it is
    /// project-scoped), but the trait requires `run`, and a direct caller
    /// holding a real on-disk path should still get real diagnostics rather
    /// than the empty vec the scratch-dir version always returned (#6018).
    /// What: delegates to `run_project` with a single-element slice. No
    /// recursion — `run_project` is overridden below and never calls back here.
    /// Test: covered by `run_project_filters_to_requested_files`, which
    /// exercises the same path with the invocation injected.
    fn run(&self, file: &Path, _content: &str) -> Result<Vec<ToolDiagnostic>> {
        self.run_project(&[file.to_path_buf()])
    }

    /// Returns true: clippy compiles a cargo project, not a loose file.
    ///
    /// Why: `cargo clippy` fails outright without a `Cargo.toml`, which is
    /// exactly what the dispatcher's per-file scratch dir lacks. Declaring
    /// project scope routes clippy to `run_project` with real paths instead
    /// (#6018).
    /// What: always returns `true`.
    /// Test: `clippy_is_project_scoped`.
    fn is_project_scoped(&self) -> bool {
        true
    }

    /// Run `cargo clippy` once per cargo root and return the diagnostics that
    /// belong to `files`.
    ///
    /// Why: one build covers every file under a root, so N files cost one
    /// invocation rather than N. This is the whole point of #6018.
    /// What: delegates to `run_project_with`, supplying the real
    /// `cargo clippy --workspace --message-format=json --quiet` invocation
    /// under the build-class timeout (`build_tool_timeout`, 300 s default) —
    /// the per-file 30 s cap is far too short for a cold compile.
    /// Test: `run_project_invokes_cargo_once_per_root_not_per_file` and
    /// `run_project_filters_to_requested_files` inject the invocation instead
    /// of spawning cargo.
    fn run_project(&self, files: &[PathBuf]) -> Result<Vec<ToolDiagnostic>> {
        run_project_with(files, clippy_stdout)
    }
}

/// Invoke `cargo clippy` at `root` and return its stdout, or `None` on failure.
///
/// Why: separating the spawn from the grouping/filtering logic is what lets
/// `run_project_with` be tested without a cargo toolchain.
/// What: shells out with `--workspace` so a workspace root covers all members
/// in one build. `--all-targets` is deliberately omitted: it would compile the
/// test and bench targets too, and this endpoint is bounded by a per-request
/// deadline. A failed spawn or a timeout logs at warn and yields `None`, which
/// the caller treats as "this root produced no diagnostics".
/// Test: not unit-tested (spawns cargo); the callers inject a fake.
fn clippy_stdout(root: &Path) -> Option<String> {
    match run_command_with_timeout(
        "cargo",
        &["clippy", "--workspace", "--message-format=json", "--quiet"],
        root,
        build_tool_timeout(),
    ) {
        Ok(o) => Some(o.stdout),
        Err(e) => {
            tracing::warn!("clippy invocation failed in {}: {e:#}", root.display());
            None
        }
    }
}

/// Group `files` by cargo root, invoke once per root, keep the matching diags.
///
/// Why: the invocation is the only part that needs a toolchain, so taking it as
/// a parameter makes the once-per-root contract directly testable — a counting
/// closure proves the fan-out collapsed (#6018).
/// What: buckets `files` by [`find_cargo_root`]; for each root calls `invoke`
/// exactly once; parses that stdout ONCE via [`parse_clippy_diagnostics`]
/// (never once per file — that would be quadratic on a large corpus); resolves
/// each diagnostic's root-relative `file_name` against the root and keeps it
/// only when the resulting absolute path is one of the requested files. Files
/// under no cargo root are skipped. The emitted `file` is absolute, matching
/// the contract the dispatcher's `abs_to_rel` expects from project-scoped
/// tools.
/// Test: `run_project_invokes_cargo_once_per_root_not_per_file`,
/// `run_project_filters_to_requested_files`,
/// `run_project_skips_files_outside_any_cargo_root`.
fn run_project_with<F>(files: &[PathBuf], mut invoke: F) -> Result<Vec<ToolDiagnostic>>
where
    F: FnMut(&Path) -> Option<String>,
{
    let mut by_root: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for f in files {
        match find_cargo_root(f) {
            Some(root) => by_root.entry(root).or_default().push(f.clone()),
            None => tracing::debug!("clippy: no Cargo.toml above {}; skipping", f.display()),
        }
    }

    let mut out = Vec::new();
    for (root, group) in &by_root {
        let Some(stdout) = invoke(root) else {
            continue;
        };
        let wanted: HashSet<&PathBuf> = group.iter().collect();
        for mut diag in parse_clippy_diagnostics(&stdout) {
            // clippy reports spans relative to the cargo root. `join` leaves an
            // already-absolute span path untouched, so both forms resolve.
            let abs = root.join(&diag.file);
            if wanted.contains(&abs) {
                diag.file = abs.to_string_lossy().into_owned();
                out.push(diag);
            }
        }
    }
    Ok(out)
}

/// Nearest enclosing cargo root for `file` — the workspace root when one exists.
///
/// Why: running clippy at each package manifest would still spawn one build per
/// crate (21 for this workspace). Preferring the outermost manifest that
/// declares `[workspace]` collapses that to a single `cargo clippy --workspace`.
/// What: walks up to [`MAX_MANIFEST_WALK`] parent directories, recording the
/// nearest directory holding a `Cargo.toml` and the outermost one whose
/// manifest declares a workspace. Returns the workspace root if found, else the
/// nearest package root, else `None`.
/// Test: `find_cargo_root_prefers_workspace_root` and
/// `find_cargo_root_returns_none_outside_a_project`.
fn find_cargo_root(file: &Path) -> Option<PathBuf> {
    let mut nearest: Option<PathBuf> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut dir = file.parent();
    for _ in 0..MAX_MANIFEST_WALK {
        let Some(d) = dir else { break };
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() {
            if nearest.is_none() {
                nearest = Some(d.to_path_buf());
            }
            if manifest_declares_workspace(&manifest) {
                workspace = Some(d.to_path_buf());
            }
        }
        dir = d.parent();
    }
    workspace.or(nearest)
}

/// True when `manifest` carries a `[workspace]` or `[workspace.*]` table.
fn manifest_declares_workspace(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest)
        .map(|s| {
            s.lines().any(|l| {
                let t = l.trim();
                t == "[workspace]" || t.starts_with("[workspace.")
            })
        })
        .unwrap_or(false)
}

/// Parse newline-delimited cargo JSON messages into every diagnostic they carry.
///
/// Why: `run_project_with` needs the whole set once, then filters — parsing the
/// same stdout once per requested file would be quadratic on a large corpus.
/// What: reads each line as a cargo build message and converts the wrapped
/// compiler `message` object. `file` is left as the span's own path (relative
/// to the cargo root); the caller resolves it.
/// Test: `parse_clippy_diagnostics_keeps_every_file`.
fn parse_clippy_diagnostics(stdout: &str) -> Vec<ToolDiagnostic> {
    let mut diags = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(root) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // cargo wraps the compiler message under `message` for build messages.
        let Some(msg) = root.get("message") else {
            continue;
        };
        if let Some(d) = clippy_message_to_diag(msg) {
            diags.push(d);
        }
    }
    diags
}

/// Convert a single rustc/clippy `message` object into a `ToolDiagnostic`.
///
/// Keeps every file: since #6018 clippy runs project-scoped, so filtering to
/// the requested subset happens once in `run_project_with` rather than here.
fn clippy_message_to_diag(msg: &Value) -> Option<ToolDiagnostic> {
    let level = msg.get("level").and_then(Value::as_str).unwrap_or("");
    if level == "note" || level.is_empty() {
        return None;
    }
    let spans = msg.get("spans").and_then(Value::as_array)?;
    let span = spans
        .iter()
        .find(|s| {
            s.get("is_primary")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .or_else(|| spans.first())?;

    let span_file = span.get("file_name").and_then(Value::as_str)?;

    let line = span.get("line_start").and_then(Value::as_u64).unwrap_or(0) as u32;
    let col = span
        .get("column_start")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let message = msg
        .get("rendered")
        .and_then(Value::as_str)
        .or_else(|| msg.get("message").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    let code = msg
        .get("code")
        .and_then(|c| c.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(ToolDiagnostic {
        tool: "clippy".into(),
        file: span_file.to_string(),
        line,
        col,
        severity: severity_from_level(level),
        code,
        message,
    })
}

/// Map a rustc level string to a `Severity`.
fn severity_from_level(level: &str) -> Severity {
    match level {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "help" => Severity::Hint,
        _ => Severity::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One cargo build message for `file` at `line`, as clippy emits it.
    fn warning_line(file: &str, line: u32) -> String {
        format!(
            r#"{{"reason":"compiler-message","message":{{"level":"warning","message":"unneeded return","rendered":"warning: unneeded return statement","code":{{"code":"clippy::needless_return"}},"spans":[{{"is_primary":true,"file_name":"{file}","line_start":{line},"column_start":5}}]}}}}"#
        )
    }

    /// Create `root/Cargo.toml` (workspace root when `workspace`) plus the
    /// given root-relative source files, and return their absolute paths.
    fn scaffold(root: &Path, workspace: bool, rel_files: &[&str]) -> Vec<PathBuf> {
        let manifest = if workspace {
            "[workspace]\nmembers = []\n"
        } else {
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"
        };
        std::fs::create_dir_all(root).expect("mkdir root");
        std::fs::write(root.join("Cargo.toml"), manifest).expect("write manifest");
        rel_files
            .iter()
            .map(|rel| {
                let abs = root.join(rel);
                std::fs::create_dir_all(abs.parent().expect("has parent")).expect("mkdir");
                std::fs::write(&abs, "fn demo() {}\n").expect("write source");
                abs
            })
            .collect()
    }

    #[test]
    fn parse_clippy_diagnostics_extracts_warning() {
        let diags = parse_clippy_diagnostics(&warning_line("src/main.rs", 7));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "src/main.rs");
        assert_eq!(diags[0].line, 7);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].code.as_deref(), Some("clippy::needless_return"));
    }

    #[test]
    fn parse_clippy_diagnostics_skips_notes() {
        let note = r#"{"message":{"level":"note","message":"n","spans":[{"is_primary":true,"file_name":"src/main.rs","line_start":1,"column_start":1}]}}"#;
        assert!(parse_clippy_diagnostics(note).is_empty());
    }

    #[test]
    fn parse_clippy_diagnostics_tolerates_garbage() {
        assert!(parse_clippy_diagnostics("not json\n{}\n").is_empty());
    }

    /// Why: #6018's cost driver. `ClippyTool` must declare project scope so the
    /// dispatcher calls `run_project` with real on-disk paths instead of
    /// writing each file to a manifest-less scratch dir and calling `run`,
    /// where `cargo clippy` errors "could not find Cargo.toml" every time.
    /// Test: this test. Fails against the pre-#6018 impl, which inherited the
    /// trait default of `false`.
    #[test]
    fn clippy_is_project_scoped() {
        assert!(
            ClippyTool.is_project_scoped(),
            "clippy compiles a cargo project; per-file dispatch cannot work"
        );
    }

    /// Why: the whole point of #6018 — clippy used to spawn once per file
    /// (0.155 s x 4097 files = 10+ minutes, all of it wasted). It must now
    /// spawn once per cargo root no matter how many files are requested.
    /// What: scaffolds a single-package tree with 8 source files, calls
    /// `run_project_with` with a closure that records each invocation, and
    /// asserts exactly one invocation, at the package root.
    /// Test: this test. Against the pre-#6018 impl (no `run_project` override)
    /// the trait default calls `run` once per file — 8 invocations.
    #[test]
    fn run_project_invokes_cargo_once_per_root_not_per_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let rel: Vec<String> = (0..8).map(|i| format!("src/a/f{i}.rs")).collect();
        let rel_refs: Vec<&str> = rel.iter().map(String::as_str).collect();
        let files = scaffold(root, false, &rel_refs);

        let calls = RefCell::new(Vec::<PathBuf>::new());
        let diags = run_project_with(&files, |dir| {
            calls.borrow_mut().push(dir.to_path_buf());
            Some(String::new())
        })
        .expect("run_project_with");

        let recorded = calls.borrow();
        assert_eq!(
            recorded.len(),
            1,
            "clippy must be invoked once per cargo root for 8 files, got {} invocations: {:?}",
            recorded.len(),
            recorded
        );
        assert_eq!(recorded[0], root, "invocation must run at the package root");
        assert!(diags.is_empty(), "empty stdout yields no diagnostics");
    }

    /// Why: one build reports the whole crate graph, so the requested subset
    /// must be filtered out of it — otherwise the endpoint returns diagnostics
    /// for files the caller never indexed.
    /// What: feeds one warning for a requested file and one for a sibling that
    /// was not requested; asserts only the requested one survives, rewritten to
    /// its absolute path (the contract `abs_to_rel` expects).
    /// Test: this test.
    #[test]
    fn run_project_filters_to_requested_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let files = scaffold(root, false, &["src/wanted.rs", "src/ignored.rs"]);
        let requested = vec![files[0].clone()];

        let stdout = format!(
            "{}\n{}\n",
            warning_line("src/wanted.rs", 3),
            warning_line("src/ignored.rs", 9)
        );
        let diags =
            run_project_with(&requested, |_| Some(stdout.clone())).expect("run_project_with");

        assert_eq!(
            diags.len(),
            1,
            "only the requested file may survive: {diags:?}"
        );
        assert_eq!(diags[0].line, 3);
        assert_eq!(
            diags[0].file,
            files[0].to_string_lossy(),
            "project-scoped diagnostics must carry absolute paths"
        );
    }

    /// Why: a file with no `Cargo.toml` above it (the old scratch-dir case)
    /// must be skipped without spawning cargo, not passed to a doomed build.
    /// Test: this test.
    #[test]
    fn run_project_skips_files_outside_any_cargo_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let orphan = tmp.path().join("loose.rs");
        std::fs::write(&orphan, "fn x() {}\n").expect("write source");

        let calls = RefCell::new(0usize);
        let diags = run_project_with(&[orphan], |_| {
            *calls.borrow_mut() += 1;
            Some(String::new())
        })
        .expect("run_project_with");

        assert_eq!(*calls.borrow(), 0, "no cargo root means no invocation");
        assert!(diags.is_empty());
    }

    /// Why: stopping at the nearest package manifest would still cost one build
    /// per member crate (21 in this workspace). The walk must prefer the
    /// outermost manifest that declares `[workspace]`.
    /// Test: this test.
    #[test]
    fn find_cargo_root_prefers_workspace_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = tmp.path();
        scaffold(ws, true, &[]);
        let member = ws.join("crates/demo");
        let files = scaffold(&member, false, &["src/lib.rs"]);

        assert_eq!(
            find_cargo_root(&files[0]).as_deref(),
            Some(ws),
            "workspace root must win over the nearer member manifest"
        );
    }

    #[test]
    fn find_cargo_root_returns_none_outside_a_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let loose = tmp.path().join("loose.rs");
        std::fs::write(&loose, "fn x() {}\n").expect("write source");
        assert_eq!(find_cargo_root(&loose), None);
    }
}
