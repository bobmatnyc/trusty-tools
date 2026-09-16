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
//! What: Queries git at build time and exposes the results to the compiled
//! crate via `cargo:rustc-env` — `GIT_COMMIT_HASH` (short SHA),
//! `GIT_COMMIT_HASH_FULL`, `GIT_COMMIT_DATE` (strict ISO 8601 committer
//! date), and `GIT_DIRTY` (`"1"` when the working tree had uncommitted or
//! untracked changes at build time). Falls back to `"unknown"` when git isn't
//! available or the directory isn't a git repo. Then attempts a real `pnpm build` of the
//! Svelte UI; if pnpm is unavailable or `SKIP_UI_BUILD=1`, falls back to
//! writing a placeholder `ui/dist/index.html` so `rust-embed` still compiles.
//! #8094: a pnpm step that RUNS and fails now aborts the cargo build instead
//! of embedding that placeholder.
//! Test: After `cargo build`, `build_info::GIT_HASH` should be non-empty and
//! either a 7-char short hash or `"unknown"`. `cargo check -p trusty-agents` must
//! succeed on a host without pnpm installed (regression coverage for #112).
//! The UI-build policy itself is covered by
//! `crates/trusty-agents/tests/build_ui_policy.rs`.

use std::path::Path;
use std::process::Command;

// #8094: the pure half of the UI-build decision, shared verbatim with the test
// target that covers it. See `build_ui_policy.rs` for why it is `include!`d.
include!("build_ui_policy.rs");

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

    // #8094: this script emits `rerun-if-changed`, which switches cargo off its
    // whole-package default, so the `include!`d policy file must be named or an
    // edit to it would not rebuild the script.
    println!("cargo:rerun-if-changed=build_ui_policy.rs");

    // #4260: watch the index too so a `git add` refreshes the dirty flag.
    println!("cargo:rerun-if-changed=.git/index");

    ensure_ui_dist();
    emit_git_provenance();
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
/// What: [`placeholder_reason`] decides whether the stub is legitimate —
/// `SKIP_UI_BUILD=1`, or a host with no pnpm — and a `cargo:warning=` names
/// which of the two applied. Otherwise runs `pnpm install` (if `node_modules`
/// is missing) followed by `pnpm build`, and #8094: a step that runs and fails
/// aborts the cargo build through [`fail_build`] rather than quietly embedding
/// the placeholder.
/// Test: With pnpm uninstalled, `cargo check -p trusty-agents` must succeed.
/// With pnpm installed, `ui/dist/index.html` must contain Vite's real output
/// (look for a `<script type="module">` tag referencing `assets/`). The
/// decision itself: `a_failing_pnpm_build_aborts_the_cargo_build`,
/// `skip_ui_build_names_the_opt_out`.
fn ensure_ui_dist() {
    let ui_dist = Path::new("ui/dist");

    let skip_ui_build = std::env::var("SKIP_UI_BUILD").as_deref() == Ok("1");
    let pnpm_available = Command::new("pnpm").arg("--version").output().is_ok();
    if let Some(reason) = placeholder_reason(skip_ui_build, pnpm_available) {
        println!("cargo:warning={reason}");
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
        let installed = Command::new("pnpm")
            .args(install_args)
            .current_dir("ui")
            .status();
        if let Err(reason) = classify_pnpm_step("pnpm install", installed) {
            fail_build(&reason);
        }
    }

    let built = Command::new("pnpm").arg("build").current_dir("ui").status();
    if let Err(reason) = classify_pnpm_step("pnpm build", built) {
        fail_build(&reason);
    }
}

/// Abort the cargo build, naming the pnpm failure that caused it (#8094).
///
/// Why: a build script that returns normally is a build that "succeeded", and
/// that is how a failed `pnpm build` used to ship a placeholder page inside an
/// installed binary. Panicking is what makes `cargo build`/`cargo install`
/// fail.
/// What: emits `reason` as a `cargo:warning=` — visible even in the summary
/// cargo prints for a failed script — and then panics with it. pnpm's own
/// stderr was inherited by this script, so cargo prints that too.
/// Test: `a_failing_pnpm_build_aborts_the_cargo_build` covers the decision
/// that reaches here.
fn fail_build(reason: &str) -> ! {
    println!("cargo:warning={reason}");
    panic!("{reason}");
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

/// Capture the git provenance of this build and expose it via `cargo:rustc-env`.
///
/// Why: Bug reports and log lines need a deterministic identifier for the
/// running binary — `CARGO_PKG_VERSION` alone collapses every commit on a
/// dev branch into the same version string, which is how a ~3.5h-stale
/// `tagent` was reported as a code bug (#4260). The commit date answers "is
/// this binary older than that merge?" directly, and the dirty flag says
/// whether the SHA describes the whole binary or only most of it.
/// What: Runs `git rev-parse --short HEAD`, `git rev-parse HEAD`,
/// `git log -1 --format=%cI`, and `git status --porcelain`, exposing them as
/// `GIT_COMMIT_HASH`, `GIT_COMMIT_HASH_FULL`, `GIT_COMMIT_DATE`, and
/// `GIT_DIRTY` (`"1"`/`"0"`). Each git value falls back to `"unknown"` (dirty
/// to `"0"`) when git is unavailable or this is not a repo. The dirty flag
/// describes the tree as of the last build-script run: the
/// `cargo:rerun-if-changed` paths above are repo-root-relative and so never
/// exist next to this crate, which makes cargo re-run this script on every
/// build and keeps the flag current.
/// Test: `crates/trusty-agents/tests/version_provenance.rs` asserts the
/// binary's `--version` carries `GIT_COMMIT_HASH`;
/// `build_info::commit_mark_matches_the_dirty_flag` covers the rendering.
fn emit_git_provenance() {
    let short = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let full = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let date = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());
    // #4260: `--porcelain` prints one line per changed or untracked path, so
    // any output at all means the tree does not match the SHA above.
    let dirty = match git(&["status", "--porcelain"]) {
        Some(_) => "1",
        None => "0",
    };

    println!("cargo:rustc-env=GIT_COMMIT_HASH={short}");
    println!("cargo:rustc-env=GIT_COMMIT_HASH_FULL={full}");
    println!("cargo:rustc-env=GIT_COMMIT_DATE={date}");
    println!("cargo:rustc-env=GIT_DIRTY={dirty}");
}

/// Run `git` with `args`, returning trimmed stdout, or `None` when the command
/// fails or produces no output.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
