//! Inspecting and preserving an already-installed launchd service unit.
//!
//! Why (issue #4823): `trusty-search service install` regenerates the
//! LaunchAgent plist from a fixed template. Any deliberate operator
//! configuration outside that template — notably the auto-discovery
//! suppression — was discarded on every regeneration, so an operator who
//! disabled auto-discovery lost it the moment the unit was reinstalled and
//! could not make the setting durable by any supported means. Regenerating a
//! service unit must not silently discard operator intent.
//!
//! What: three things —
//!
//! 1. [`parse_truthy_bool`] — the shared truthiness rule. It is the clap
//!    `value_parser` for `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER`
//!    *and* the rule used to read an installed unit. Before #4823 the flag was
//!    a bare `bool` with `env = …`, which under clap means the CLI flag is a
//!    presence flag but the env var goes through strict `FromStr<bool>` —
//!    accepting only `true`/`false`. The documented spelling
//!    `TRUSTY_NO_AUTO_DISCOVER=1` (README, CHANGELOG #314) was therefore
//!    *rejected* and the daemon refused to boot. Platform-independent: the
//!    flag exists everywhere, so this and its tests are never gated.
//! 2. [`InstalledUnit`] + [`parse_installed_unit`] — a dependency-free reader
//!    for the two plist sections that carry operator intent
//!    (`ProgramArguments`, `EnvironmentVariables`).
//! 3. [`resolve_auto_discover`] / [`resolve_persisted_env`] — what the
//!    regenerated unit should carry forward.
//!
//! Items 2 and 3 are `#[cfg(target_os = "macos")]`: launchd is the only
//! service manager this crate generates units for, so their sole consumers
//! ([`crate::commands::service`]) compile out elsewhere and leaving them
//! ungated is dead code that fails `clippy -D warnings` on Linux. Their logic
//! is still pure string handling — the macOS gate is about who calls them, not
//! about platform APIs.
//!
//! Test: `parse_truthy_bool_*` and `cli_tests::*` (every platform);
//! `parse_installed_unit_*`, `suppresses_auto_discover_*`,
//! `resolve_auto_discover_*`, `resolve_persisted_env_*` (macOS).

#[cfg(target_os = "macos")]
use crate::service::PERSISTED_ENV_VARS;

/// Env var backing `--no-auto-discover`.
///
/// This name is deliberately NEVER emitted into a generated plist's
/// `EnvironmentVariables` — see [`resolve_persisted_env`].
#[cfg(target_os = "macos")]
pub const NO_AUTO_DISCOVER_ENV: &str = "TRUSTY_NO_AUTO_DISCOVER";

/// The CLI spelling written into the generated plist's `ProgramArguments`.
#[cfg(target_os = "macos")]
pub const NO_AUTO_DISCOVER_ARG: &str = "--no-auto-discover";

/// Parse a boolean written the way operators actually write booleans.
///
/// Why (issue #4823): clap's built-in `bool` env parsing accepts only the
/// literal strings `true` / `false`. Every other trusty-search document —
/// the `--no-auto-discover` help text, the README, the #314 changelog entry,
/// and the `no_auto_discover_resolution` unit test — tells operators to write
/// `TRUSTY_NO_AUTO_DISCOVER=1`, which clap then rejected with
/// `invalid value '1' … [possible values: true, false]`, aborting daemon
/// startup. This parser makes the code agree with the documentation instead
/// of the other way round.
///
/// Deliberately still strict about garbage: an unrecognised spelling is an
/// error, not a silent `false`. A typo therefore fails loudly at startup
/// rather than quietly re-enabling a scan the operator disabled — the exact
/// silent-capability-change failure mode #4823 is about. The empty string is
/// the one exception: `TRUSTY_NO_AUTO_DISCOVER=` reads as "not set" and maps
/// to `false`, matching the pre-existing documented behaviour.
///
/// What: case-insensitive, whitespace-trimmed. `1`/`true`/`yes`/`on` → `true`;
/// ``/`0`/`false`/`no`/`off` → `false`; anything else → `Err`.
///
/// Deliberately NOT merged with `commands::start::embedder::truthy_env`, which
/// recognises the same truthy set but is fail-open (unset or unrecognised →
/// `false`) and reads the process env itself. That contract is right for an
/// opt-in escape hatch and wrong for a clap `value_parser`, where swallowing a
/// typo would silently re-enable the scan.
/// Test: `parse_truthy_bool_accepts_numeric_and_word_spellings`,
/// `parse_truthy_bool_rejects_garbage`.
pub fn parse_truthy_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => Err(format!(
            "invalid boolean `{other}` — expected one of: \
             1, true, yes, on, 0, false, no, off"
        )),
    }
}

/// The operator-meaningful contents of an installed launchd plist.
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstalledUnit {
    /// `ProgramArguments` minus `argv[0]` handling — every `<string>` in the
    /// array, in document order (so element 0 is the executable path).
    pub args: Vec<String>,
    /// `EnvironmentVariables` as ordered key/value pairs.
    pub env: Vec<(String, String)>,
    /// `WorkingDirectory`, when the installed unit set one.
    ///
    /// #4868: the live unit on the owner's host carries this and the generated
    /// template never emitted it, so regeneration silently dropped the
    /// daemon's working directory along with the env keys.
    pub working_directory: Option<String>,
}

#[cfg(target_os = "macos")]
impl InstalledUnit {
    /// Look up an environment variable the installed unit carried.
    pub fn env_value(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Report whether this unit disables the daemon's auto-discovery scan.
    ///
    /// Why: an operator can have expressed the suppression two ways — the CLI
    /// flag in `ProgramArguments` (what this code now generates) or the env
    /// var in `EnvironmentVariables` (what the legacy hand-made unit and the
    /// 2026-08-04 hand-edit used). Both must be recognised or regeneration
    /// silently drops the hand-edit.
    /// What: true when `ProgramArguments` contains `--no-auto-discover` (bare,
    /// or `=<truthy>`), or `TRUSTY_NO_AUTO_DISCOVER` is set to a truthy value.
    /// An explicit falsey spelling (`--no-auto-discover=false`,
    /// `TRUSTY_NO_AUTO_DISCOVER=0`) reads as *not* suppressed.
    /// Test: `suppresses_auto_discover_via_arg`,
    /// `suppresses_auto_discover_via_env`,
    /// `suppresses_auto_discover_respects_explicit_false`.
    pub fn suppresses_auto_discover(&self) -> bool {
        for arg in &self.args {
            if arg == NO_AUTO_DISCOVER_ARG {
                return true;
            }
            if let Some(val) = arg.strip_prefix(&format!("{NO_AUTO_DISCOVER_ARG}=")) {
                return parse_truthy_bool(val).unwrap_or(false);
            }
        }
        self.env_value(NO_AUTO_DISCOVER_ENV)
            .map(|v| parse_truthy_bool(v).unwrap_or(false))
            .unwrap_or(false)
    }
}

/// Read the operator-meaningful sections out of a launchd plist document.
///
/// Why: we need to know what the unit on disk already says before overwriting
/// it. Pulling in a full plist parser for two well-known keys is not worth a
/// new dependency, and the sections we care about are flat.
/// What: extracts `ProgramArguments` (an `<array>` of `<string>`) and
/// `EnvironmentVariables` (a `<dict>` of `<key>`/`<string>` pairs), XML-
/// unescaping every value. Missing or malformed sections yield empty vectors
/// rather than an error — a unit we cannot read is treated as carrying no
/// operator intent, which is the same position we were in before #4823.
/// Test: `parse_installed_unit_reads_args_and_env`,
/// `parse_installed_unit_tolerates_missing_sections`,
/// `parse_installed_unit_unescapes_xml`.
#[cfg(target_os = "macos")]
pub fn parse_installed_unit(xml: &str) -> InstalledUnit {
    InstalledUnit {
        args: section(xml, "ProgramArguments", "<array>", "</array>")
            .map(|body| {
                scan_tags(body, &["string"])
                    .into_iter()
                    .map(|(_, v)| v)
                    .collect()
            })
            .unwrap_or_default(),
        env: section(xml, "EnvironmentVariables", "<dict>", "</dict>")
            .map(pair_key_strings)
            .unwrap_or_default(),
        // #4868: a scalar, not a container, so it is read by scanning the one
        // `<string>` that follows the key rather than via `section`.
        working_directory: scalar_string(xml, "WorkingDirectory"),
    }
}

/// Read the `<string>` value immediately following `<key>{name}</key>`.
///
/// Why: `WorkingDirectory` is a top-level scalar; [`section`] only finds
/// balanced containers.
/// What: returns the first `<string>` after the key, or `None` when the key is
/// absent or is not followed by a string.
/// Test: `parse_installed_unit_reads_working_directory`.
#[cfg(target_os = "macos")]
fn scalar_string(xml: &str, name: &str) -> Option<String> {
    let needle = format!("<key>{name}</key>");
    let after = &xml[xml.find(&needle)? + needle.len()..];
    let (tag, text) = scan_tags(after, &["string", "key"]).into_iter().next()?;
    (tag == "string").then_some(text)
}

/// Pair `<key>`/`<string>` tokens into (key, value), skipping any key whose
/// value is not a plain string (a `<key>` immediately followed by another
/// `<key>` drops the first).
#[cfg(target_os = "macos")]
fn pair_key_strings(body: &str) -> Vec<(String, String)> {
    let tokens = scan_tags(body, &["key", "string"]);
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for (tag, text) in tokens {
        match tag.as_str() {
            "key" => pending = Some(text),
            _ => {
                if let Some(k) = pending.take() {
                    out.push((k, text));
                }
            }
        }
    }
    out
}

/// Return the body between the balanced `open`/`close` container that follows
/// `<key>{name}</key>`.
#[cfg(target_os = "macos")]
fn section<'a>(xml: &'a str, name: &str, open: &str, close: &str) -> Option<&'a str> {
    let needle = format!("<key>{name}</key>");
    let after = &xml[xml.find(&needle)? + needle.len()..];
    let body_start = after.find(open)? + open.len();
    let mut depth = 1usize;
    let mut cursor = body_start;
    while depth > 0 {
        let next_close = after[cursor..].find(close).map(|p| cursor + p)?;
        let next_open = after[cursor..next_close].find(open).map(|p| cursor + p);
        match next_open {
            Some(o) => {
                depth += 1;
                cursor = o + open.len();
            }
            None => {
                depth -= 1;
                if depth == 0 {
                    return Some(&after[body_start..next_close]);
                }
                cursor = next_close + close.len();
            }
        }
    }
    None
}

/// Collect `(tag, unescaped_text)` for every occurrence of any tag in `tags`,
/// in document order.
#[cfg(target_os = "macos")]
fn scan_tags(body: &str, tags: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while cursor < body.len() {
        let mut best: Option<(usize, &str)> = None;
        for tag in tags {
            if let Some(rel) = body[cursor..].find(&format!("<{tag}>")) {
                let abs = cursor + rel;
                if best.is_none_or(|(b, _)| abs < b) {
                    best = Some((abs, tag));
                }
            }
        }
        let Some((pos, tag)) = best else { break };
        let text_start = pos + tag.len() + 2;
        let close = format!("</{tag}>");
        let Some(rel_end) = body[text_start..].find(&close) else {
            break;
        };
        let text_end = text_start + rel_end;
        out.push((
            (*tag).to_string(),
            xml_unescape(&body[text_start..text_end]),
        ));
        cursor = text_end + close.len();
    }
    out
}

/// Reverse the escaping applied by `LaunchdConfig::render_plist`.
#[cfg(target_os = "macos")]
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// What the regenerated unit should do about auto-discovery.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDiscover {
    /// Emit `--no-auto-discover`. `preserved` is true when the suppression came
    /// from the installed unit rather than from this invocation's flag.
    Suppress { preserved: bool },
    /// Do not emit the flag. `dropped` is true when the installed unit *did*
    /// suppress and the operator explicitly asked to re-enable.
    Enable { dropped: bool },
}

#[cfg(target_os = "macos")]
impl AutoDiscover {
    /// Whether the generated `ProgramArguments` must carry the flag.
    pub fn suppressed(self) -> bool {
        matches!(self, AutoDiscover::Suppress { .. })
    }
}

/// Decide the auto-discovery setting for a regenerated unit.
///
/// Why (issue #4823): silently discarding a suppression the installed unit
/// carried is a silent capability change — the daemon starts scanning
/// directories the operator deliberately excluded. Preservation is the
/// default; turning it back on must be an explicit act, and either way the
/// caller announces what happened. Requiring an explicit flag on *every*
/// install would be the wrong trade: it makes the common case hostile without
/// making the dangerous case any safer.
/// What: `--auto-discover` wins (and reports whether it dropped anything),
/// then `--no-auto-discover`, then whatever the installed unit carried.
/// Test: `resolve_auto_discover_preserves_existing_suppression`,
/// `resolve_auto_discover_explicit_request`,
/// `resolve_auto_discover_explicit_reenable_reports_drop`,
/// `resolve_auto_discover_defaults_to_enabled`.
#[cfg(target_os = "macos")]
pub fn resolve_auto_discover(
    request_off: bool,
    request_on: bool,
    existing: Option<&InstalledUnit>,
) -> AutoDiscover {
    let existing_off = existing.is_some_and(InstalledUnit::suppresses_auto_discover);
    if request_on {
        return AutoDiscover::Enable {
            dropped: existing_off,
        };
    }
    if request_off || existing_off {
        return AutoDiscover::Suppress {
            preserved: !request_off && existing_off,
        };
    }
    AutoDiscover::Enable { dropped: false }
}

/// Resolve the operator tunables to write into the regenerated plist.
///
/// Why (issue #4823, generalised): `service install` is normally run from a
/// plain shell that exports none of the `TRUSTY_*` tunables, so the previous
/// "read the current process env" behaviour blanked every tunable the
/// installed unit carried — the same silent-discard defect as the
/// auto-discover case, just less visible. Carrying values forward makes
/// regeneration non-destructive while still letting an operator override any
/// single var by exporting it before the install.
/// What: for each [`PERSISTED_ENV_VARS`] key, prefer the process env, then the
/// installed unit's value. [`NO_AUTO_DISCOVER_ENV`] is never emitted: the
/// suppression is expressed as a `ProgramArguments` flag, which is
/// unambiguous, human-readable in the plist, and — crucially — cannot carry a
/// value the daemon's clap parser would reject on boot.
///
/// #4868: an allowlist was never enough, and became actively dangerous once
/// install started overwriting the LIVE unit instead of a differently-named
/// one. The plist on the owner's host carried five keys no allowlist mentioned
/// — `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS`, `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`,
/// `FASTEMBED_CACHE_DIR`, `FASTEMBED_CACHE_PATH`, `RUST_LOG` — and the first of
/// those is the hand-patch from an incident where a restart cost a 200k-chunk
/// index to a 30 s redb open timeout under warm-boot contention. Dropping it
/// re-arms that incident, invisibly, until the next restart. So every key the
/// installed unit carried is now carried forward unless the generated template
/// sets it itself; extending the allowlist would only have postponed the next
/// loss.
///
/// Precedence, highest first: the process env, then the installed unit. Keys
/// the template computes at install time (`template_owned`) are never carried
/// forward — a stale `HF_HOME` or `PATH` from an old unit must not outrank the
/// freshly resolved one.
///
/// Test: `resolve_persisted_env_prefers_process_env`,
/// `resolve_persisted_env_carries_forward_installed_values`,
/// `resolve_persisted_env_never_emits_no_auto_discover`,
/// `resolve_persisted_env_carries_forward_unknown_keys`.
#[cfg(target_os = "macos")]
pub fn resolve_persisted_env(
    lookup: impl Fn(&str) -> Option<String>,
    existing: Option<&InstalledUnit>,
    template_owned: &[&str],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |key: &str, value: String| {
        if key == NO_AUTO_DISCOVER_ENV || template_owned.contains(&key) {
            return;
        }
        if out.iter().any(|(k, _)| k == key) {
            return;
        }
        out.push((key.to_string(), value));
    };

    // Named tunables first, so their order in the rendered plist stays stable
    // and an operator export still outranks the installed value.
    for key in PERSISTED_ENV_VARS {
        if let Some(value) =
            lookup(key).or_else(|| existing.and_then(|u| u.env_value(key)).map(str::to_owned))
        {
            push(key, value);
        }
    }

    // Then everything else the unit carried. This is the part that generalises:
    // a key nobody anticipated survives regeneration instead of vanishing.
    if let Some(unit) = existing {
        for (key, value) in &unit.env {
            let resolved = lookup(key).unwrap_or_else(|| value.clone());
            push(key, resolved);
        }
    }

    out
}

// Test bodies live in sibling `service_unit_*_tests.rs` files (each name
// ending `_tests.rs`, so `scripts/check_line_cap.sh` classifies them as
// test files under the 3000-SLOC cap rather than counting against this
// file's 500-SLOC production budget) — same split convention as
// `service.rs` / `service_tests.rs` in this directory. `#[path]` keeps each
// one nested exactly where its inline body used to be, so `use super::*;`
// inside them resolves identically.
#[cfg(test)]
#[path = "service_unit_truthy_bool_tests.rs"]
mod tests;

/// Tests for the launchd-unit half of this module — see
/// `service_unit_launchd_tests.rs`.
///
/// Gated to macOS for the same reason the code under test is — see the module
/// docs. The logic is pure string handling, so these would run anywhere; the
/// items they exercise simply do not exist on other targets.
#[cfg(test)]
#[cfg(target_os = "macos")]
#[path = "service_unit_launchd_tests.rs"]
mod launchd_unit_tests;

/// End-to-end clap tests for [`parse_truthy_bool`] — see
/// `service_unit_cli_tests.rs`.
#[cfg(test)]
#[path = "service_unit_cli_tests.rs"]
mod cli_tests;
