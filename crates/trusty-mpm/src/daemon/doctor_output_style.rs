//! `tm doctor` output-style verification probe (DOC-28 R4(a)).
//!
//! Why: the self-awareness incident's root failure (F4) was a stale global
//! `outputStyle` value (`"claude_mpm"`) that resolves to no bundled style
//! file, so Claude Code silently fell back to its plain default system
//! prompt — and nothing in the framework detected or reported the gap. This
//! probe closes that gap deterministically by inspecting the *configuration*
//! (the effective `outputStyle` string and whether a matching file exists on
//! disk) rather than asking the model, which is the only way to catch total
//! instruction-load omission (see `docs/specs/trusty-mpm-self-awareness.md`
//! §6, §9).
//! What: [`check_output_style`] resolves Claude Code's EFFECTIVE `outputStyle`
//! by walking the FULL settings precedence chain Claude Code itself applies —
//! `project-local` (`<project>/.claude/settings.local.json`) > `project`
//! (`<project>/.claude/settings.json`) > `user-local`
//! (`<home>/.claude/settings.local.json`) > `user`
//! (`<home>/.claude/settings.json`) — stopping at the first layer that sets
//! the key (issue #1863: a settings file that exists but is silent on
//! `outputStyle` must NOT shadow a value in a lower-precedence layer, since a
//! settings file only overrides keys it actually sets). The scope-ordering
//! (project fully outranks user) is empirically confirmed against a real
//! Claude Code build in `docs/specs/standalone-managed-trusty-mpm.md:707`
//! ("Local > Project > User"); Claude Code does not itself document a
//! user-level `.local` tier, but issue #3453 extends the same
//! local-overrides-plain rule to the user scope for consistency, since
//! trusty-mpm already models `user_local_settings` as a first-class layer
//! (`ClaudeConfigReader::paths_for_project`). The resolved value is validated
//! against [`OUTPUT_STYLES`] and the deployed style files under
//! `<home>/.claude/output-styles/`; the report names which scope the resolved
//! value came from (see [`resolution_layers`]).
//! Test: `output_style_ok_when_style_resolves`,
//! `output_style_fail_when_id_unknown`, `output_style_warn_when_key_absent`,
//! `output_style_fail_when_file_missing`, `output_style_fail_on_malformed_json`,
//! `output_style_prefers_project_over_global`,
//! `output_style_falls_back_to_global_when_project_silent`,
//! `output_style_falls_back_to_global_when_project_missing`,
//! `output_style_project_local_overrides_project`,
//! `output_style_user_local_overrides_user`,
//! `output_style_project_local_wins_even_when_project_settings_correct`.
//!
//! [`check_output_style_staleness`] closes a second gap (issue #2333): even
//! when [`check_output_style`] reports `Ok` (the configured id resolves to a
//! non-empty file), that file's CONTENT can still have drifted from the
//! bundled asset — exactly what happened after PR #2328 corrected the PM
//! identity string but the deployed `~/.claude/output-styles/*.md` copies
//! were never refreshed. It adopts the same Warn-on-drift diagnostic pattern
//! `skill_staleness` (issue #2876) already uses for skills, but compares
//! deployed bytes directly rather than via a deploy-manifest checksum — output
//! styles have no per-deploy manifest, and a direct byte read also catches
//! post-deploy manual edits/corruption that a manifest-checksum comparison
//! would not see. Plus an orphan-file scan for foreign/dormant files sitting
//! in the same directory (e.g. a pre-rebrand `claude-mpm.md`).
//! Test: `staleness_ok_when_in_sync`, `staleness_warns_on_drift`,
//! `staleness_warns_on_orphan`, `staleness_ok_when_dir_missing`,
//! `staleness_ok_when_file_never_deployed`,
//! `staleness_orphan_exempts_configured_custom_id`.
//!
//! [`check_output_style_legacy_ids`] closes a third gap (issue #3453 part 2):
//! `check_output_style` only ever inspects the single EFFECTIVE layer, by
//! design — but a legacy/unresolvable id left behind in a lower-precedence,
//! currently-*shadowed* layer (e.g. a corrected `settings.json` sitting under
//! a stale `settings.local.json` that DOESN'T currently win) is invisible to
//! it, and is a landmine: tm's config-seed/repair path rewrites `settings.json`
//! but never touches `settings.local.json` (often gitignored personal
//! config), so the moment the winning layer's key is ever removed, the
//! dormant stale id silently becomes effective. This is advisory-only
//! (`Warn`, never `Fail`, and NEVER auto-migrates) — see the function doc for
//! why silently rewriting `.local` files is unsafe.
//! Test: `legacy_ids_warns_on_shadowed_layer`, `legacy_ids_ok_when_all_known`,
//! `legacy_ids_ok_when_none_configured`,
//! `legacy_ids_does_not_double_report_the_effective_layer`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::bundle::OUTPUT_STYLES;
use crate::core::claude_config::ClaudeConfigReader;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Result of reading the `outputStyle` key out of one settings file.
///
/// Why: a settings file can be "silent" on `outputStyle` in two equivalent
/// ways — the file does not exist, or it exists but has no such key — and
/// both must fall through to the next scope in the resolution chain the same
/// way. Splitting this from a malformed/unreadable file (which must stop
/// resolution and `Fail` immediately) needs its own type rather than
/// overloading `Option`.
/// What: `Present` carries the configured id; `Silent` means "keep looking".
/// Test: exercised indirectly by every `check_output_style` test below.
enum StyleKey {
    Present(String),
    Silent,
}

/// Read the `outputStyle` key from `path`, distinguishing "absent" (fall
/// through) from "malformed" (stop and report).
///
/// Why: shared by both the project and global read attempts in
/// [`check_output_style`] so the two scopes apply identical parsing and
/// error-reporting rules.
/// What: `Ok(StyleKey::Silent)` when the file is missing or has no
/// `outputStyle` key; `Ok(StyleKey::Present(id))` when it does;
/// `Err(DoctorCheck)` (a ready-to-return `Fail`) when the file exists but is
/// unreadable or not valid JSON.
/// Test: `output_style_fail_on_malformed_json`, `output_style_warn_when_key_absent`.
fn read_style_key(path: &Path) -> Result<StyleKey, DoctorCheck> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StyleKey::Silent),
        Err(e) => {
            return Err(DoctorCheck::new(
                "output_style",
                CheckStatus::Fail,
                format!("{} unreadable: {e}", path.display()),
            ));
        }
    };

    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!("{} is not valid JSON: {e}", path.display()),
        )
    })?;

    match value.get("outputStyle").and_then(|v| v.as_str()) {
        Some(id) => Ok(StyleKey::Present(id.to_string())),
        None => Ok(StyleKey::Silent),
    }
}

/// Build the ordered settings-file layers Claude Code actually consults for
/// one project, highest-precedence first.
///
/// Why: [`check_output_style`], [`resolve_effective_style_id`], and
/// [`check_output_style_legacy_ids`] all need the IDENTICAL precedence chain —
/// duplicating it per caller is exactly how a check drifts from the consumer
/// it's supposed to model (the general lesson of issue #3453). Reuses
/// [`ClaudeConfigReader::paths_for_project_with_home`] (the project/user
/// four-file path model already shared with the `claude-config` analyzer and
/// checkpointer) rather than re-joining `.claude/settings*.json` a second
/// time.
/// What: `project-local > project > user-local > user`, matching Claude
/// Code's real precedence — scope (project outranks user) applied first,
/// then format (`.local.json` outranks the plain file) within each scope; see
/// the module doc for the evidence trail. The two project-scope layers are
/// omitted entirely when `project_dir` is `None` (no project to resolve).
/// Test: `output_style_project_local_overrides_project`,
/// `output_style_user_local_overrides_user`,
/// `output_style_falls_back_to_global_when_project_missing`.
fn resolution_layers(project_dir: Option<&Path>, home: &Path) -> Vec<(PathBuf, &'static str)> {
    let project = project_dir.unwrap_or(home);
    let paths = ClaudeConfigReader::paths_for_project_with_home(project, home);

    let mut layers = Vec::with_capacity(4);
    if project_dir.is_some() {
        layers.push((paths.project_local_settings, "project-local"));
        layers.push((paths.project_settings, "project"));
    }
    layers.push((paths.user_local_settings, "user-local"));
    layers.push((paths.user_settings, "user"));
    layers
}

/// Probe whether the EFFECTIVE `outputStyle` setting resolves to a real,
/// on-disk trusty-mpm style file.
///
/// Why: this is the deterministic, configuration-level check that catches the
/// exact incident condition (an `outputStyle` value that does not exist),
/// which no session-side "please confirm you loaded" instruction can ever
/// catch, because that instruction itself would be part of what failed to
/// load. It must inspect the value Claude Code will ACTUALLY resolve across
/// EVERY layer it consults — including `settings.local.json`, which Claude
/// Code applies ahead of the plain `settings.json` at the same scope — so a
/// stale value there is never masked by a correct value in a
/// lower-precedence layer (issue #1863's original project-vs-user gap;
/// issue #3453's `.local` gap).
/// What: walks [`resolution_layers`] in precedence order, returning at the
/// first layer that sets the key. `Fail`s immediately on an
/// unreadable/malformed settings file at whichever layer it was found in.
/// `Warn` when NO layer configures a value (Claude Code will use its own
/// default, a valid if unconfigured state). Otherwise validates the resolved
/// id against [`OUTPUT_STYLES`] and the deployed file under
/// `<home>/.claude/output-styles/`, reporting `Fail`/`Ok` with the resolved
/// scope (`"project-local"`/`"project"`/`"user-local"`/`"user"`) named in the
/// message.
/// Test: `output_style_ok_when_style_resolves`, `output_style_fail_when_id_unknown`,
/// `output_style_warn_when_key_absent`, `output_style_fail_when_file_missing`,
/// `output_style_falls_back_to_global_when_project_silent`,
/// `output_style_project_local_wins_even_when_project_settings_correct`.
pub(crate) fn check_output_style(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let layers = resolution_layers(project_dir, home);

    for (path, scope) in &layers {
        match read_style_key(path) {
            Ok(StyleKey::Present(style_id)) => return evaluate_style(&style_id, home, path, scope),
            Ok(StyleKey::Silent) => { /* fall through to the next layer */ }
            Err(check) => return check,
        }
    }

    let checked = layers
        .iter()
        .map(|(p, _)| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    DoctorCheck::new(
        "output_style",
        CheckStatus::Warn,
        format!(
            "no outputStyle configured in any resolved scope (checked {checked}) — \
             Claude Code will use its own default"
        ),
    )
}

/// Validate a resolved `outputStyle` id against the known styles and the
/// deployed style files, naming which scope it was resolved from.
///
/// Why: shared by both the project-scope and global-scope resolution
/// branches in [`check_output_style`] so an unknown id or a missing deployed
/// file is reported identically regardless of which settings file supplied
/// it — only the reported scope/path differs.
/// What: `Fail` when `style_id` matches no [`OUTPUT_STYLES`] entry or its
/// deployed file under `<home>/.claude/output-styles/` is missing/empty;
/// `Ok` otherwise. `source` and `source_path` are folded into the message so
/// the operator knows which file the effective value came from.
/// Test: `output_style_ok_when_style_resolves`, `output_style_fail_when_id_unknown`,
/// `output_style_fail_when_file_missing`.
fn evaluate_style(style_id: &str, home: &Path, source_path: &Path, source: &str) -> DoctorCheck {
    let known_ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    let Some(style) = OUTPUT_STYLES.iter().find(|s| s.id == style_id) else {
        return DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "{source} outputStyle {style_id:?} ({}) is not a known trusty-mpm style \
                 (valid: {}) — run `tm run`/`tm load` to rewrite it correctly",
                source_path.display(),
                known_ids.join(", ")
            ),
        );
    };

    let style_path = home
        .join(".claude")
        .join("output-styles")
        .join(style.file_name);
    match std::fs::metadata(&style_path) {
        Ok(meta) if meta.len() > 0 => DoctorCheck::new(
            "output_style",
            CheckStatus::Ok,
            format!(
                "{source} outputStyle {style_id:?} ({}) resolves to {}",
                source_path.display(),
                style_path.display()
            ),
        ),
        _ => DoctorCheck::new(
            "output_style",
            CheckStatus::Fail,
            format!(
                "{source} outputStyle {style_id:?} ({}) is a known id but {} is missing or \
                 empty — run `tm install` to redeploy styles",
                source_path.display(),
                style_path.display()
            ),
        ),
    }
}

/// Best-effort resolution of the effective `outputStyle` id, ignoring any
/// error/malformed states — those are already reported by
/// [`check_output_style`] itself. Used only to exempt the currently
/// configured style's file from orphan detection below.
///
/// Why: [`check_output_style_staleness`] must not flag the file backing a
/// deliberately configured custom style id (one that resolves to no
/// [`OUTPUT_STYLES`] entry — already `Fail`ed by [`evaluate_style`], but the
/// operator's own file, not a foreign leftover) as an orphan. Must walk the
/// SAME [`resolution_layers`] precedence [`check_output_style`] does — before
/// issue #3453 this only checked plain `settings.json` at each scope, so a
/// custom id configured only via `settings.local.json` was wrongly treated as
/// an orphan.
/// What: first non-silent layer in [`resolution_layers`] order; any
/// unreadable/malformed/absent file at any layer is skipped (resolves to
/// `None` if no layer sets it) rather than propagating an error, since this
/// helper only narrows an advisory scan.
/// Test: `staleness_orphan_exempts_configured_custom_id`.
fn resolve_effective_style_id(project_dir: Option<&Path>, home: &Path) -> Option<String> {
    for (path, _) in resolution_layers(project_dir, home) {
        if let Ok(StyleKey::Present(id)) = read_style_key(&path) {
            return Some(id);
        }
    }
    None
}

/// Scan every resolution layer — not just the effective one — for a
/// legacy/unresolvable `outputStyle` id (issue #3453 part 2).
///
/// Why: [`check_output_style`] only ever inspects the single layer that WINS
/// (by design — that's the only value that affects the current session), so
/// a stale id sitting in a currently-*shadowed*, lower-precedence layer is
/// invisible to it. That is precisely how the live incident happened in
/// reverse: tm's config-seed/repair path rewrites `settings.json` but never
/// touches `settings.local.json` (frequently gitignored personal config), so
/// a legacy id left behind in a shadowed layer is a landmine — it silently
/// becomes effective the moment the winning layer's key is ever removed or
/// the winning file is deleted. This probe is Warn-only and NEVER rewrites
/// any file: `settings.local.json` in particular is often gitignored,
/// operator-owned config that may be mid-edit, and even the shared
/// `settings.json` is edited by the operator directly in many projects, so a
/// doctor probe silently mutating it without being asked would violate the
/// "no silent fallbacks/rewrites" rule — naming the exact file and offending
/// value is enough to make the fix (edit or run `tm run`/`tm load`) obvious,
/// mirroring how [`evaluate_style`] already reports the winning-layer case.
/// What: reads the `outputStyle` key out of EVERY layer in
/// [`resolution_layers`] order. The FIRST layer that sets the key is the
/// effective one — already reported by [`check_output_style`], so it is
/// intentionally excluded here to avoid double-reporting the same offender
/// under two check names. Every layer AFTER that (i.e. genuinely shadowed) is
/// checked against [`OUTPUT_STYLES`]; an id matching no entry is an offender,
/// named as `"<scope> <path> = <id>"`. Unreadable/malformed shadowed layers
/// are skipped (best-effort advisory scan, not a second `Fail` source). `Ok`
/// when no shadowed layer carries an unknown id (including when nothing is
/// configured anywhere); `Warn` naming every offender otherwise.
/// Test: `legacy_ids_warns_on_shadowed_layer`, `legacy_ids_ok_when_all_known`,
/// `legacy_ids_ok_when_none_configured`,
/// `legacy_ids_does_not_double_report_the_effective_layer`.
pub(crate) fn check_output_style_legacy_ids(
    project_dir: Option<&Path>,
    home: &Path,
) -> DoctorCheck {
    let known_ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    let mut seen_effective_layer = false;
    let mut offenders: Vec<String> = Vec::new();

    for (path, scope) in resolution_layers(project_dir, home) {
        let Ok(StyleKey::Present(id)) = read_style_key(&path) else {
            continue;
        };
        if !seen_effective_layer {
            // This is the layer `check_output_style` itself resolves to and
            // already reports on (Fail/Ok) — don't double-report it here.
            seen_effective_layer = true;
            continue;
        }
        if !known_ids.contains(&id.as_str()) {
            offenders.push(format!("{scope} {} = {id:?}", path.display()));
        }
    }

    if offenders.is_empty() {
        return DoctorCheck::new(
            "output_style_legacy_ids",
            CheckStatus::Ok,
            "no legacy/unresolvable outputStyle ids found in any shadowed settings layer",
        );
    }

    DoctorCheck::new(
        "output_style_legacy_ids",
        CheckStatus::Warn,
        format!(
            "legacy/unresolvable outputStyle id(s) found in shadowed (currently non-effective) \
             settings layers ({}) — valid ids: {}; these are dormant until the higher-precedence \
             layer's key is ever removed. The known pre-rebrand id `claude_mpm` has no deployed \
             style and falls back to Claude Code's built-in default if it ever becomes effective \
             — edit or remove the key in the named file(s)",
            offenders.join("; "),
            known_ids.join(", ")
        ),
    )
}

/// Probe deployed output-style files for content drift against the bundled
/// catalog, and flag orphaned files under `output-styles/` (issue #2333).
///
/// Why: [`check_output_style`] only validates that the configured id
/// RESOLVES to a non-empty file — it never compares content, so a stale
/// on-disk copy reports a clean `Ok` even though the deployed text is out of
/// date (the exact gap that let the PR #2328 identity-string fix sit
/// undeployed through an install + daemon restart). A dormant foreign file
/// (e.g. a pre-rebrand `claude-mpm.md`) beside the current styles is also
/// invisible today and is a landmine if `outputStyle` is ever mis-set to it.
/// What: for every [`OUTPUT_STYLES`] entry, byte-compares the deployed
/// `<home>/.claude/output-styles/<file_name>` against the bundled `content`;
/// a mismatch is reported as drifted (a missing file is skipped here — that
/// state is already `Fail`ed by [`check_output_style`], so it is not
/// double-reported as drift). Separately scans `output-styles/` for `.md`
/// files whose name matches no bundled `file_name` and is not
/// `<configured_id>.md` (via [`resolve_effective_style_id`]) — those are
/// orphans, named but NEVER deleted (issue #2333: a foreign file may belong
/// to a separately-installed tool). `Warn` when either set is non-empty,
/// naming what was found and `tm install` as the drift remediation; `Ok`
/// when the directory is fully in sync (including when it does not exist
/// yet — nothing has been deployed there, a state [`check_output_style`]
/// already reports on).
/// Test: `staleness_ok_when_in_sync`, `staleness_warns_on_drift`,
/// `staleness_warns_on_orphan`, `staleness_ok_when_dir_missing`,
/// `staleness_ok_when_file_never_deployed`,
/// `staleness_orphan_exempts_configured_custom_id`.
pub(crate) fn check_output_style_staleness(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let styles_dir = home.join(".claude").join("output-styles");
    let known_names: HashSet<&str> = OUTPUT_STYLES.iter().map(|s| s.file_name).collect();

    let mut drifted: Vec<&str> = Vec::new();
    for style in OUTPUT_STYLES {
        match std::fs::read(styles_dir.join(style.file_name)) {
            Ok(bytes) if bytes != style.content.as_bytes() => drifted.push(style.file_name),
            _ => {} // in sync, missing (not this probe's concern), or unreadable (skip)
        }
    }

    let mut orphans: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&styles_dir) {
        let exempt_custom =
            resolve_effective_style_id(project_dir, home).map(|id| format!("{id}.md"));
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".md") || known_names.contains(name) {
                continue;
            }
            if exempt_custom.as_deref() == Some(name) {
                continue;
            }
            orphans.push(name.to_string());
        }
    }
    orphans.sort_unstable();

    if drifted.is_empty() && orphans.is_empty() {
        return DoctorCheck::new(
            "output_style_staleness",
            CheckStatus::Ok,
            "deployed output styles match the installed binary's bundled assets",
        );
    }

    let mut parts: Vec<String> = Vec::new();
    if !drifted.is_empty() {
        parts.push(format!(
            "{} drifted from bundled content ({}) — run `tm install` to redeploy",
            drifted.len(),
            drifted.join(", ")
        ));
    }
    if !orphans.is_empty() {
        parts.push(format!(
            "{} unrecognized file(s) present ({}) — not removed automatically; confirm they \
             are not referenced by outputStyle before deleting",
            orphans.len(),
            orphans.join(", ")
        ));
    }

    DoctorCheck::new(
        "output_style_staleness",
        CheckStatus::Warn,
        parts.join("; "),
    )
}

#[cfg(test)]
#[path = "doctor_output_style_tests.rs"]
mod tests;
