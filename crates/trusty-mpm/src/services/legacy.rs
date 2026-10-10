//! Load the user's `~/.claude-mpm/services.yaml`, mapping a retired
//! trusty-search entry on read (#9543).
//!
//! Why: `tm services init` wrote trusty-search as a static HTTP probe of port
//! 7878. trusty-search 0.59.0 binds no TCP port, so that entry would report a
//! live daemon DOWN. The file belongs to the user: it is mapped in memory, with
//! one warning, and never rewritten. The same holds for the `start --no-http`
//! that trusty-mpm 1.8.x wrote into its start and restart commands (#9536).
//! What: [`load_user_manifest`] parses, maps, strips, validates and
//! tilde-expands one file, and falls back to the embedded default when the
//! file is absent.
//! Test: `legacy_search_entry_maps_to_socket_and_leaves_the_file_alone`,
//! `other_trusty_search_shapes_are_left_alone`,
//! `search_start_port_is_stripped_in_both_spellings_and_nothing_else_is`.

use std::io::Write;
use std::path::Path;

use super::manifest::{HealthProbe, PortDiscovery, ServiceDecl, ServicesManifest};

/// The one service whose retired shape is mapped.
const SEARCH_SERVICE: &str = "trusty-search";

/// The TCP port the retired trusty-search entry probed.
const LEGACY_SEARCH_PORT: u16 = 7878;

/// Load the manifest `tm services` probes with.
///
/// Why: the binary used to parse the user's file inline, twice; the legacy
/// mapping has to apply to every read, and it needs a test.
/// What: absent file → the embedded default. Otherwise parse, map a legacy
/// trusty-search entry to `health_probe: uds_search`, strip the flags
/// `trusty-search start` no longer accepts (#9536) — one `WARN` line naming
/// the file to `warn` for each — then validate and expand `~/` paths. The
/// file is only read.
///
/// # Errors
///
/// When the file cannot be read or parsed, fails validation, or the home
/// directory cannot be resolved for tilde expansion.
///
/// Test: `legacy_search_entry_maps_to_socket_and_leaves_the_file_alone`,
/// `other_trusty_search_shapes_are_left_alone`,
/// `tm_1_8_search_start_no_http_is_stripped_on_load_and_the_file_is_untouched`.
pub fn load_user_manifest(path: &Path, warn: &mut dyn Write) -> anyhow::Result<ServicesManifest> {
    if !path.exists() {
        return Ok(ServicesManifest::default_manifest());
    }
    let text = std::fs::read_to_string(path)?;
    let mut manifest: ServicesManifest = serde_yaml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse services.yaml: {e}"))?;
    if map_legacy_search_entry(&mut manifest) {
        // A warning sink that cannot be written must not fail the load.
        let _ = writeln!(
            warn,
            "WARN: {}: trusty-search probes port {LEGACY_SEARCH_PORT} over HTTP, which it no \
             longer binds; probing its socket instead (health_probe: uds_search). The file is \
             unchanged; `tm services init --force` rewrites it.",
            path.display()
        );
    }
    // #9536: a 1.8.x `start --no-http` restart_cmd would stop the daemon and
    // then exit 2; strip the removed flags in memory, warn once.
    let removed = strip_removed_search_start_flags(&mut manifest);
    if !removed.is_empty() {
        let _ = writeln!(
            warn,
            "WARN: {}: trusty-search start no longer accepts {}; removed from its start_cmd and \
             restart_cmd for this run. The file is unchanged; `tm services init --force` \
             rewrites it.",
            path.display(),
            removed.join(", ")
        );
    }
    manifest
        .validate()
        .map_err(|e| anyhow::anyhow!("services.yaml validation failed: {e}"))?;
    manifest.expand_paths()?;
    Ok(manifest)
}

/// True for the trusty-search entry an older `tm services init` wrote: a
/// static HTTP probe of port 7878.
fn is_legacy_search_decl(decl: &ServiceDecl) -> bool {
    decl.health_probe == HealthProbe::Http
        && decl.port_discovery == PortDiscovery::Static
        && decl.default_port == Some(LEGACY_SEARCH_PORT)
        && decl
            .health_url
            .as_deref()
            .is_some_and(|u| u.starts_with("http://") || u.starts_with("https://"))
}

/// Map a legacy trusty-search entry to the socket probe; true when it did.
///
/// Any other trusty-search shape, and every other service, is left as written.
fn map_legacy_search_entry(manifest: &mut ServicesManifest) -> bool {
    match manifest.services.get_mut(SEARCH_SERVICE) {
        Some(decl) if is_legacy_search_decl(decl) => {
            decl.health_probe = HealthProbe::UdsSearch;
            decl.default_port = None;
            decl.health_url = None;
            true
        }
        _ => false,
    }
}

/// Shell operators that end one command inside a `start_cmd`/`restart_cmd`.
const SHELL_SEPARATORS: [&str; 5] = ["&&", "||", ";", "|", "&"];

/// Strip the flags `trusty-search start` no longer parses from the
/// trusty-search entry; returns each removed flag as written, deduplicated.
///
/// Why: `tm services init` on trusty-mpm 1.8.0–1.8.2 wrote
/// `trusty-search start --no-http` into `start_cmd` and `restart_cmd`.
/// trusty-search 0.60.0 rejects `--no-http` and `--port` with exit 2, so
/// `tm services restart trusty-search` would stop the daemon and fail to
/// start it (#9536).
/// What: on the trusty-search entry's `start_cmd` and `restart_cmd` only,
/// drops `--no-http`, `--port <n>` and `--port=<n>` when they are whole
/// tokens of a `trusty-search start` invocation. Every other token, command
/// and service is left byte-for-byte as written.
/// Test: `tm_1_8_search_start_no_http_is_stripped_on_load_and_the_file_is_untouched`,
/// `search_start_port_is_stripped_in_both_spellings_and_nothing_else_is`.
fn strip_removed_search_start_flags(manifest: &mut ServicesManifest) -> Vec<String> {
    let Some(decl) = manifest.services.get_mut(SEARCH_SERVICE) else {
        return Vec::new();
    };
    let mut removed: Vec<String> = Vec::new();
    for cmd in [&mut decl.start_cmd, &mut decl.restart_cmd]
        .into_iter()
        .flatten()
    {
        let (kept, dropped) = strip_search_start_flags(cmd);
        if dropped.is_empty() {
            continue;
        }
        *cmd = kept;
        for flag in dropped {
            if !removed.contains(&flag) {
                removed.push(flag);
            }
        }
    }
    removed
}

/// Byte spans of the whitespace-separated tokens of `cmd`.
fn token_spans(cmd: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in cmd.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, cmd.len()));
    }
    spans
}

/// True for a token naming the trusty-search binary, bare or by path.
fn is_search_bin(token: &str) -> bool {
    token == SEARCH_SERVICE || token.ends_with("/trusty-search")
}

/// `cmd` without the removed flags of each `trusty-search start` in it, plus
/// those flags as written. Removal takes the whitespace before each flag.
fn strip_search_start_flags(cmd: &str) -> (String, Vec<String>) {
    let spans = token_spans(cmd);
    let tok = |i: usize| &cmd[spans[i].0..spans[i].1];
    // Inclusive (first, last) token ranges to drop, in order.
    let mut drops = Vec::new();
    let mut in_start = false;
    let mut i = 0;
    while i < spans.len() {
        let t = tok(i);
        if SHELL_SEPARATORS.contains(&t) {
            in_start = false;
        } else if is_search_bin(t) && i + 1 < spans.len() && tok(i + 1) == "start" {
            in_start = true;
            i += 1;
        } else if in_start && (t == "--no-http" || t.starts_with("--port=")) {
            drops.push((i, i));
        } else if in_start && t == "--port" {
            // The value is the next token unless that is a flag or an operator.
            let has_value = i + 1 < spans.len()
                && !tok(i + 1).starts_with('-')
                && !SHELL_SEPARATORS.contains(&tok(i + 1));
            let last = if has_value { i + 1 } else { i };
            drops.push((i, last));
            i = last;
        }
        i += 1;
    }
    let mut kept = String::with_capacity(cmd.len());
    let mut removed = Vec::with_capacity(drops.len());
    let mut from = 0;
    for (first, last) in drops {
        let cut = cmd[..spans[first].0].trim_end().len();
        kept.push_str(&cmd[from..cut]);
        from = spans[last].1;
        removed.push(cmd[spans[first].0..from].to_string());
    }
    kept.push_str(&cmd[from..]);
    (kept, removed)
}
