//! Load the user's `~/.claude-mpm/services.yaml`, mapping a retired
//! trusty-search entry on read (#9543).
//!
//! Why: `tm services init` wrote trusty-search as a static HTTP probe of port
//! 7878. trusty-search 0.59.0 binds no TCP port, so that entry would report a
//! live daemon DOWN. The file belongs to the user: it is mapped in memory, with
//! one warning, and never rewritten.
//! What: [`load_user_manifest`] parses, maps, validates and tilde-expands one
//! file, and falls back to the embedded default when the file is absent.
//! Test: `legacy_search_entry_maps_to_socket_and_leaves_the_file_alone`,
//! `other_trusty_search_shapes_are_left_alone`.

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
/// trusty-search entry to `health_probe: uds_search` (writing one `WARN` line
/// naming the file to `warn`), validate, and expand `~/` paths. The file is
/// only read.
///
/// # Errors
///
/// When the file cannot be read or parsed, fails validation, or the home
/// directory cannot be resolved for tilde expansion.
///
/// Test: `legacy_search_entry_maps_to_socket_and_leaves_the_file_alone`,
/// `other_trusty_search_shapes_are_left_alone`.
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
