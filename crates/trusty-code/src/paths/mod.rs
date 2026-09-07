//! Trusty Code's own configuration and state layout (#5426, epic #2892).
//!
//! Why: every discovery site in this crate hard-coded `.claude/` — the agents
//! directory, the skills directory, the plugin tree, `settings.json`, and the
//! project context file each joined that literal themselves. That made
//! Claude Code's directory the source of truth for a product that is not Claude
//! Code: a clean project could not install or run without a `.claude/` tree, and
//! nothing stopped Trusty Code writing INTO another product's directory. #5426
//! inverts the relationship. `<project>/.trusty-code/` is where Trusty Code
//! reads and — critically — the ONLY place it writes; `.claude/` (and the older
//! `.open-mpm/`) stay readable as compatibility INPUTS, never as write targets.
//!
//! What: one resolver, [`resolve_project_entry`], that every discovery site
//! calls instead of joining a literal. It walks [`SEARCH_ROOTS`] in order and
//! reports the winning source ([`ConfigSource`]) alongside the path, so
//! diagnostics can answer "which directory won, and what else was tried?"
//! without re-deriving the rule. [`native_config_dir`] and
//! [`check_native_write_target`] are the write half: a path that is not beneath
//! `<project>/.trusty-code/` — including one that reaches outside it through a
//! symlink — is refused rather than written. [`private_state`] owns the private
//! mutable half (`~/.trusty-code/`, mode `0700`), and [`import`] owns the
//! deterministic, non-overwriting legacy import from `.claude/`.
//!
//! **Precedence, and where it is documented.** The order is
//! `.trusty-code/` → `.claude/` → `.open-mpm/`, then a default that names
//! `.trusty-code/` even when nothing exists yet. The operator-facing statement
//! of that rule, including the fallback and warning behaviour, lives in
//! [the crate README](../../README.md#configuration-layout-trusty-code) — this
//! module is its implementation, not a second specification.
//!
//! Test: `paths::tests::*`, `paths::private_state_tests::*`,
//! `paths::import_tests::*`.
//!
//! [`private_state`]: crate::paths::private_state
//! [`import`]: crate::paths::import

use std::path::{Path, PathBuf};

pub mod import;
pub mod private_state;

/// Trusty Code's own project configuration directory name.
///
/// Why: named once so the convention cannot drift to `.trusty_code` /
/// `.trustycode` at a second call site.
/// What: `".trusty-code"` — both the project-config directory under a project
/// root and (via [`private_state`]) the private state directory under `$HOME`,
/// mirroring trusty-mpm's `.trusty-mpm` precedent.
/// Test: `paths::tests::native_config_dir_is_always_trusty_code`.
pub const TRUSTY_CODE_DIRNAME: &str = ".trusty-code";

/// Claude Code's project configuration directory name.
///
/// Why: a compatibility INPUT — read when Trusty Code's own directory has not
/// been created yet, never written to. See [`check_native_write_target`].
/// What: `".claude"`.
/// Test: `paths::tests::falls_back_to_claude_when_trusty_code_absent`.
pub const CLAUDE_COMPAT_DIRNAME: &str = ".claude";

/// The pre-Claude-Code open-mpm configuration directory name.
///
/// Why: `agents::locate_agents_dir` already honoured it; dropping it here would
/// silently break a project that still uses it.
/// What: `".open-mpm"`.
/// Test: `paths::tests::falls_back_to_open_mpm_when_neither_native_nor_claude`.
pub const OPEN_MPM_LEGACY_DIRNAME: &str = ".open-mpm";

/// The search order every project-scoped lookup walks, highest precedence first.
///
/// Why: one ordered table rather than a chain of `if exists` blocks repeated per
/// call site — the bug #5426 exists to remove.
/// What: `.trusty-code` (native) → `.claude` (compatibility) → `.open-mpm`
/// (legacy), each paired with the [`ConfigSource`] it resolves to.
/// Test: `paths::tests::precedence_prefers_trusty_code_over_claude`.
pub const SEARCH_ROOTS: &[(&str, ConfigSource)] = &[
    (TRUSTY_CODE_DIRNAME, ConfigSource::TrustyCode),
    (CLAUDE_COMPAT_DIRNAME, ConfigSource::ClaudeCompat),
    (OPEN_MPM_LEGACY_DIRNAME, ConfigSource::OpenMpmLegacy),
];

/// Which configuration root a resolved path came from.
///
/// Why: "the winning source" is a diagnostic every CLI and log line needs, and
/// deriving it back out of a `PathBuf` by string-matching would be a second,
/// drifting implementation of [`SEARCH_ROOTS`].
/// What: one variant per search root, plus [`ConfigSource::Default`] for the
/// case where nothing exists yet and the resolver names the native path anyway.
/// Test: `paths::tests::precedence_prefers_trusty_code_over_claude`,
/// `paths::tests::default_source_when_nothing_exists`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// `<project>/.trusty-code/…` — Trusty Code's own directory.
    TrustyCode,
    /// `<project>/.claude/…` — Claude Code compatibility input.
    ClaudeCompat,
    /// `<project>/.open-mpm/…` — pre-Claude-Code legacy input.
    OpenMpmLegacy,
    /// Nothing exists yet; the path names the native location regardless.
    Default,
}

impl ConfigSource {
    /// A stable lowercase token for logs, JSON diagnostics, and CLI output.
    ///
    /// Why: the CLI and the log field must agree, and a `Debug` rendering is not
    /// a wire contract.
    /// What: `"trusty-code"`, `"claude"`, `"open-mpm"`, `"default"`.
    /// Test: `paths::tests::source_tokens_are_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustyCode => "trusty-code",
            Self::ClaudeCompat => "claude",
            Self::OpenMpmLegacy => "open-mpm",
            Self::Default => "default",
        }
    }

    /// Whether this source is Trusty Code's own directory.
    ///
    /// Why: a caller deciding whether to suggest `tcode config import` needs the
    /// question answered once, here.
    /// What: `true` for [`ConfigSource::TrustyCode`] and [`ConfigSource::Default`]
    /// — both name a path beneath `.trusty-code/`.
    /// Test: `paths::tests::source_tokens_are_stable`.
    pub fn is_native(self) -> bool {
        matches!(self, Self::TrustyCode | Self::Default)
    }
}

/// What a project-scoped lookup resolved to, and what it had to skip to get there.
///
/// Why: a bare `PathBuf` cannot answer "did the native directory lose, and why?"
/// — the exact question the fail-open warning and the `tcode config paths`
/// diagnostic both need. Carrying `unreadable` as data (not only as a log line)
/// is what makes the fail-open branch assertable in a test.
/// What: the winning `path`, its [`ConfigSource`], and every candidate that
/// existed but could not be read (each already logged at `warn`).
/// Test: `paths::tests::unreadable_native_dir_falls_back_and_is_reported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// The resolved path. Exists unless `source` is [`ConfigSource::Default`].
    pub path: PathBuf,
    /// Which root won.
    pub source: ConfigSource,
    /// Candidates that existed but could not be read, in search order.
    pub unreadable: Vec<PathBuf>,
}

/// Whether a lookup targets a directory or a single file.
///
/// Why: "unreadable" means different syscalls for the two — a directory is
/// unreadable when `read_dir` fails, a file when `File::open` fails — and the
/// fail-open warning must not fire merely because a file candidate is a
/// directory or vice versa.
/// What: the two shapes [`resolve_project_entry`] accepts.
/// Test: `paths::tests::unreadable_native_dir_falls_back_and_is_reported`,
/// `paths::tests::unreadable_settings_file_falls_back_and_is_reported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory, e.g. `agents/`.
    Dir,
    /// A regular file, e.g. `settings.json`.
    File,
}

/// Resolve one project-scoped configuration entry across [`SEARCH_ROOTS`].
///
/// Why: the single entry point #5426 replaces every hard-coded `.claude/` join
/// with. Centralising it is what makes the precedence testable in one place and
/// makes "a clean project with only `.trusty-code/` works" true everywhere at
/// once rather than per call site.
/// What: walks [`SEARCH_ROOTS`] in order and returns the first candidate that
/// exists AND can be opened. A candidate that exists but cannot be opened is
/// SKIPPED, recorded in [`ResolvedConfig::unreadable`], and logged at `warn`
/// with the path tried — falling through to the next root rather than failing
/// the run, because a harness that refuses to start over an unreadable optional
/// config is worse than one that starts with less config and says so. When no
/// candidate is usable, the returned path is `<project_root>/.trusty-code/<rel>`
/// with source [`ConfigSource::Default`]: callers that need existence check for
/// it themselves, and every WRITE goes to the native path regardless.
/// Test: `paths::tests::precedence_prefers_trusty_code_over_claude`,
/// `paths::tests::falls_back_to_claude_when_trusty_code_absent`,
/// `paths::tests::default_source_when_nothing_exists`,
/// `paths::tests::unreadable_native_dir_falls_back_and_is_reported`.
pub fn resolve_project_entry(
    project_root: &Path,
    relative: &str,
    kind: EntryKind,
) -> ResolvedConfig {
    let mut unreadable = Vec::new();
    for (dirname, source) in SEARCH_ROOTS {
        let candidate = project_root.join(dirname).join(relative);
        match probe(&candidate, kind) {
            Probe::Missing => continue,
            Probe::Usable => {
                return ResolvedConfig {
                    path: candidate,
                    source: *source,
                    unreadable,
                };
            }
            Probe::Unreadable(err) => {
                // #5426: fail open — an unreadable candidate must never abort the
                // run, but it must never be silent either.
                tracing::warn!(
                    path = %candidate.display(),
                    source = source.as_str(),
                    error = %err,
                    "trusty-code config path exists but could not be read; \
                     skipping it and falling back to the next configuration root"
                );
                unreadable.push(candidate);
            }
        }
    }
    ResolvedConfig {
        path: native_child(project_root, relative),
        source: ConfigSource::Default,
        unreadable,
    }
}

/// The outcome of testing one candidate path.
enum Probe {
    /// Nothing of the requested kind is there.
    Missing,
    /// Present and openable.
    Usable,
    /// Present but the open failed (permissions, a broken mount, a race).
    Unreadable(std::io::Error),
}

/// Test one candidate path for existence and readability.
///
/// Why: `Path::exists` alone cannot distinguish "absent" from "present but
/// unreadable", and only the second deserves a warning.
/// What: for [`EntryKind::Dir`], a non-directory is `Missing` and a `read_dir`
/// failure is `Unreadable`; for [`EntryKind::File`], a non-file is `Missing` and
/// a `File::open` failure is `Unreadable`.
/// Test: `paths::tests::unreadable_native_dir_falls_back_and_is_reported`,
/// `paths::tests::unreadable_settings_file_falls_back_and_is_reported`.
fn probe(candidate: &Path, kind: EntryKind) -> Probe {
    match kind {
        EntryKind::Dir => {
            if !candidate.is_dir() {
                return Probe::Missing;
            }
            match std::fs::read_dir(candidate) {
                Ok(_) => Probe::Usable,
                Err(e) => Probe::Unreadable(e),
            }
        }
        EntryKind::File => {
            if !candidate.is_file() {
                return Probe::Missing;
            }
            match std::fs::File::open(candidate) {
                Ok(_) => Probe::Usable,
                Err(e) => Probe::Unreadable(e),
            }
        }
    }
}

/// The agents directory for a project root.
///
/// Why: `agents::locate_agents_dir`'s two-way `.claude`/`.open-mpm` check
/// predates `.trusty-code/` and is now one instance of the general rule.
/// What: [`resolve_project_entry`] for `agents`, as a directory.
/// Test: `paths::tests::agents_dir_prefers_trusty_code`.
pub fn agents_dir(project_root: &Path) -> ResolvedConfig {
    resolve_project_entry(project_root, "agents", EntryKind::Dir)
}

/// The skills directory for a project root.
///
/// Why: see [`agents_dir`] — `skills::locate_skills_dir` was pinned to
/// `.claude/skills` exactly, which is precisely what #5426 unpins.
/// What: [`resolve_project_entry`] for `skills`, as a directory.
/// Test: `paths::tests::skills_dir_prefers_trusty_code`.
pub fn skills_dir(project_root: &Path) -> ResolvedConfig {
    resolve_project_entry(project_root, "skills", EntryKind::Dir)
}

/// The plugins directory for a project root.
///
/// Why: `plugins::discover_plugin_roots` joined `.claude/plugins` directly.
/// What: [`resolve_project_entry`] for `plugins`, as a directory.
/// Test: `paths::tests::plugins_dir_prefers_trusty_code`.
pub fn plugins_dir(project_root: &Path) -> ResolvedConfig {
    resolve_project_entry(project_root, "plugins", EntryKind::Dir)
}

/// The `settings.json` file for a project root.
///
/// Why: `mode::read_settings_json_mode` read `.claude/settings.json` directly,
/// so a project with only `.trusty-code/` could not set `code_harness.mode`.
/// What: [`resolve_project_entry`] for `settings.json`, as a file.
/// Test: `paths::tests::settings_file_prefers_trusty_code`.
pub fn settings_file(project_root: &Path) -> ResolvedConfig {
    resolve_project_entry(project_root, SETTINGS_FILENAME, EntryKind::File)
}

/// The project-settings filename, shared by both configuration roots.
///
/// Why: `.trusty-code/settings.json` deliberately keeps Claude Code's filename
/// and schema so an imported file needs no rewriting — the directory moves, the
/// format does not.
/// What: `"settings.json"`.
/// Test: `paths::tests::settings_file_prefers_trusty_code`.
pub const SETTINGS_FILENAME: &str = "settings.json";

/// Trusty Code's own project configuration directory — the ONLY write target.
///
/// Why: [`resolve_project_entry`] may legitimately RESOLVE into `.claude/`, but
/// a write there would make Trusty Code mutate another product's directory. The
/// read path and the write path are therefore deliberately different functions.
/// What: `<project_root>/.trusty-code`, unconditionally — never probed, never
/// falling back.
/// Test: `paths::tests::native_config_dir_is_always_trusty_code`.
pub fn native_config_dir(project_root: &Path) -> PathBuf {
    project_root.join(TRUSTY_CODE_DIRNAME)
}

/// A path beneath [`native_config_dir`].
///
/// Why: the write-side counterpart of [`resolve_project_entry`]'s `relative`.
/// What: `<project_root>/.trusty-code/<relative>`.
/// Test: `paths::tests::native_config_dir_is_always_trusty_code`.
pub fn native_child(project_root: &Path, relative: &str) -> PathBuf {
    native_config_dir(project_root).join(relative)
}

/// Why a candidate write target was refused.
///
/// Why: the two refusals are different failures with different fixes — one means
/// "you aimed at the wrong product's directory", the other "something on this
/// path is a symlink out of the tree" — and collapsing them into one string
/// would hide which.
/// What: [`WriteTargetError::CrossProduct`] for a path outside
/// `<project>/.trusty-code/`; [`WriteTargetError::SymlinkEscape`] for a path
/// whose nearest existing ancestor resolves outside it.
/// Test: `paths::tests::write_to_claude_dir_is_refused`,
/// `paths::tests::symlinked_native_subdir_escape_is_refused`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WriteTargetError {
    /// The path is not beneath the project's own `.trusty-code/` directory.
    #[error(
        "refusing to write to {path}: trusty-code writes only beneath {native_root} \
         (another product's configuration directory is a read-only compatibility input)"
    )]
    CrossProduct {
        /// The refused path.
        path: PathBuf,
        /// The only permitted write root.
        native_root: PathBuf,
    },
    /// A symlink on the path resolves outside `.trusty-code/`.
    #[error("refusing to write to {path}: it resolves to {resolved}, outside {native_root}")]
    SymlinkEscape {
        /// The refused path, as supplied.
        path: PathBuf,
        /// Where its nearest existing ancestor actually points.
        resolved: PathBuf,
        /// The only permitted write root.
        native_root: PathBuf,
    },
}

/// Refuse any write target that is not genuinely beneath `<project>/.trusty-code/`.
///
/// Why: "native project writes stay beneath `<project>/.trusty-code/`" is only a
/// property if something enforces it, and a lexical `starts_with` does not: a
/// symlink at `.trusty-code/agents -> ../../elsewhere` satisfies it while every
/// write lands outside the tree.
///
/// Resolving the target alone is not enough either. The first version of this
/// guard resolved the target AND the config root the same way and compared them
/// to each other, so a `.trusty-code` that was ITSELF a symlink out of the
/// project moved the goalposts with the target and approved every write
/// (code-critic BLOCK, PR #6980). The root is therefore anchored to the project
/// FIRST, and only then does the target get compared against it.
///
/// What: three checks, in order.
/// 1. Lexical — `path` must start with [`native_config_dir`], else
///    [`WriteTargetError::CrossProduct`].
/// 2. Root anchoring — when `<project>/.trusty-code` exists, it must
///    canonicalise to somewhere inside the canonicalised `project_root`, else
///    [`WriteTargetError::SymlinkEscape`]. A `project_root` that cannot itself be
///    canonicalised fails CLOSED here: containment cannot be established, so the
///    write is refused rather than assumed safe.
/// 3. Target containment — the target's nearest EXISTING ancestor (the target
///    itself is usually absent, since we are about to create it) must resolve
///    inside that same anchored root.
///
/// With nothing on the path existing yet, no symlink can be redirecting it and
/// check 1 is the whole answer.
/// Test: `paths::tests::write_to_native_dir_is_allowed`,
/// `paths::tests::write_to_claude_dir_is_refused`,
/// `paths::tests::write_outside_project_is_refused`,
/// `paths::tests::symlinked_native_subdir_escape_is_refused`,
/// `paths::tests::symlinked_native_root_escape_is_refused`.
pub fn check_native_write_target(
    project_root: &Path,
    path: &Path,
) -> Result<PathBuf, WriteTargetError> {
    let native_root = native_config_dir(project_root);
    if !path.starts_with(&native_root) {
        return Err(WriteTargetError::CrossProduct {
            path: path.to_path_buf(),
            native_root,
        });
    }

    let escape = |resolved: PathBuf| WriteTargetError::SymlinkEscape {
        path: path.to_path_buf(),
        resolved,
        native_root: native_root.clone(),
    };

    // #5426: anchor the config root to the project before trusting it. Without
    // this, a symlinked `.trusty-code` makes every later comparison vacuous.
    if native_root.exists() {
        let canon_root = native_root
            .canonicalize()
            .map_err(|_| escape(native_root.clone()))?;
        let canon_project = project_root
            .canonicalize()
            .map_err(|_| escape(canon_root.clone()))?;
        if !canon_root.starts_with(&canon_project) {
            return Err(escape(canon_root));
        }
    }

    // The target itself is typically absent, so containment is judged on the
    // deepest ancestor that DOES exist.
    let Some(canon_root) = canonicalize_existing(&native_root) else {
        // Nothing on this path exists yet, so no symlink can be escaping through
        // it; the lexical check above is the whole answer.
        return Ok(path.to_path_buf());
    };
    match canonicalize_existing(path) {
        Some(resolved) if !resolved.starts_with(&canon_root) => Err(escape(resolved)),
        _ => Ok(path.to_path_buf()),
    }
}

/// Canonicalise the deepest existing ancestor of `path` (including `path`).
///
/// Why: `Path::canonicalize` fails outright on a path that does not exist yet,
/// which is the normal case for a write target. Resolving the nearest existing
/// ancestor still catches every symlink ON the path, which is what the guard
/// needs.
/// What: walks up from `path` until `canonicalize` succeeds; `None` when even
/// the root fails (a path with no existing ancestor at all).
/// Test: `paths::tests::symlinked_native_subdir_escape_is_refused`.
fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(p) = current {
        if let Ok(canon) = p.canonicalize() {
            return Some(canon);
        }
        current = p.parent();
    }
    None
}

/// Normalised key words whose presence marks a value as secret-bearing.
///
/// Why: project configuration under `.trusty-code/` is committed to the
/// project's repository by convention, so a credential landing there is a leak
/// with a long tail. The import path checks against this list before copying a
/// settings file rather than after.
/// What: lowercase, separator-free, SINGULAR word forms — not substrings. A key
/// is split into words first ([`key_segments`]) and each word is de-pluralised
/// before matching, so one entry covers every spelling of it: `api_key`,
/// `API-KEY`, `x-api-key`, `xApiKey`, `APIKEY` and `apiKeys` all reduce to the
/// `apikey` form. Multi-word entries (`apikey`, `accesskey`, `privatekey`) are
/// matched against adjacent word PAIRS as well as against a single unseparated
/// word.
/// Test: `paths::tests::secret_keys_match_across_separator_and_case_spellings`,
/// `paths::tests::secret_keys_match_plural_spellings`,
/// `paths::tests::benign_keys_containing_a_hint_are_not_flagged`.
pub const SECRET_KEY_HINTS: &[&str] = &[
    "accesskey",
    "apikey",
    "authorization",
    "credential",
    "passphrase",
    "password",
    "privatekey",
    "secret",
    "token",
];

/// Whole keys that name a COUNT rather than a credential.
///
/// Why: de-pluralising makes `tokens` a hit, and `max_tokens` is an LLM sampling
/// parameter this crate's own `code_harness` settings carry — refusing to import
/// the very file the feature exists to migrate is a worse outcome than the leak
/// risk of a key that holds an integer (code-critic round 2, PR #6980). The
/// exemption is deliberately a short, closed list rather than a heuristic.
/// What: normalised whole-key forms. The comparison is against the ENTIRE key,
/// not a window of it, so appending a credential to an exempt name
/// (`max_tokens_api_key`) does not inherit the exemption.
/// Test: `paths::tests::max_tokens_stays_benign_as_a_count_not_a_credential`.
pub const SECRET_KEY_EXEMPTIONS: &[&str] = &[
    "maxtokens",
    "mintokens",
    "numtokens",
    "tokenbudget",
    "tokencount",
    "tokenlimit",
    "tokenusage",
    "totaltokens",
];

/// Split a key into lowercase word segments.
///
/// Why: matching raw substrings both missed spellings and invented matches —
/// `x-api-key` contained no hint while `tokenizer` contained `token`
/// (code-critic HIGH, PR #6980). Splitting into words first makes the rule
/// spelling-insensitive and word-exact at the same time.
/// What: breaks on any non-alphanumeric character and on a lower-to-upper
/// transition (so `xApiKey` yields three words), lowercasing each. An
/// all-uppercase run stays one word, which is why the unseparated forms
/// (`apikey`) are listed in [`SECRET_KEY_HINTS`] too.
/// Test: `paths::tests::secret_keys_match_across_separator_and_case_spellings`,
/// `paths::tests::benign_keys_containing_a_hint_are_not_flagged`.
fn key_segments(key: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut prev_was_lower = false;
    for ch in key.chars() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            prev_was_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && prev_was_lower && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        prev_was_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        current.push(ch.to_ascii_lowercase());
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Whether one JSON key names a credential.
///
/// Why: the single rule both the import refusal and its tests apply, so the
/// spelling coverage cannot drift between them.
/// What: `false` outright when the whole key is a [`SECRET_KEY_EXEMPTIONS`]
/// entry. Otherwise `true` when any single word of the key, or any two ADJACENT
/// words joined, matches [`SECRET_KEY_HINTS`] under [`matches_hint`]'s
/// de-pluralising comparison. Word-exact, so `tokenizer` and `secretary` are
/// clean while `token`, `tokens` and `x-api-key` are not.
/// Test: `paths::tests::secret_keys_match_across_separator_and_case_spellings`,
/// `paths::tests::secret_keys_match_plural_spellings`,
/// `paths::tests::max_tokens_stays_benign_as_a_count_not_a_credential`,
/// `paths::tests::benign_keys_containing_a_hint_are_not_flagged`.
pub fn is_secret_key(key: &str) -> bool {
    let segments = key_segments(key);
    // #5426: the exemption matches the WHOLE key, never a window of it, so a
    // credential appended to an exempt name still trips the check below.
    if SECRET_KEY_EXEMPTIONS.contains(&segments.concat().as_str()) {
        return false;
    }
    (1..=2).any(|width| {
        segments
            .windows(width)
            .any(|window| matches_hint(&window.concat()))
    })
}

/// Whether one normalised word (or word pair) is a hint, singular or plural.
///
/// Why: the hint list carried `credential` and `credentials` but left `secret`,
/// `token`, `apikey`, `accesskey` and `privatekey` singular-only, so `tokens`,
/// `secretsFile`, `apiKeys` and four more plural spellings walked past the
/// matcher (code-critic round 2 HIGH, PR #6980). De-pluralising at the
/// comparison keeps the list singular and closes every plural at once, instead
/// of doubling the list and inviting the same omission again.
/// What: matches `joined` against [`SECRET_KEY_HINTS`] as given, then again with
/// one trailing `s` removed. Stripping only a trailing `s` is deliberately
/// narrow: `passwordless` becomes `passwordles`, not `password`, and
/// `secretary` has no trailing `s` to strip at all.
/// Test: `paths::tests::secret_keys_match_plural_spellings`,
/// `paths::tests::benign_keys_containing_a_hint_are_not_flagged`.
fn matches_hint(joined: &str) -> bool {
    if SECRET_KEY_HINTS.contains(&joined) {
        return true;
    }
    joined
        .strip_suffix('s')
        .is_some_and(|singular| SECRET_KEY_HINTS.contains(&singular))
}

/// Find the first secret-bearing key path in a JSON value, if any.
///
/// Why: a boolean would tell an operator that their import was refused without
/// telling them which key to remove.
/// What: a depth-first walk returning the dotted key path of the first key
/// [`is_secret_key`] accepts; `None` when clean. Array elements are indexed
/// (`mcp.0.token`). Keys only — a bare credential under an unrelated key is out
/// of scope, as the README states.
/// Test: `paths::tests::secret_bearing_keys_are_detected`,
/// `paths::tests::nested_kebab_secret_is_reported_by_path`,
/// `paths::tests::ordinary_settings_carry_no_secret`.
pub fn find_secret_key(value: &serde_json::Value) -> Option<String> {
    fn walk(value: &serde_json::Value, prefix: &str, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    if is_secret_key(key) {
                        *out = Some(path);
                        return;
                    }
                    walk(child, &path, out);
                    if out.is_some() {
                        return;
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{prefix}.{i}"), out);
                    if out.is_some() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = None;
    walk(value, "", &mut out);
    out
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
