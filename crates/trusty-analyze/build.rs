//! build.rs — build the UI before compiling so rust-embed has assets.
//!
//! Why: The web admin panel ships embedded in the binary via rust-embed; the
//! source of truth is `ui/src/`, and Vite produces the static bundle in
//! `ui/dist/`. Running `pnpm build` here means a plain `cargo build` always
//! produces a binary with up-to-date assets, with no separate UI build step.
//! What: Skips entirely if `SKIP_UI_BUILD=1` (CI / first-time bootstrap when
//! pnpm is unavailable). Otherwise runs `<pm> install [--frozen-lockfile]`
//! followed by `<pm> run build` in `ui/`. Emits cargo:rerun directives so a
//! `cargo build` only re-runs the JS pipeline when UI sources change. After a
//! successful build it re-stamps `ui/dist/ui-source-hash.txt`, which Vite's
//! `emptyOutDir` has just deleted (#5936).
//!
//! NOTE: The core UI-build logic (SKIP_UI_BUILD guard, pnpm detection,
//! install+build pipeline, placeholder fallback) is intentionally kept
//! identical across trusty-memory, trusty-analyze, trusty-console, and
//! trusty-search (issue #987). `scripts/check_buildrs_sync.sh` asserts that
//! the canonical implementation block does not drift between these four files.
//!
//! Test: `SKIP_UI_BUILD=1 cargo check -p trusty-analyze` exits without invoking
//! pnpm; a normal `cargo build` populates `ui/dist/index.html`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let crate_root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let ui_dir = crate_root.join("ui");
    let dist_dir = ui_dir.join("dist");

    println!("cargo:rerun-if-env-changed=SKIP_UI_BUILD");
    println!("cargo:rerun-if-changed=ui/package.json");
    println!("cargo:rerun-if-changed=ui/vite.config.js");
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/src");

    // #5936: `pnpm run build` runs with `emptyOutDir`, which clears ui/dist/
    // and takes the committed ui-source-hash.txt with it. Re-stamp whenever a
    // build actually ran, so the deletion is never left staged.
    if build_svelte_ui(&ui_dir, &dist_dir, "trusty-analyze") {
        restamp_ui_bundle(&crate_root, "trusty-analyze");
    }
}

// ── CANONICAL BLOCK BEGIN (kept in sync by scripts/check_buildrs_sync.sh) ──

/// Run the Svelte UI build pipeline, or degrade gracefully to a placeholder.
///
/// Why: Centralises SKIP_UI_BUILD handling, pnpm detection, frozen-lockfile
/// install, and placeholder fallback so all four UI-embedding crates share
/// identical logic without a published build-helper crate (#987).
/// What: Checks SKIP_UI_BUILD, detects pnpm/npm, runs install + build inside
/// `ui_dir`, writes a placeholder on any failure so the Rust build still
/// completes even without the JS toolchain. Returns `true` only when Vite
/// actually rebuilt the bundle, which is the caller's signal to re-stamp —
/// see `restamp_ui_bundle` (#5936).
/// Test: `SKIP_UI_BUILD=1 cargo check` short-circuits; `cargo build` with pnpm
/// installed populates `dist_dir/index.html` with real Vite output.
fn build_svelte_ui(ui_dir: &Path, dist_dir: &Path, crate_name: &str) -> bool {
    // Step 1: honour explicit skip (CI / `cargo publish --verify`).
    if std::env::var("SKIP_UI_BUILD").as_deref() == Ok("1") {
        if !dist_dir.join("index.html").exists() {
            println!(
                "cargo:warning=SKIP_UI_BUILD=1 but {dist}/ is empty — \
                 run `pnpm --dir ui install && pnpm --dir ui build` before publishing.",
                dist = dist_dir.display()
            );
            ensure_placeholder(dist_dir, crate_name);
        }
        return false;
    }

    // Step 2: no `ui/package.json` means we are inside an extracted tarball
    // that already shipped the dist — nothing to build.
    if !ui_dir.join("package.json").exists() {
        ensure_placeholder(dist_dir, crate_name);
        return false;
    }

    // Step 3: detect package manager (pnpm preferred, npm fallback).
    let Some(pm) = detect_pm() else {
        println!(
            "cargo:warning={crate_name}: no pnpm/npm on PATH — skipping UI \
             build (set SKIP_UI_BUILD=1 to silence, or install pnpm)."
        );
        ensure_placeholder(dist_dir, crate_name);
        return false;
    };

    // Step 4a: install — prefer frozen lockfile when pnpm-lock.yaml exists.
    // Only pass --frozen-lockfile when pnpm is the detected manager; npm does
    // not support that flag (it uses --ci instead) and will error out.
    let mut install_args = vec!["install"];
    if pm == "pnpm" && ui_dir.join("pnpm-lock.yaml").exists() {
        install_args.push("--frozen-lockfile");
    }
    let install_ok = Command::new(pm)
        .args(&install_args)
        .current_dir(ui_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !install_ok {
        println!("cargo:warning={crate_name}: `{pm} install` failed — embedding placeholder UI.");
        ensure_placeholder(dist_dir, crate_name);
        return false;
    }

    // Step 4b: build.
    let build_ok = Command::new(pm)
        .args(["run", "build"])
        .current_dir(ui_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !build_ok {
        println!("cargo:warning={crate_name}: `{pm} run build` failed — embedding placeholder UI.");
        ensure_placeholder(dist_dir, crate_name);
        return false;
    }
    true
}

/// Rewrite the bundle's `ui-source-hash.txt` after Vite has rebuilt it.
///
/// Why: every bundle-shipping crate sets `build.emptyOutDir` in its
/// `vite.config.js`, so `pnpm run build` clears the output directory — which
/// deletes the committed stamp that `scripts/check-ui-bundle-freshness.sh`
/// reads. Nothing chained the rebuild to a re-stamp, so an ordinary
/// `cargo build` left the stamp staged for deletion and it rode into unrelated
/// commits (#5936). The polarity is the reverse of the obvious guess: a host
/// with no pnpm never reaches the build and never deletes anything.
/// What: shells out to `scripts/stamp-ui-bundle.sh <crate>`, the one writer of
/// that file. A no-op when the script is unreachable — a published crate
/// tarball has no `scripts/` and already ships a stamped bundle.
/// Test: `scripts/check_buildrs_sync.sh` requires every crate in
/// `scripts/ui-bundle-manifest.tsv` to call this; the CI gate
/// `scripts/check-ui-bundle-freshness.sh` fails STAMP-MISSING if it stops
/// running.
fn restamp_ui_bundle(crate_root: &Path, crate_name: &str) {
    let Ok(repo_root) = crate_root.join("..").join("..").canonicalize() else {
        return;
    };
    let script = repo_root.join("scripts").join("stamp-ui-bundle.sh");
    if !script.exists() {
        return;
    }
    let ok = Command::new("bash")
        .arg(&script)
        .arg("--repo")
        .arg(&repo_root)
        .arg(crate_name)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        println!(
            "cargo:warning={crate_name}: UI bundle rebuilt but re-stamping failed — \
             run `bash scripts/stamp-ui-bundle.sh {crate_name}` before committing."
        );
    }
}

/// Write a stub `index.html` so embed macros compile without the JS build.
///
/// Why: `rust_embed` and `include_dir!` fail at compile time if the referenced
/// directory is absent or empty; a single-file stub is the minimum viable
/// artefact that satisfies both macros while making the "UI not built" state
/// obvious to anyone who opens `/` in a browser.
/// What: Creates `dist_dir` if needed and writes a minimal HTML document.
/// Idempotent — exits immediately if `index.html` already exists.
/// Test: After `SKIP_UI_BUILD=1 cargo build`, `dist_dir/index.html` exists.
fn ensure_placeholder(dist_dir: &Path, crate_name: &str) {
    if dist_dir.join("index.html").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dist_dir);
    let html = format!(
        "<!doctype html><html><body><p>{crate_name}: UI assets not built. \
         Run <code>pnpm --dir ui install &amp;&amp; pnpm --dir ui build</code> \
         and rebuild.</p></body></html>"
    );
    let _ = std::fs::write(dist_dir.join("index.html"), html);
}

/// Detect the available Node.js package manager on PATH.
///
/// Why: The workspace uses pnpm (lockfile is `pnpm-lock.yaml`), but `npm`
/// works as a fallback on machines that have Node but not pnpm separately.
/// What: Probes `pnpm --version` then `npm --version`; returns the first
/// that exits 0, or `None` when neither is found.
/// Test: `detect_pm()` returns `Some("pnpm")` on a standard dev machine;
/// `Some("npm")` on a machine without pnpm; `None` in a bare container.
fn detect_pm() -> Option<&'static str> {
    let ok = |bin: &str| {
        Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if ok("pnpm") {
        Some("pnpm")
    } else if ok("npm") {
        Some("npm")
    } else {
        None
    }
}

// ── CANONICAL BLOCK END ──
