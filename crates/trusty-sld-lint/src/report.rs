//! Diagnostics emitted by the SLD linter.
//!
//! Why: the linter must report each violation with an actionable `file:line`
//! location and a stable check name (so CI logs and the allowlist can key on
//! it). Modelling that as one value type keeps the check engine's output
//! uniform and easy to sort, filter, and count.
//! What: [`Severity`] (error vs advisory), [`Diagnostic`] (a single finding with
//! path, line, check id, and message), and its `Display` as a
//! `path:line: [check] message` line.
//! Test: `super::tests::report_display`.

use std::fmt;

/// Whether a finding fails the lint (error) or is advisory (warning).
///
/// Why: DOC-38 makes some conditions hard violations (an unresolved reference)
/// and others explicitly non-blocking (revision drift is flagged, not enforced —
/// §1.3, §4.4). The severity lets the CLI exit non-zero only on real errors while
/// still surfacing advisories.
/// What: `Error` counts toward a non-zero exit; `Warning` is informational.
/// Test: `super::tests::report_exit_counts_errors_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A hard violation — fails the lint (non-zero exit).
    Error,
    /// An advisory — surfaced but does not fail the lint.
    Warning,
}

/// A single SLD finding at a specific `file:line`.
///
/// Why: an actionable diagnostic names exactly where and what — the file, the
/// line, the check that fired, and a human-readable reason — so an author can
/// fix it without hunting; the stable `check` id is also the allowlist key.
/// What: the repo-relative `path`, 1-based `line` (0 when file-scoped, not
/// line-scoped), the `severity`, the `check` identifier, and the `message`.
/// Test: `super::tests::report_display`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Repo-relative path of the offending file.
    pub path: String,
    /// 1-based line, or 0 when the finding is file-scoped.
    pub line: usize,
    /// Error vs advisory.
    pub severity: Severity,
    /// Stable check identifier (e.g. `ref-unresolved`, `anchor-mismatch`).
    pub check: &'static str,
    /// Human-readable description of the violation.
    pub message: String,
}

impl Diagnostic {
    /// Construct an `Error`-severity diagnostic.
    ///
    /// Why: most findings are hard errors; a named constructor keeps call sites
    /// terse and consistent.
    /// What: builds a [`Diagnostic`] with [`Severity::Error`].
    /// Test: `super::tests::report_display`.
    pub fn error(
        path: impl Into<String>,
        line: usize,
        check: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            line,
            severity: Severity::Error,
            check,
            message: message.into(),
        }
    }

    /// Construct a `Warning`-severity (advisory) diagnostic.
    ///
    /// Why: some DOC-38 conditions (revision drift) are surfaced but must not
    /// fail the lint (§1.3); this constructor marks them advisory.
    /// What: builds a [`Diagnostic`] with [`Severity::Warning`].
    /// Test: `super::tests::report_exit_counts_errors_only`.
    pub fn warning(
        path: impl Into<String>,
        line: usize,
        check: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(path, line, check, message)
        }
    }
}

impl fmt::Display for Diagnostic {
    /// Render as `path:line: [severity check] message` (line omitted when 0).
    ///
    /// Why: a compact, greppable, editor-jump-friendly line is the standard
    /// linter output shape.
    /// What: prints the path, the line when non-zero, the severity+check tag,
    /// and the message.
    /// Test: `super::tests::report_display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        if self.line == 0 {
            write!(f, "{}: [{sev} {}] {}", self.path, self.check, self.message)
        } else {
            write!(
                f,
                "{}:{}: [{sev} {}] {}",
                self.path, self.line, self.check, self.message
            )
        }
    }
}
