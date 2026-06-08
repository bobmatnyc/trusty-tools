//! build.rs — build the UI before compiling so rust-embed has assets.
//!
//! Why: The web console ships an embedded Svelte SPA; the source lives in
//! `ui/src/`, and Vite produces the static bundle in `ui/dist/`. Running
//! `pnpm build` here means a plain `cargo build` always produces a binary
//! with up-to-date assets, with no separate UI build step.
//! What: Skips entirely if `SKIP_UI_BUILD=1` (CI / first-time bootstrap
//! when pnpm is unavailable). Otherwise runs `pnpm install` (frozen lockfile
//! if present) followed by `pnpm build` in `ui/`. Emits cargo:rerun
//! directives so a `cargo build` only re-runs the JS pipeline when UI
//! sources change.
//! Test: `SKIP_UI_BUILD=1 cargo check` exits without invoking pnpm; a normal
//! `cargo build` populates `ui/dist/index.html`.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let ui_dir = Path::new(&manifest_dir).join("ui");
    let dist_dir = ui_dir.join("dist");
    let pkg_json = ui_dir.join("package.json");

    println!("cargo:rerun-if-env-changed=SKIP_UI_BUILD");
    println!("cargo:rerun-if-changed=ui/package.json");
    println!("cargo:rerun-if-changed=ui/vite.config.js");
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/src");

    if std::env::var("SKIP_UI_BUILD").as_deref() == Ok("1") {
        println!("cargo:warning=SKIP_UI_BUILD=1 — skipping UI build");
        ensure_dist_placeholder(&dist_dir);
        return;
    }

    if !pkg_json.exists() {
        println!("cargo:warning=ui/package.json missing — skipping UI build");
        ensure_dist_placeholder(&dist_dir);
        return;
    }

    if which("pnpm").is_none() {
        println!(
            "cargo:warning=pnpm not found on PATH — skipping UI build \
             (set SKIP_UI_BUILD=1 to silence)"
        );
        ensure_dist_placeholder(&dist_dir);
        return;
    }

    let lockfile = ui_dir.join("pnpm-lock.yaml");
    let install_args: Vec<&str> = if lockfile.exists() {
        vec!["install", "--frozen-lockfile"]
    } else {
        vec!["install"]
    };
    let install_status = Command::new("pnpm")
        .args(&install_args)
        .current_dir(&ui_dir)
        .status();
    match install_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!("cargo:warning=pnpm install failed with status {s:?}");
            ensure_dist_placeholder(&dist_dir);
            return;
        }
        Err(e) => {
            println!("cargo:warning=failed to spawn pnpm install: {e}");
            ensure_dist_placeholder(&dist_dir);
            return;
        }
    }

    let build_status = Command::new("pnpm")
        .args(["build"])
        .current_dir(&ui_dir)
        .status();
    match build_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!("cargo:warning=pnpm build failed with status {s:?}");
            ensure_dist_placeholder(&dist_dir);
        }
        Err(e) => {
            println!("cargo:warning=failed to spawn pnpm build: {e}");
            ensure_dist_placeholder(&dist_dir);
        }
    }
}

/// Ensure `ui/dist/` exists with at least an index.html so rust-embed can
/// embed *something* — a missing folder makes the include fail at compile time.
///
/// Why: When SKIP_UI_BUILD=1 or pnpm is absent, `ui/dist/` may not exist yet;
/// rust-embed requires the folder to be present at compile time.
/// What: Creates `ui/dist/` and writes a minimal placeholder `index.html`.
/// Test: `SKIP_UI_BUILD=1 cargo check` must pass with this placeholder.
fn ensure_dist_placeholder(dist_dir: &Path) {
    if dist_dir.join("index.html").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dist_dir);
    let _ = std::fs::write(
        dist_dir.join("index.html"),
        "<!doctype html><html><body><p>trusty-console: UI assets not built. \
         Run <code>pnpm --dir ui install &amp;&amp; pnpm --dir ui build</code> \
         and rebuild.</p></body></html>",
    );
}

/// Cheap which() — avoid a heavy dep just for build.rs.
fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
        for ext in ["cmd", "exe"] {
            let with_ext = dir.join(format!("{cmd}.{ext}"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}
