//! `tm whats-new` and the `tm doctor` notice that points at it (#11003).
//!
//! Why: the selection, store and notice decision live in
//! `trusty_mpm::core::whats_new`; this file only prints.
//! What: [`run`] prints the installed release's highlights; [`print_doctor_notice`]
//! prints the once-per-release line and reports a store failure on stderr.
//! Test: `tests/tm_whats_new.rs`.

use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::whats_new::{self, SeenStore, StoreError};

/// `tm whats-new`: print the highlights for this binary's version.
pub(crate) fn run() -> anyhow::Result<()> {
    print!("{}", whats_new::render(env!("CARGO_PKG_VERSION")));
    Ok(())
}

/// Print the doctor notice when this release has not been announced yet.
///
/// Fail-open: a store failure prints the notice and a stderr warning, records
/// nothing, and never fails doctor. The root resolves fail-closed
/// (`try_default`), so a broken `install.toml` never writes under `$HOME`.
/// Test: `tm_doctor_shows_the_whats_new_notice_once`,
/// `tm_doctor_reports_an_unreadable_whats_new_store`.
pub(crate) fn print_doctor_notice() {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .ok()
        .and_then(|v| whats_new::for_version(&v));
    let store = FrameworkPaths::try_default()
        .map(|paths| SeenStore::in_root(&paths.root))
        .map_err(|e| StoreError::Unresolved(e.to_string()));
    let outcome = whats_new::doctor_notice(store, current.as_ref());
    if let Some(line) = outcome.line {
        println!("\n{line}");
    }
    if let Some(err) = outcome.warning {
        eprintln!("tm: whats-new notice not recorded as seen ({err}); it shows again next run");
    }
}
