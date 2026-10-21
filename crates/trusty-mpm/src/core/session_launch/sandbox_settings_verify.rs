//! The read-back half of the #8928 step 5 sandbox settings generator.
//!
//! Why: `.claude/settings.local.json` outranks the project file and can turn
//! `sandbox.enabled` or `allowUnsandboxedCommands` off, `excludedCommands`
//! runs the commands it lists outside the sandbox even with
//! `allowUnsandboxedCommands: false`, and a merge can lose to a concurrent
//! writer. #8928 step 5 therefore reads
//! the merged files back and refuses the launch unless they hold the sandbox
//! the role asked for. #10107: the merge keeps user-tier `sandbox` keys, list
//! keys such as `allowUnixSockets` merge across tiers, and an edit-tool
//! `permissions.allow` rule widens `allowWrite`, so verify reads those too.
//! What: [`verify_sandbox_settings`]; split from `sandbox_settings.rs` for the
//! SLOC cap.
//! Test: `sandbox_settings_tests.rs`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::sandbox_settings::{SandboxInputs, SandboxRole, SandboxSettingsError, project_policy};

/// Keys that widen the sandbox unless `false` (#10107). `disabled` counts
/// only under `filesystem`.
const WIDENING_FLAGS: [&str; 3] = [
    "allowAllUnixSockets",
    "allowLocalBinding",
    "allowAppleEvents",
];

/// The `permissions.allow` rule names whose path widens `allowWrite`
/// (#10107), compared case-insensitively.
const EDIT_RULES: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The characters that start a glob in a rule path.
const GLOB_CHARS: [char; 4] = ['*', '?', '[', '{'];

/// Read back the project, local and user tiers and refuse a wrong sandbox.
///
/// Why: see the module doc.
/// What: for a PM or agent, refuses unless the project file's
/// `sandbox.enabled` is `true` and `sandbox.allowUnsandboxedCommands` is
/// `false`, the user file's `sandbox.network.strictAllowlist` is `true`,
/// neither file sets a non-empty `sandbox.excludedCommands` (#8928 fix
/// round), and `settings.local.json` sets no `sandbox` key. #10107: refuses
/// too when any tier sets `allowAllUnixSockets`, `allowLocalBinding`,
/// `allowAppleEvents` or `filesystem.disabled` to anything but `false`, or a
/// non-empty `allowRead`; when the user tier sets a `sandbox` key other than
/// `network.strictAllowlist` and `credentials`; and when any tier's
/// `permissions.allow` holds an edit-tool rule outside the workspace and the
/// role's `allowWrite` roots. For the Architect, refuses unless the project
/// file's `sandbox.enabled` is `false`. FAIL-CLOSED: a missing project or user
/// file, any of the three that cannot be read or is not a JSON object, an
/// allow list or rule that is not text, and a rule path that cannot be placed
/// are errors; only a missing local file is fine.
///
/// # Errors
///
/// [`SandboxSettingsError::Unreadable`] or [`SandboxSettingsError::Refused`].
///
/// Test: `a_local_override_refuses_the_launch`,
/// `a_weakened_project_file_refuses_the_launch`,
/// `unreadable_settings_refuse_the_launch`,
/// `excluded_commands_refuse_the_launch`,
/// `widening_sandbox_keys_refuse_the_launch`,
/// `apple_events_and_outside_edit_rules_refuse_the_launch`,
/// `inside_edit_rules_still_verify`,
/// `an_edit_rule_with_no_home_to_place_it_refuses`.
pub fn verify_sandbox_settings(
    role: SandboxRole,
    inputs: &SandboxInputs,
    user_config_dir: &Path,
) -> Result<(), SandboxSettingsError> {
    let workspace = &inputs.workspace;
    let claude = workspace.join(".claude");
    let project_file = claude.join("settings.json");
    let project = read(&project_file, true)?.unwrap_or(Value::Null);
    let enabled = project.pointer("/sandbox/enabled");
    if role == SandboxRole::Architect {
        return (enabled == Some(&Value::Bool(false)))
            .then_some(())
            .ok_or_else(|| {
                refused("the Architect's project settings do not set `sandbox.enabled: false`")
            });
    }
    let local_file = claude.join("settings.local.json");
    let local = read(&local_file, false)?.unwrap_or(Value::Null);
    if let Some(sandbox) = local.get("sandbox") {
        let keys = sandbox
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return Err(refused(&format!(
            "`.claude/settings.local.json` overrides the sandbox ({keys}); remove its `sandbox` key"
        )));
    }
    if enabled != Some(&Value::Bool(true)) {
        return Err(refused(
            "the project settings do not set `sandbox.enabled: true`",
        ));
    }
    if project.pointer("/sandbox/allowUnsandboxedCommands") != Some(&Value::Bool(false)) {
        return Err(refused(
            "the project settings do not set `sandbox.allowUnsandboxedCommands: false`",
        ));
    }
    let user_file = user_config_dir.join("settings.json");
    let user = read(&user_file, true)?.unwrap_or(Value::Null);
    // #10107: the roots an edit-tool allow rule may name — the workspace and
    // the role's own `allowWrite`, never the file's, which a writer can widen.
    let policy = project_policy(role, inputs);
    let mut roots = vec![workspace.clone()];
    roots.extend(
        policy
            .pointer("/sandbox/filesystem/allowWrite")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(PathBuf::from),
    );
    let tiers = [
        (&project, &project_file, [workspace.clone(), claude.clone()]),
        (&local, &local_file, [workspace.clone(), claude.clone()]),
        (
            &user,
            &user_file,
            [user_config_dir.to_path_buf(), workspace.clone()],
        ),
    ];
    for (settings, file, bases) in &tiers {
        // #8928 fix round: an excluded command runs outside the sandbox.
        if let Some(why) = excluded_commands(settings, file) {
            return Err(why);
        }
        // #10107: widening keys, and edit-tool rules that widen `allowWrite`.
        if let Some(why) = widening_key(settings.get("sandbox"), "sandbox", file) {
            return Err(why);
        }
        let place = Place {
            bases,
            workspace,
            home: &inputs.home,
        };
        edit_allow_rules(settings, file, &place, &roots)?;
    }
    user_tier_keys(&user, &user_file)?;
    if user.pointer("/sandbox/network/strictAllowlist") != Some(&Value::Bool(true)) {
        return Err(refused(
            "tm's user-level settings do not set `sandbox.network.strictAllowlist: true`",
        ));
    }
    Ok(())
}

/// The refusal when `settings` sets a non-empty `sandbox.excludedCommands`.
fn excluded_commands(settings: &Value, file: &Path) -> Option<SandboxSettingsError> {
    let list = settings.pointer("/sandbox/excludedCommands")?;
    if list.is_null() || list.as_array().is_some_and(Vec::is_empty) {
        return None;
    }
    Some(refused(&format!(
        "`{}` sets `sandbox.excludedCommands` to {list}, which runs those commands outside \
         the sandbox; remove the key or empty the list",
        file.display()
    )))
}

/// The refusal for the first widening key under `value`, at any depth.
///
/// Why: #10107 — these keys open the sandbox, and the merge keeps them.
/// What: a [`WIDENING_FLAGS`] key, or `disabled` under `filesystem`, set to
/// anything but `false` or `null` refuses; so does an `allowRead` that is not
/// `null` or an empty list, because it overrides `denyRead`. Fail-closed: a
/// non-boolean flag and a non-list `allowRead` refuse.
/// Test: `widening_sandbox_keys_refuse_the_launch`,
/// `apple_events_and_outside_edit_rules_refuse_the_launch`.
fn widening_key(value: Option<&Value>, key: &str, file: &Path) -> Option<SandboxSettingsError> {
    let map = value?.as_object()?;
    for (k, v) in map {
        let flag = WIDENING_FLAGS.contains(&k.as_str()) || (key == "filesystem" && k == "disabled");
        let widens = if flag {
            !(v.is_null() || *v == Value::Bool(false))
        } else if k == "allowRead" {
            !(v.is_null() || v.as_array().is_some_and(Vec::is_empty))
        } else {
            false
        };
        if widens {
            return Some(refused(&format!(
                "`{}` sets `{key}.{k}` to {v}, which widens the sandbox",
                file.display()
            )));
        }
        if let Some(why) = widening_key(Some(v), k, file) {
            return Some(why);
        }
    }
    None
}

/// Refuse a user-tier `sandbox` key other than `network.strictAllowlist` and
/// `credentials`.
///
/// Why: #10107 — the merge keeps every user-tier key, and list keys such as
/// `allowUnixSockets` merge across tiers, so a user-tier list could re-add the
/// sccache socket.
/// What: `credentials` passes only as an object. Any other key passes only
/// when [`is_empty_of_its_type`]; a non-object `sandbox` is left to the
/// `strictAllowlist` check, which refuses it. `network` passes too as an
/// object whose other keys are empty of their type. #10107 fix round: Claude
/// Code may skip a user file whose values its schema rejects, which would
/// drop `strictAllowlist` and the credentials deny, so `null` and a
/// mistyped empty value refuse.
/// Test: `widening_sandbox_keys_refuse_the_launch`,
/// `type_matched_empty_user_keys_still_verify`,
/// `excluded_commands_refuse_the_launch`.
fn user_tier_keys(user: &Value, file: &Path) -> Result<(), SandboxSettingsError> {
    let Some(sandbox) = user.get("sandbox").and_then(Value::as_object) else {
        return Ok(());
    };
    for (key, value) in sandbox {
        let allowed = match key.as_str() {
            "credentials" => value.is_object(),
            "network" => value.as_object().is_some_and(|network| {
                network
                    .iter()
                    .all(|(k, v)| k == "strictAllowlist" || is_empty_of_its_type(k, v))
            }),
            _ => is_empty_of_its_type(key, value),
        };
        if !allowed {
            return Err(refused(&format!(
                "`{}` sets `sandbox.{key}` to {value}; tm's user tier may hold only \
                 `sandbox.network.strictAllowlist`, a `sandbox.credentials` object, and \
                 empty values of a key's own type",
                file.display()
            )));
        }
    }
    Ok(())
}

/// The sandbox keys whose value is a list.
const LIST_KEYS: [&str; 7] = [
    "excludedCommands",
    "allowUnixSockets",
    "allowedDomains",
    "allowRead",
    "allowWrite",
    "denyRead",
    "denyWrite",
];

/// The sandbox keys whose value is an object.
const OBJECT_KEYS: [&str; 3] = ["network", "filesystem", "credentials"];

/// Whether `value` adds nothing and has `key`'s type: `[]` for a
/// [`LIST_KEYS`] key, `{}` for an [`OBJECT_KEYS`] key. `null`, and any empty
/// value of an unknown key, is not (#10107 fix round).
fn is_empty_of_its_type(key: &str, value: &Value) -> bool {
    (LIST_KEYS.contains(&key) && value.as_array().is_some_and(Vec::is_empty))
        || (OBJECT_KEYS.contains(&key) && value.as_object().is_some_and(serde_json::Map::is_empty))
}

/// Where one tier's rule paths are placed.
struct Place<'a> {
    /// The directories a `/path` rule may be relative to: the settings
    /// file's own and the project's. Every one must land inside a root.
    bases: &'a [PathBuf; 2],
    /// The session's working directory, for a relative rule.
    workspace: &'a Path,
    /// The home a `~/` rule names.
    home: &'a Path,
}

/// Refuse an edit-tool `permissions.allow` rule outside `roots`.
///
/// Why: #10107 — Claude Code adds an `Edit(...)` allow path to the sandbox's
/// `allowWrite`, so a committed `Edit(~/.cargo/bin/**)` would let sandboxed
/// Bash replace `tm`.
/// What: each [`EDIT_RULES`] rule's path is placed as Claude Code reads it —
/// `//p` absolute, `~/p` under the home, `/p` under each of `place.bases`, any
/// other relative to the workspace — and its literal prefix (up to the first
/// glob), after [`resolve_existing`], must sit inside one of `roots`, each
/// resolved the same way. Fail-closed: a non-list `allow`, a non-text rule, a
/// bare `Edit`, a `..` segment, a `~user` path, a home that is not absolute
/// and a path or root whose symlinks cannot be resolved all refuse.
/// Test: `apple_events_and_outside_edit_rules_refuse_the_launch`,
/// `inside_edit_rules_still_verify`, `an_edit_rule_with_no_home_to_place_it_refuses`,
/// `an_edit_rule_path_that_cannot_be_resolved_refuses`.
fn edit_allow_rules(
    settings: &Value,
    file: &Path,
    place: &Place<'_>,
    roots: &[PathBuf],
) -> Result<(), SandboxSettingsError> {
    let why = |what: String| refused(&format!("`{}` {what}", file.display()));
    let Some(allow) = settings.pointer("/permissions/allow") else {
        return Ok(());
    };
    let Some(rules) = allow.as_array() else {
        return Err(why(format!(
            "sets `permissions.allow` to {allow}, not a list"
        )));
    };
    let mut edits = Vec::new();
    for rule in rules {
        let Some(text) = rule.as_str() else {
            return Err(why(format!(
                "holds the allow rule {rule}, which is not text"
            )));
        };
        edits.extend(edit_spec(text).map(|spec| (text, spec)));
    }
    if edits.is_empty() {
        return Ok(());
    }
    // #10107 fix round: judged after symlinks, so a committed `tools ->
    // <outside>` link cannot pass `Edit(/tools/**)` on its spelling.
    let unresolved = |path: &Path, e: std::io::Error| {
        why(format!(
            "names `{}`, whose symlinks cannot be resolved ({e})",
            path.display()
        ))
    };
    let mut real_roots = Vec::new();
    for root in roots {
        real_roots.push(resolve_existing(root).map_err(|e| unresolved(root, e))?);
    }
    for (text, spec) in edits {
        let outside = || {
            why(format!(
                "allows `{text}`, which widens the sandbox's writes outside the workspace and \
                 its `allowWrite` roots"
            ))
        };
        for prefix in rule_prefixes(spec, place).ok_or_else(outside)? {
            let real = resolve_existing(&prefix).map_err(|e| unresolved(&prefix, e))?;
            if !real_roots.iter().any(|root| real.starts_with(root)) {
                return Err(outside());
            }
        }
    }
    Ok(())
}

/// `path` with the symlinks of its deepest existing ancestor resolved, and
/// the rest joined as spelled.
///
/// Why: #10107 fix round — the sandbox writes where a link points, not where
/// it is spelled.
/// What: walks `path`'s ancestors to the first that `canonicalize` resolves.
/// Fail-closed: a `canonicalize` error other than NotFound is `Err`, and so
/// is a dangling symlink on the way, which a later write could follow out.
/// Test: `apple_events_and_outside_edit_rules_refuse_the_launch`,
/// `an_edit_rule_path_that_cannot_be_resolved_refuses`,
/// `inside_edit_rules_still_verify`.
fn resolve_existing(path: &Path) -> std::io::Result<PathBuf> {
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(real) => {
                let rest = path.strip_prefix(ancestor).map_err(std::io::Error::other)?;
                return Ok(if rest.as_os_str().is_empty() {
                    real
                } else {
                    real.join(rest)
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    return Err(std::io::Error::other("a dangling symlink"));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no ancestor resolves"))
}

/// The path of an [`EDIT_RULES`] rule (`""` for a bare one), or `None` for
/// any other rule.
fn edit_spec(rule: &str) -> Option<&str> {
    let rule = rule.trim();
    let (tool, spec) = match rule.split_once('(') {
        Some((tool, rest)) => (tool.trim(), rest.strip_suffix(')').unwrap_or(rest)),
        None => (rule, ""),
    };
    EDIT_RULES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(tool))
        .then_some(spec.trim())
}

/// The absolute literal prefix of `spec` under each base it may be read
/// against; `None` when it cannot be placed (see [`edit_allow_rules`]).
fn rule_prefixes(spec: &str, place: &Place<'_>) -> Option<Vec<PathBuf>> {
    let full: Vec<PathBuf> = if let Some(rest) = spec.strip_prefix("//") {
        vec![Path::new("/").join(rest)]
    } else if spec == "~" || spec.starts_with("~/") {
        if !place.home.is_absolute() {
            return None;
        }
        vec![place.home.join(spec[1..].trim_start_matches('/'))]
    } else if spec.is_empty() || spec.starts_with('~') {
        return None;
    } else if let Some(rest) = spec.strip_prefix('/') {
        place.bases.iter().map(|base| base.join(rest)).collect()
    } else {
        vec![place.workspace.join(spec)]
    };
    full.iter().map(|path| literal_prefix(path)).collect()
}

/// `path` up to its first glob segment; `None` on a `..` segment or when the
/// result is not absolute.
fn literal_prefix(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut globbed = false;
    for component in path.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            Component::Normal(name) if globbed || name.to_string_lossy().contains(GLOB_CHARS) => {
                globbed = true;
            }
            other => out.push(other),
        }
    }
    out.is_absolute().then_some(out)
}

/// `path` as a JSON object; `Ok(None)` when absent and not `required`.
fn read(path: &Path, required: bool) -> Result<Option<Value>, SandboxSettingsError> {
    let unreadable = |why: String| SandboxSettingsError::Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        Ok(_) => Err(unreadable("not a JSON object".to_owned())),
        Err(e) => Err(unreadable(e.to_string())),
    }
}

/// A refusal naming `why`.
fn refused(why: &str) -> SandboxSettingsError {
    SandboxSettingsError::Refused(format!("#8928 sandbox settings: {why}"))
}
