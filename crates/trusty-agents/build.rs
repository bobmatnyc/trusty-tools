//! Build script exposing git commit metadata to the binary and ensuring the
//! embedded UI bundle directory always exists.
//!
//! Why: Users of the binary benefit from knowing which exact commit produced
//! the running artifact; pairing `CARGO_PKG_VERSION` with the short git SHA
//! gives a deterministic identifier for bug reports and log correlation. The
//! script also guarantees `ui/dist/` exists with at least a stub
//! `index.html` so the `#[derive(RustEmbed)] #[folder = "ui/dist/"]` macro
//! in `src/api/server.rs` always finds a directory to scan — without this,
//! a missing `ui/dist/` (e.g. fresh clone with no pnpm installed) yields the
//! 3 compile errors tracked in #112.
//! What: Queries `git rev-parse --short HEAD` at build time and exposes it
//! as the `GIT_COMMIT_HASH` environment variable to the compiled crate via
//! `cargo:rustc-env`. Falls back to `"unknown"` when git isn't available or
//! the directory isn't a git repo. Then attempts a real `pnpm build` of the
//! Svelte UI; if pnpm is unavailable or `SKIP_UI_BUILD=1`, falls back to
//! writing a placeholder `ui/dist/index.html` so `rust-embed` still compiles.
//! Test: After `cargo build`, `build_info::GIT_HASH` should be non-empty and
//! either a 7-char short hash or `"unknown"`. `cargo check -p trusty-agents` must
//! succeed on a host without pnpm installed (regression coverage for #112).

use std::path::Path;
use std::process::Command;

fn main() {
    // Re-run whenever HEAD moves so a new commit triggers a rebuild.
    println!("cargo:rerun-if-changed=.git/HEAD");

    // Re-run whenever the built UI bundle changes so rust-embed re-inlines fresh
    // assets. We watch `ui/dist/index.html` (the output) rather than `ui/src/`
    // because cargo's `rerun-if-changed` only tracks the directory inode, not
    // recursive file mutations — edits to `.svelte`/`.ts` files inside `ui/src/`
    // wouldn't trigger a rebuild and stale assets would stay embedded. Watching
    // the build output is reliable: every `pnpm build` regenerates `index.html`,
    // and the `pnpm build` invocation below runs on every cargo build anyway,
    // so cargo will pick up the resulting change on the subsequent build cycle.
    println!("cargo:rerun-if-changed=ui/dist/index.html");
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/package.json");
    println!("cargo:rerun-if-env-changed=SKIP_UI_BUILD");

    ensure_ui_dist();
    emit_git_hash();
}

/// Run the Svelte UI build (or fall back to a placeholder `ui/dist/index.html`).
///
/// Why: `src/api/server.rs` declares `#[derive(rust_embed::RustEmbed)]
/// #[folder = "ui/dist/"]`, which requires the folder to exist at compile
/// time. If pnpm is missing or `SKIP_UI_BUILD=1` is set, we still need
/// *something* under `ui/dist/` or the derive macro fails the build with the
/// 3 errors from #112. The placeholder lets `cargo check` and `cargo build`
/// succeed without the JS toolchain installed — UI features simply return
/// "UI not built" at runtime instead of breaking the entire workspace.
/// What: When `SKIP_UI_BUILD=1` or pnpm is unavailable, writes a stub
/// `ui/dist/index.html`. Otherwise runs `pnpm install` (if `node_modules`
/// is missing) followed by `pnpm build`. If `pnpm build` fails for any
/// reason, falls back to the placeholder so the Rust build still completes.
/// Test: With pnpm uninstalled, `cargo check -p trusty-agents` must succeed.
/// With pnpm installed, `ui/dist/index.html` must contain Vite's real output
/// (look for a `<script type="module">` tag referencing `assets/`).
fn ensure_ui_dist() {
    let ui_dist = Path::new("ui/dist");

    if std::env::var("SKIP_UI_BUILD").as_deref() == Ok("1") {
        println!("cargo:warning=SKIP_UI_BUILD=1 — web UI build skipped");
        ensure_placeholder(ui_dist);
        return;
    }

    if Command::new("pnpm").arg("--version").output().is_err() {
        println!("cargo:warning=pnpm not found — web UI will not be embedded");
        ensure_placeholder(ui_dist);
        return;
    }

    if !Path::new("ui/node_modules").exists() {
        // Use --no-frozen-lockfile when pnpm-lock.yaml is absent (e.g.
        // inside a cargo publish verification sandbox where only
        // git-tracked files are present).
        let lockfile_exists = Path::new("ui/pnpm-lock.yaml").exists();
        let install_args: &[&str] = if lockfile_exists {
            &["install", "--frozen-lockfile"]
        } else {
            &["install", "--no-frozen-lockfile"]
        };
        match Command::new("pnpm")
            .args(install_args)
            .current_dir("ui")
            .status()
        {
            Ok(s) if s.success() => {}
            Ok(s) => {
                println!("cargo:warning=pnpm install exited with {s:?} — embedding placeholder UI");
                ensure_placeholder(ui_dist);
                return;
            }
            Err(e) => {
                println!(
                    "cargo:warning=failed to spawn pnpm install ({e}) — embedding placeholder UI"
                );
                ensure_placeholder(ui_dist);
                return;
            }
        }
    }

    match Command::new("pnpm").arg("build").current_dir("ui").status() {
        Ok(s) if s.success() => {
            // Real build succeeded — `ui/dist/` now has Vite's output.
        }
        Ok(s) => {
            println!("cargo:warning=pnpm build exited with {s:?} — embedding placeholder UI");
            ensure_placeholder(ui_dist);
        }
        Err(e) => {
            println!("cargo:warning=failed to spawn pnpm build ({e}) — embedding placeholder UI");
            ensure_placeholder(ui_dist);
        }
    }
}

/// Write a stub `ui/dist/index.html` when the real UI build was skipped.
///
/// Why: `rust-embed` walks the folder at compile time and fails the derive
/// macro if the directory is missing. A single-file stub is the smallest
/// thing that satisfies the macro while making the "UI not built" state
/// obvious to anyone who navigates to `/` in a browser.
/// What: Creates `ui/dist/` if it doesn't exist and writes a minimal HTML
/// document explaining how to enable the real UI. Idempotent — if a real
/// `index.html` is already present (i.e. the Svelte build succeeded earlier
/// in this `cargo build`), it is left untouched.
/// Test: After running with `SKIP_UI_BUILD=1`, `ui/dist/index.html` must
/// exist and contain the literal string "UI not built".
fn ensure_placeholder(ui_dist: &Path) {
    if ui_dist.join("index.html").exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(ui_dist) {
        println!("cargo:warning=failed to create {}: {e}", ui_dist.display());
        return;
    }
    let stub = "<!doctype html><html><body><p>trusty-agents: UI not built. \
                Install pnpm and rebuild (or unset SKIP_UI_BUILD) to embed \
                the Svelte frontend.</p></body></html>";
    if let Err(e) = std::fs::write(ui_dist.join("index.html"), stub) {
        println!(
            "cargo:warning=failed to write {}/index.html: {e}",
            ui_dist.display()
        );
    }
}

/// Capture the short git SHA at build time and expose it via `cargo:rustc-env`.
///
/// Why: Bug reports and log lines need a deterministic identifier for the
/// running binary — `CARGO_PKG_VERSION` alone collapses every commit on a
/// dev branch into the same version string.
/// What: Runs `git rev-parse --short HEAD`, exposes the result as
/// `GIT_COMMIT_HASH`, or falls back to `"unknown"` if git is unavailable.
/// Test: After `cargo build`, the compiled binary's startup banner should
/// include either a 7-char hash or the literal `"unknown"`.
fn emit_git_hash() {
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT_HASH={git_hash}");
}
