//! Glob matching, subject extraction, and precedence for a compiled
//! `PermissionMap` (#7948).
//!
//! Why: a permission map is written as an unordered YAML table, so "which rule
//! applies" cannot be "the first one that matches" — an operator writing
//! `bash: { "git status*": allow, "*": ask }` expects the narrow rule to win
//! regardless of key order. This module owns the total order that makes that
//! mechanical.
//! What: `PermissionMap::evaluate` (one subject), `PermissionMap::evaluate_call`
//! (every subject of one call), and `subjects_for`. Precedence, highest first:
//!   1. an argument-level rule beats a tool-level rule;
//!   2. at the same level, the longest LITERAL PREFIX wins;
//!   3. at equal specificity, `deny` > `ask` > `allow`.
//!
//! A call that matches no rule returns `None`, which every caller reads as
//! `allow` — the pre-#7948 behaviour.
//!
//! A `bash` rule matches the command TEXT, never the program that ends up
//! running: `sh -c 'rm x'`, `/bin/rm x`, and `true; rm x` all miss a `rm *`
//! deny. Treat a bash rule as a guard against an accident, not against an
//! adversary — an agent that wants to get around it can.
//! Test: `permissions::tests::matcher_tests` — the whole module.

use std::path::Path;

use serde_json::Value;

use super::config::{PermissionMap, RuleBody, RuleDecision};

/// The rule that decided one call, and what it decided.
///
/// What: `rule` is the pattern as written, in `tool` form for a tool-level
/// rule and `tool[arg]` form for an argument-level one.
/// Test: `arg_level_rule_beats_tool_level_rule`, `rule_text_names_the_arg_pattern`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatch {
    pub decision: RuleDecision,
    pub rule: String,
}

/// `(level, literal-prefix, decision rank)` — every field ordered "higher wins".
type Specificity = (u8, usize, u8);

impl PermissionMap {
    /// Decide `tool` with at most one `subject`, or `None` when no rule matches.
    ///
    /// Why: the single precedence implementation; `evaluate_call` and every
    /// test go through it.
    /// What: collects tool-level candidates and argument-level candidates whose
    /// subject glob matches, and keeps the maximum `Specificity`. An `Args`
    /// body contributes nothing when `subject` is `None` or no glob matches.
    /// Test: `arg_level_rule_beats_tool_level_rule`,
    /// `longest_literal_prefix_wins_among_arg_rules`,
    /// `deny_beats_ask_beats_allow_at_equal_specificity`,
    /// `unmatched_tool_has_no_rule`,
    /// `arg_table_with_no_matching_subject_contributes_nothing`.
    pub fn evaluate(&self, tool: &str, subject: Option<&str>) -> Option<RuleMatch> {
        let mut best: Option<(Specificity, RuleMatch)> = None;
        for rule in self.rules.iter().filter(|r| r.glob.is_match(tool)) {
            match &rule.body {
                RuleBody::Tool(decision) => consider(
                    &mut best,
                    (0, rule.glob.literal_prefix_len(), decision.rank()),
                    *decision,
                    rule.glob.pattern().to_string(),
                ),
                RuleBody::Args(args) => {
                    let Some(subject) = subject else { continue };
                    for arg in args.iter().filter(|a| a.glob.is_match(subject)) {
                        consider(
                            &mut best,
                            (1, arg.glob.literal_prefix_len(), arg.decision.rank()),
                            arg.decision,
                            format!("{}[{}]", rule.glob.pattern(), arg.glob.pattern()),
                        );
                    }
                }
            }
        }
        best.map(|(_, m)| m)
    }

    /// Decide one call that may carry several subjects.
    ///
    /// Why (#7948): `write_files` writes many paths in one call. Evaluating
    /// only one of them would let a `write_files: { "src/**": allow, "*": deny }`
    /// map pass a denied path hidden behind an allowed one.
    /// What: no subjects evaluates once with `None`. Otherwise each subject is
    /// evaluated on its own and the SAFEST result wins (`deny` > `ask` >
    /// `allow`); a subject no rule matches counts as `allow`.
    /// Test: `write_files_is_decided_by_its_most_restrictive_path`.
    pub fn evaluate_call(&self, tool: &str, subjects: &[String]) -> Option<RuleMatch> {
        if subjects.is_empty() {
            return self.evaluate(tool, None);
        }
        subjects
            .iter()
            .filter_map(|s| self.evaluate(tool, Some(s)))
            .max_by_key(|m| m.decision.rank())
    }
}

/// Keep the candidate when it is strictly more specific than `best`.
///
/// Why: strictly-greater keeps the result independent of iteration order — two
/// candidates tying on all three axes carry the same decision.
fn consider(
    best: &mut Option<(Specificity, RuleMatch)>,
    spec: Specificity,
    decision: RuleDecision,
    rule: String,
) {
    if best.as_ref().is_none_or(|(current, _)| spec > *current) {
        *best = Some((spec, RuleMatch { decision, rule }));
    }
}

/// The argument(s) a tool call is discriminated by.
///
/// Why: a map says "bash may run `git status*`", not "bash may receive this
/// JSON object", so rules match against the string that names what the tool
/// acts on. Probing by argument NAME rather than a tool-name table keeps a
/// renamed or new file tool covered instead of silently dropping it to
/// tool-level matching.
/// What: MCP tools (`mcp__<server>__<tool>`) have no subject — their argument
/// shape belongs to a third-party server. A `files` array (`write_files`)
/// yields every entry's string `path`. Otherwise the first string among
/// `command` (bash), `path` (file tools), and `pattern` (glob/grep).
///
/// Every PATH subject (a `files[].path` or a `path`) yields its raw form and,
/// when different, its [`normalise_path_subject`] form. `evaluate_call` takes
/// the most restrictive decision across subjects, so `./secrets/x` meets the
/// same `secrets/**` deny as `secrets/x`, and a rewrite never loosens the raw
/// form's decision.
/// Test: `subject_for_bash_is_the_command`, `subject_for_read_file_is_the_path`,
/// `subject_for_grep_is_the_pattern`, `mcp_tools_have_no_subject`,
/// `subjects_for_write_files_are_every_path`,
/// `subject_is_none_when_no_known_argument_is_present`,
/// `path_deny_holds_for_every_lexical_spelling`.
pub fn subjects_for(tool: &str, args: &Value) -> Vec<String> {
    subjects_for_in_root(tool, args, None)
}

/// [`subjects_for`] with the run's working root supplied.
///
/// Why (#7948): `tools::fs::scoped_path` accepts an ABSOLUTE path that resolves
/// inside the working root (`scoped_path_accepts_absolute_inside_dir`), so the
/// same file has two spellings the model may pick between. An operator writes a
/// path rule relative — `secrets/**` — and a purely lexical subject keeps
/// `/<root>/secrets/token` absolute, so the deny never matched it.
/// What: on top of every form [`subjects_for`] yields, a path subject that
/// normalises to an absolute path UNDER `root` also yields its root-relative
/// spelling. `evaluate_call` folds every subject to the safest decision, so the
/// extra form can only add restriction. `root` of `None` is exactly
/// [`subjects_for`].
/// Test: `absolute_in_root_path_also_yields_its_root_relative_form`,
/// `path_deny_holds_for_the_absolute_in_root_spelling`,
/// `absolute_path_outside_the_root_yields_no_relative_form`.
pub fn subjects_for_in_root(tool: &str, args: &Value, root: Option<&Path>) -> Vec<String> {
    if tool.starts_with("mcp__") {
        return Vec::new();
    }
    let mut subjects = Vec::new();
    if let Some(files) = args.get("files").and_then(Value::as_array) {
        for path in files
            .iter()
            .filter_map(|f| f.get("path").and_then(Value::as_str))
        {
            push_path_forms(&mut subjects, path, root);
        }
        return subjects;
    }
    if let Some(command) = args.get("command").and_then(Value::as_str) {
        subjects.push(command.to_string());
    } else if let Some(path) = args.get("path").and_then(Value::as_str) {
        push_path_forms(&mut subjects, path, root);
    } else if let Some(pattern) = args.get("pattern").and_then(Value::as_str) {
        subjects.push(pattern.to_string());
    }
    subjects
}

/// Push `raw`, its normalised form when that differs, and its root-relative
/// form when it names a file under `root`.
// #7948: every form is decided, so a rewrite can only add restriction.
fn push_path_forms(subjects: &mut Vec<String>, raw: &str, root: Option<&Path>) {
    subjects.push(raw.to_string());
    let normalised = normalise_path_subject(raw);
    if normalised != raw {
        subjects.push(normalised.clone());
    }
    if let Some(relative) = root.and_then(|root| root_relative_subject(&normalised, root)) {
        subjects.push(relative);
    }
}

/// The root-relative spelling of an absolute path subject under `root`, or
/// `None` when either side is relative or the subject is outside the root.
///
/// Why (#7948): see [`subjects_for_in_root`].
/// What: pure string work, comparing the [`normalise_path_subject`] form of
/// both sides at a path-SEGMENT boundary — `/a/bc/x` is not under `/a/b`. The
/// root itself yields `None`; a rule names a file, not the tree's own entry.
///
/// Known limit (#7948): lexical only, matching this module's other path work —
/// a root reached through a symlink (macOS `/var` → `/private/var`) is not
/// recognised in its other spelling. `scoped_path` still confines the call.
/// Test: `absolute_in_root_path_also_yields_its_root_relative_form`,
/// `absolute_path_outside_the_root_yields_no_relative_form`.
fn root_relative_subject(normalised: &str, root: &Path) -> Option<String> {
    if !normalised.starts_with('/') {
        return None;
    }
    let root = normalise_path_subject(root.to_string_lossy().as_ref());
    if !root.starts_with('/') {
        return None;
    }
    let relative = normalised
        .strip_prefix(root.trim_end_matches('/'))?
        .strip_prefix('/')?;
    (!relative.is_empty()).then(|| relative.to_string())
}

/// Lexically normalise a path subject: drop `.` segments and empty segments
/// from repeated `/`, fold `name/..`, and strip a trailing `/`.
///
/// Why (#7948): rules glob-match the path the model wrote, while
/// `tools::fs::scoped_path` resolves it only later. Without this,
/// `secrets/**: deny` blocks `secrets/x` but not `./secrets/x`,
/// `secrets/./x`, `a/../secrets/x`, or `secrets//x`.
/// What: pure string work with no filesystem I/O. A relative `..` with nothing
/// left to fold is KEPT (`a/../../x` becomes `../x`), so a path that climbs
/// above the root never normalises into an in-tree path; an absolute path's
/// `..` at `/` stays at `/`. An empty relative result is `.`. The caller still
/// decides the raw form too, so normalisation never widens a decision.
///
/// Known limits (#7948): no case-folding, so `Secrets/x` misses a `secrets/**`
/// rule on a case-insensitive filesystem; no symlink resolution, so a symlink
/// into a denied tree, or a `link/..` fold that differs from the physical
/// path, is not caught here. `scoped_path` still confines the call to the tree.
/// Test: `normalise_path_subject_folds_lexical_spellings`,
/// `path_deny_holds_for_every_lexical_spelling`,
/// `path_climbing_above_the_root_is_never_more_permissive`.
pub fn normalise_path_subject(raw: &str) -> String {
    let absolute = raw.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if absolute => {}
                _ => parts.push(".."),
            },
            name => parts.push(name),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}
