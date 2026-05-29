//! Wave-decomposition shapes for `assignments.json` and their validation /
//! topological-repair logic (#88, #114, #162).
//!
//! Why: The plan-agent emits an `assignments.json` describing a topologically
//! layered set of files for the wave loop to implement. Parsing, validating,
//! and (where possible) repairing that document is intricate enough to warrant
//! its own file separate from the top-level workflow/phase config (#359).
//! What: `FileAssignment` / `WaveDef` / `Assignments` plus the `validate`,
//! `repair_wave_ordering`, and best-effort `load` entry points.
//! Test: `assignments_load_*`, `validate_file_path_*`, `validate_assignments_*`,
//! and `wave_validator_*` tests in the parent module's `tests` submodule.

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

/// #88: One file assignment within a wave, produced by the plan-agent.
///
/// Why: The wave loop runs one code-agent per file; each agent needs its own
/// scoped instruction (path, purpose, line budget, dependency list) plus a
/// pointer to its stub so it can implement the signatures exactly as planned.
/// What: Deserializes from the `files[]` array inside each wave entry of
/// `assignments.json`.
/// Test: `assignments_load_parses_valid_json` round-trips a two-wave document.
#[derive(Debug, Deserialize, Clone)]
pub struct FileAssignment {
    /// Destination path within `out_dir` (e.g. `src/foo.py`). The code-agent
    /// may ONLY write to this path.
    pub path: String,
    /// Stub file name (relative to `out_dir/stubs/`) the code-agent reads to
    /// learn the signatures it must implement verbatim. `None` when the
    /// plan-agent emits `"stub": null` (e.g. for `__init__.py` files that
    /// need no implementation scaffold).
    pub stub: Option<String>,
    /// One-line purpose the agent gets in its prompt so it knows the intent
    /// without having to re-derive it from the stub.
    pub purpose: String,
    /// Relative paths of files this one depends on — the agent reads them for
    /// context before writing its own file. Defaults to empty.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Optional maximum line budget; the prompt advertises this so agents
    /// self-constrain. `None` falls back to a default (300) at the caller.
    pub max_lines: Option<u32>,
}

/// #88: One wave in the topological decomposition.
///
/// Why: Waves group files that can be implemented once all earlier waves are
/// done; within a wave files are still run sequentially so cost stays bounded
/// and conflicts are impossible.
/// What: `wave` is the 1-indexed ordinal, `files` is the ordered list of
/// assignments for that wave.
/// Test: Shares the `assignments_load_parses_valid_json` fixture.
#[derive(Debug, Deserialize, Clone)]
pub struct WaveDef {
    pub wave: u32,
    pub files: Vec<FileAssignment>,
}

/// #88: Top-level shape of `out_dir/assignments.json`.
///
/// Why: The workflow engine treats the presence of this file as the switch
/// from monolithic single-agent code-phase to per-file wave loop. Keeping the
/// schema small and self-contained lets the plan-agent emit it directly.
/// What: `error_convention` is an optional global hint (e.g. "exceptions",
/// "Result"). `waves` is the ordered list of waves.
/// Test: `assignments_load_returns_none_for_missing_file`,
/// `assignments_load_parses_valid_json`,
/// `assignments_load_returns_none_for_invalid_json`.
#[derive(Debug, Deserialize, Clone)]
pub struct Assignments {
    pub error_convention: Option<String>,
    pub waves: Vec<WaveDef>,
}

impl Assignments {
    /// Validate that a single wave-file path is safe to join with `out_dir` (#114).
    ///
    /// Why: `run_wave_loop` does `out_dir.join(&file.path)` to both read and
    /// write the agent's assigned file. Without validation a malicious or
    /// buggy plan-agent could emit `../../src/main.rs` or `/etc/passwd` and
    /// have the code-agent overwrite files outside the workflow output dir.
    /// Also emits a non-fatal warning (#152) when a flat filename looks like
    /// it may be encoding directory structure with underscores instead of
    /// slashes (e.g. `stages_extraction.py` instead of `stages/extraction.py`).
    /// What: Rejects (1) empty strings, (2) absolute paths, (3) any component
    /// equal to `..`. Emits a tracing WARN for suspiciously flat filenames.
    /// Lexical-only — we don't touch the filesystem here.
    /// Test: `validate_file_path_*` unit tests in this module.
    pub fn validate_file_path(path: &str) -> Result<()> {
        anyhow::ensure!(!path.is_empty(), "file path is empty");
        let p = Path::new(path);
        anyhow::ensure!(
            p.is_relative(),
            "file path '{}' is absolute; must be relative to out_dir",
            path,
        );
        for component in p.components() {
            if matches!(component, std::path::Component::ParentDir) {
                anyhow::bail!(
                    "file path '{}' contains '..' component; refusing to write outside out_dir",
                    path,
                );
            }
        }

        // #152: Heuristic warning for paths that appear to encode directory
        // structure using underscores instead of slashes. We only check the
        // final filename component (not intermediate directories) and only
        // fire when the path is flat (no `/` parent) and the stem (filename
        // without extension) is long and contains multiple underscores —
        // a strong signal of `stages_extraction.py` rather than `conftest.py`.
        // Non-fatal: legitimate flat filenames like `my_module.py` pass.
        if let Some(filename) = p.file_name().and_then(|n| n.to_str()) {
            let has_parent = p.parent().map(|par| par != Path::new("")).unwrap_or(false);
            if !has_parent {
                let stem = Path::new(filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(filename);
                let underscore_count = stem.chars().filter(|&c| c == '_').count();
                if stem.len() > 20 && underscore_count >= 2 {
                    tracing::warn!(
                        path = path,
                        stem = stem,
                        underscore_count = underscore_count,
                        "assignments.json path '{}' looks like it may encode directory structure \
                         with underscores (e.g. 'stages_extraction.py' instead of \
                         'stages/extraction.py'). If this file belongs inside a subdirectory, \
                         update the plan-agent output to use forward-slash separators.",
                        path
                    );
                }
            }
        }

        Ok(())
    }

    /// Validate structural invariants of the wave decomposition (#114).
    ///
    /// Why: A malformed `assignments.json` can silently skip dependencies or
    /// allow a later wave to run before its prerequisites land. Catching all
    /// violations at load time makes plan-agent bugs loud and keeps the wave
    /// loop's invariants (sequential 1-indexed ordinals, no duplicate paths,
    /// `depends_on` references a path in a strictly earlier wave) explicit.
    /// What: Collects every violation into one multi-line error so operators
    /// can fix them in a single pass. Adopted from
    /// `out/self-review-1/patches/02_wave_ordinal_validation.rs`.
    /// Test: `validate_assignments_*` unit tests in this module.
    pub fn validate(&self) -> Result<()> {
        let (fatal, ordering) = self.collect_validation_errors();
        let mut errors = fatal;
        errors.extend(ordering);
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "assignments.json validation failed:\n  - {}",
                errors.join("\n  - ")
            )
        }
    }

    /// Collect validation errors, splitting fatal (un-repairable) from
    /// ordering (repairable) violations (#162).
    ///
    /// Why: The wave validator historically rejected *any* error — including
    /// same-wave dependencies that are trivially fixable by reshuffling files
    /// into a topologically valid ordering. Splitting violation classes lets
    /// `load()` attempt repair when only ordering is broken while still
    /// rejecting plans with duplicates, path traversal, bad ordinals, or
    /// true dependency cycles.
    /// What: Returns `(fatal_errors, ordering_errors)`. Fatal = empty waves,
    /// non-sequential ordinals, duplicate paths, unsafe paths, unknown deps.
    /// Ordering = depends_on references a path in the same wave or a later
    /// wave (linearizable via Kahn's algorithm unless it's a true cycle).
    /// Test: `wave_validator_repairs_same_wave_dep` exercises the split.
    fn collect_validation_errors(&self) -> (Vec<String>, Vec<String>) {
        let mut fatal: Vec<String> = Vec::new();
        let mut ordering: Vec<String> = Vec::new();

        if self.waves.is_empty() {
            fatal.push("assignments has no waves".into());
        }

        for (i, wave) in self.waves.iter().enumerate() {
            let expected = (i + 1) as u32;
            if wave.wave != expected {
                fatal.push(format!(
                    "wave position {}: expected ordinal {expected}, got {}",
                    i, wave.wave
                ));
            }
        }

        // Path traversal + duplicate detection.
        let mut seen_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut all_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for wave in &self.waves {
            for file in &wave.files {
                if let Err(e) = Self::validate_file_path(&file.path) {
                    fatal.push(format!("wave {}: {e}", wave.wave));
                }
                if !seen_paths.insert(&file.path) {
                    fatal.push(format!("duplicate file path: '{}'", file.path));
                }
                all_paths.insert(&file.path);
            }
        }

        // depends_on classification:
        // - unknown dep (not in any wave) → fatal (can't repair without inventing a file)
        // - same-wave or later-wave dep → ordering (repairable via topo sort)
        let mut earlier_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for wave in &self.waves {
            for file in &wave.files {
                for dep in &file.depends_on {
                    if !all_paths.contains(dep.as_str()) {
                        fatal.push(format!(
                            "file '{}' in wave {} depends on '{}' which is not declared in any wave",
                            file.path, wave.wave, dep
                        ));
                    } else if !earlier_paths.contains(dep.as_str()) {
                        ordering.push(format!(
                            "file '{}' in wave {} depends on '{}' which is not in an earlier wave",
                            file.path, wave.wave, dep
                        ));
                    }
                }
            }
            for file in &wave.files {
                earlier_paths.insert(&file.path);
            }
        }

        (fatal, ordering)
    }

    /// Best-effort loader that returns `None` when the file is absent OR when
    /// parsing fails.
    ///
    /// Why: Falling back silently to the legacy monolithic code phase means a
    /// partially written or malformed `assignments.json` never bricks a
    /// workflow run — the engine just behaves as it did before #88. HOWEVER,
    /// silent failures made it impossible to diagnose why the wave loop was
    /// skipped in live runs (#88 follow-up): a plan-agent that emits JSON
    /// missing one required field would silently return `None`, leaving
    /// operators guessing. We now log every branch (absent / read-fail /
    /// parse-fail / ok) so a `RUST_LOG=debug` run makes the decision obvious.
    /// What: Reads `out_dir/assignments.json`; returns `Some(Self)` on a
    /// clean parse, otherwise `None`. Every return path emits a log line.
    /// Test: All three `assignments_load_*` tests in this module plus
    /// `wave_loop_runs_one_agent_per_file` exercise the success path, and
    /// `assignments_load_returns_none_for_invalid_json` covers the parse-fail
    /// log path (log line is informational, not asserted).
    pub fn load(out_dir: &Path) -> Option<Self> {
        let path = out_dir.join("assignments.json");
        if !path.exists() {
            tracing::debug!(
                path = %path.display(),
                "assignments.json not found; falling back to monolithic code phase"
            );
            return None;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "assignments.json exists but read failed; falling back to monolithic code phase"
                );
                return None;
            }
        };
        match serde_json::from_str::<Self>(&content) {
            Ok(mut a) => {
                // #114/#162: Structural validation. Split errors into fatal
                // (duplicates, path traversal, bad ordinals, empty waves,
                // unknown deps) vs ordering (same-wave or forward deps).
                // Ordering-only failures are repairable via topological sort
                // (Kahn's algorithm) — don't fall back to the monolithic code
                // phase when we can trivially fix the layering.
                let (fatal, ordering) = a.collect_validation_errors();
                if !fatal.is_empty() {
                    tracing::error!(
                        path = %path.display(),
                        errors = %fatal.join("; "),
                        "assignments.json has fatal validation errors; refusing to run wave loop"
                    );
                    return None;
                }
                if !ordering.is_empty() {
                    let violation_count = ordering.len();
                    match a.repair_wave_ordering() {
                        Ok(true) => {
                            tracing::warn!(
                                path = %path.display(),
                                violations = violation_count,
                                "wave validator repaired {} same-wave dependency violations via topological sort",
                                violation_count
                            );
                        }
                        Ok(false) => {
                            tracing::error!(
                                path = %path.display(),
                                "wave validator reported ordering violations but repair made no changes; refusing to run wave loop"
                            );
                            return None;
                        }
                        Err(cycle_msg) => {
                            tracing::error!(
                                path = %path.display(),
                                error = %cycle_msg,
                                "assignments.json has true dependency cycles; refusing to run wave loop"
                            );
                            return None;
                        }
                    }
                    if let Err(e) = a.validate() {
                        tracing::error!(
                            path = %path.display(),
                            error = %e,
                            "assignments.json still invalid after repair; refusing to run wave loop"
                        );
                        return None;
                    }
                }
                tracing::info!(
                    path = %path.display(),
                    waves = a.waves.len(),
                    files = a.waves.iter().map(|w| w.files.len()).sum::<usize>(),
                    "assignments.json parsed; wave loop will run"
                );
                Some(a)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    bytes = content.len(),
                    "assignments.json parse failed; falling back to monolithic code phase"
                );
                None
            }
        }
    }
}
