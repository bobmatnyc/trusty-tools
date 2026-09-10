//! Disk accounting for the Disk dashboard (DOC-73 §16).
//!
//! Why: DOC-73 §16.6 item 1 makes a cached byte figure the prerequisite for
//! every other Disk issue — the sunburst spans many projects and worktrees at
//! hundreds of GB to TiB, and neither existing primitive is an index
//! (`trusty-common`'s `dir_size_bytes` and this crate's `measure_bytes_until`
//! both re-stat every file on every call). This module is where that index
//! lives, and where the Disk survey MCP tool (#6927) and the console routes
//! (§16.5) read their bytes from.
//! What: [`size_index::DirSizeIndex`], a path → bytes-with-timestamp cache
//! whose refresh re-reads only the directories whose contents actually
//! changed.
//! Test: `size_index_tests`.
//!
//! # Why trusty-mpm and not trusty-common (#6926)
//!
//! The issue hedged "likely in `trusty-common` alongside `dir_size_bytes`".
//! It lives here instead because §16.1 names trusty-mpm as the Disk
//! dashboard's data source (project registry, worktree discovery) and §16.4
//! states console reaches trusty-mpm only through MCP tools — so the index has
//! exactly one caller, and it is in this crate. CLAUDE.md's common-entry-point
//! rule scopes itself to capabilities shared ACROSS crates; promoting this one
//! before a second crate calls it would buy a published-crate version bump and
//! a workspace-wide gate for sharing that does not exist yet. Moving the module
//! to `trusty-common` is a file move plus a re-export the day a second crate
//! needs it.
//!
//! [`size_index::DirSizeIndex`]: crate::disk::size_index::DirSizeIndex

pub mod size_index;
// #6927: the Disk dashboard's worktree survey — the classification the
// `disk_survey` MCP tool returns, and the run that gathers its facts. Crate
// -internal: every type in it names a `pub(crate)` reclaim type, and the MCP
// tool is the only consumer.
pub(crate) mod survey;
pub(crate) mod survey_run;

/// Render a byte count the way an operator reads a disk figure.
///
/// Why: "1180591620717411303424" is not an early warning. The 1.1 TiB in the
/// 2026-07-21 post-mortem is only legible in binary units. It lives here, on
/// the module that owns disk accounting, rather than in
/// [`crate::daemon::doctor_worktree_disk`] where it started (#7313): `tm
/// session disk` renders the same figures from the `tm` binary, which is a
/// separate crate from the library and so cannot reach a private daemon
/// helper. One formatter, so the doctor row and the CLI never disagree about
/// what 1.1 TiB looks like.
/// What: binary units (1024-based), one decimal place above KiB.
/// Test: `human_bytes_renders_binary_units`.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn human_bytes_renders_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3), "3.0 MiB");
        // The figure from the 2026-07-21 post-mortem must render as terabytes,
        // not as an unreadable integer.
        assert_eq!(human_bytes(1_209_462_790_553), "1.1 TiB");
    }
}
