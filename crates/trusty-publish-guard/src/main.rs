//! `publish-guard` — CLI wrapper around the trusty-publish-guard library
//! (issue #3366).
//!
//! Walks every `crates/*/Cargo.toml`, skips crates marked `publish = false`,
//! and for each remaining crate checks whether its CURRENT version is
//! already live on crates.io with source that matches the local working
//! tree. Exits non-zero (and prints a `[FAIL]` block per offending crate) the
//! moment ANY crate shows drift or ANY crate's status could not be verified —
//! this check fails closed and loud by design; see the module docs in
//! `lib.rs` for why.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use trusty_publish_guard::fetch::CratesIoFetcher;
use trusty_publish_guard::{ParityStatus, check_crate};

/// Detects unpublished source drift under an already-published crate
/// version (issue #3366).
#[derive(Parser)]
#[command(name = "publish-guard", version)]
struct Cli {
    /// Workspace root containing a `crates/` directory. Defaults to the
    /// current directory.
    #[arg(long)]
    root: Option<PathBuf>,
}

struct CrateManifest {
    /// Directory name under `crates/` (e.g. `trusty-git-analytics`).
    dir: String,
    /// The crates.io package name from `[package] name` (e.g. `tga`) —
    /// usually equal to `dir`, but not always.
    name: String,
    version: String,
    publish: bool,
}

/// Minimum number of publishable crates a real run must have checked.
///
/// Why: the exit code depends only on `drifted == 0 && unverifiable == 0`, so a
/// run that discovered no crates prints `checked 0 publishable crate(s) — 0
/// drifted, 0 unverifiable.` and exits 0 — a version-parity gate that cannot
/// fail (issue #4618). `checked` is the number that separates "compared every
/// crate against crates.io" from "compared nothing", so it is floored.
/// Deliberately far below the current publishable set and far above zero.
/// What: compared against `checked` via [`scan_floor_violation`].
/// Test: `tests::floor_rejects_empty_scan`.
const MIN_CHECKED_CRATES: usize = 10;

/// Reports why a run's crate coverage is too low to be trusted, if it is.
///
/// Why: keeps the floor a pure, unit-testable predicate instead of an inline
/// `if` in `main` that only CI can exercise (issue #4618).
/// What: `Some(message)` when fewer than [`MIN_CHECKED_CRATES`] crates were
/// checked, `None` otherwise.
/// Test: `tests::floor_rejects_empty_scan`, `tests::floor_accepts_real_scan`.
fn scan_floor_violation(checked: usize) -> Option<String> {
    if checked >= MIN_CHECKED_CRATES {
        return None;
    }
    Some(format!(
        "publish-guard: SCAN FLOOR — checked {checked} publishable crate(s), below the declared \
         minimum of {MIN_CHECKED_CRATES}. A run that discovered no crates reports 0 drifted and \
         exits 0; that is a broken scan, not a clean workspace (issue #4618). Check --root, or \
         lower the floor in main.rs on purpose."
    ))
}

/// Parses `publish = ...` from a `[package]` toml table. Cargo accepts either
/// a bool or an array of allowed registries; anything other than the literal
/// `false` means the crate CAN reach crates.io and is in scope for this
/// check. Absent `publish` defaults to publishable (Cargo's own default).
fn parse_publish(package: &toml::Value) -> bool {
    match package.get("publish") {
        Some(toml::Value::Boolean(b)) => *b,
        _ => true,
    }
}

fn discover_crates(crates_dir: &Path) -> Result<Vec<CrateManifest>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(crates_dir)
        .with_context(|| format!("reading {}", crates_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut out = Vec::new();
    for dir in dirs {
        let manifest_path = dir.join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let value: toml::Value = text
            .parse()
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        let Some(package) = value.get("package") else {
            continue;
        };
        let (Some(name), Some(version)) = (
            package.get("name").and_then(|v| v.as_str()),
            package.get("version").and_then(|v| v.as_str()),
        ) else {
            continue;
        };

        out.push(CrateManifest {
            dir: dir
                .file_name()
                .expect("read_dir entries always have a file name")
                .to_string_lossy()
                .to_string(),
            name: name.to_string(),
            version: version.to_string(),
            publish: parse_publish(package),
        });
    }
    Ok(out)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| PathBuf::from("."));
    let crates_dir = root.join("crates");

    let manifests = discover_crates(&crates_dir)?;
    let fetcher = CratesIoFetcher::new()?;

    let mut drifted = 0usize;
    let mut unverifiable = 0usize;
    let mut checked = 0usize;

    for m in manifests.iter().filter(|m| m.publish) {
        let crate_root = crates_dir.join(&m.dir);
        checked += 1;
        match check_crate(&fetcher, &crate_root, &m.name, &m.version) {
            Ok(ParityStatus::NotYetPublished) => {
                println!(
                    "[ OK ] {} {} — not yet published, nothing to compare",
                    m.name, m.version
                );
            }
            Ok(ParityStatus::Parity) => {
                println!(
                    "[ OK ] {} {} — source matches published crates.io tarball",
                    m.name, m.version
                );
            }
            Ok(ParityStatus::Drift(entries)) => {
                drifted += 1;
                println!(
                    "[FAIL] {} {} — UNPUBLISHED SOURCE DRIFT under an already-published version:",
                    m.name, m.version
                );
                for entry in &entries {
                    println!("       {entry}");
                }
                println!(
                    "       Fix: bump the version in crates/{}/Cargo.toml (this is the #3366 defect).",
                    m.dir
                );
            }
            Err(err) => {
                unverifiable += 1;
                eprintln!(
                    "[ERR ] {} {} — could not verify parity: {err:#}",
                    m.name, m.version
                );
            }
        }
    }

    println!();
    println!(
        "publish-guard: checked {checked} publishable crate(s) (floor {MIN_CHECKED_CRATES}) — {drifted} drifted, {unverifiable} unverifiable."
    );

    if let Some(msg) = scan_floor_violation(checked) {
        eprintln!("{msg}");
        std::process::exit(1);
    }

    if drifted > 0 || unverifiable > 0 {
        eprintln!(
            "publish-guard: FAILED. An unverifiable crate is treated as a failure, not a pass \
             (fail-closed by design — see issue #3366)."
        );
        std::process::exit(1);
    }

    println!("publish-guard: OK — no version-parity drift detected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MIN_CHECKED_CRATES, scan_floor_violation};

    /// A run that compared no crates must fail, not report OK (issue #4618).
    #[test]
    fn floor_rejects_empty_scan() {
        let msg = scan_floor_violation(0).expect("an empty scan violates the floor");
        assert!(
            msg.contains("SCAN FLOOR"),
            "message names the failure: {msg}"
        );
        assert!(
            scan_floor_violation(MIN_CHECKED_CRATES - 1).is_some(),
            "one below the floor still violates it"
        );
    }

    /// A run at or above the declared minimum passes the floor.
    #[test]
    fn floor_accepts_real_scan() {
        assert!(scan_floor_violation(MIN_CHECKED_CRATES).is_none());
        assert!(scan_floor_violation(1_000).is_none());
    }
}
