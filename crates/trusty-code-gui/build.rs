//! build.rs — builds the Svelte frontend bundle, then runs `tauri_build::build()`.
//!
//! Why: `src/lib.rs`'s `tauri::generate_context!()` embeds `ui/dist`, the
//! `frontendDist` declared in `tauri.conf.json`. `ui/dist` is gitignored, so on
//! a fresh clone or worktree it does not exist and the proc macro panics with a
//! bare "this path doesn't exist" — which took down `cargo test --workspace`
//! and `cargo clippy --workspace --all-targets` for the ENTIRE workspace, since
//! neither reaches a single test before this crate fails to compile (#4699).
//! Nothing built the bundle; it existed only where someone had run pnpm by hand.
//!
//! The check fires only when `tauri`'s `custom-protocol` feature is on. A bare
//! `cargo check -p trusty-code-gui` leaves it off, so the macro falls back to
//! `devUrl` and passes; a workspace-wide build unifies features with
//! `trusty-agents-ui` (which enables `custom-protocol` by default) and turns it
//! on for every member. That is why the failure looked workspace-only.
//!
//! What: emits `cargo:rerun-if-changed` for the UI sources, then runs
//! `pnpm install` + `pnpm run build` inside `ui/` unless `SKIP_UI_BUILD=1` is
//! set. Every failure path — no usable pnpm, a failed install, a failed build,
//! or a build that exits 0 without producing `ui/dist/index.html` — aborts the
//! crate build with a message naming this crate and the `SKIP_UI_BUILD=1`
//! escape hatch. A stale or empty `dist/` is never embedded silently.
//!
//! NOTE: the block between the CANONICAL BLOCK markers is kept byte-identical
//! across all three Tauri crates — this one, `crates/trusty-mpm-gui/build.rs`,
//! and `crates/trusty-agents/ui/src-tauri/build.rs`;
//! `scripts/check_buildrs_sync.sh` asserts it. One of those is edition 2021, so
//! the block uses no let-chains. It is deliberately NOT the block the four
//! UI-embedding daemon crates share (`trusty-memory`, `trusty-analyze`,
//! `trusty-console`, `trusty-search`): those degrade to a placeholder on any
//! failure, which is right for an optional web surface and wrong for a desktop
//! app whose entire window is the bundle.
//!
//! Test: `cargo check -p trusty-code-gui --features tauri/custom-protocol` on a
//! tree with no `ui/dist` — the #4699 reproducer. It panics before this change
//! and builds the real bundle after it.

use std::path::Path;
use std::process::Command;

/// Names this crate in every diagnostic the shared block emits.
const CRATE_NAME: &str = "trusty-code-gui";

fn main() {
    let crate_root = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let ui_dir = crate_root.join("ui");
    let dist_dir = ui_dir.join("dist");

    println!("cargo:rerun-if-env-changed=SKIP_UI_BUILD");
    // Only paths that EXIST may be declared: cargo treats a declared-but-absent
    // path as changed, so one stale entry re-runs the whole pnpm build on every
    // `cargo check`. `vitest.config.ts` is omitted for the same reason in
    // reverse — it exists but feeds `pnpm test`, not `pnpm build`.
    for rel in [
        "ui/package.json",
        "ui/pnpm-lock.yaml",
        "ui/pnpm-workspace.yaml",
        "ui/index.html",
        "ui/vite.config.ts",
        "ui/svelte.config.js",
        "ui/tailwind.config.js",
        "ui/postcss.config.js",
        "ui/tsconfig.json",
        "ui/src",
        "ui/public",
    ] {
        println!("cargo:rerun-if-changed={rel}");
    }
    // `ui/dist` is deliberately NOT declared. It is this script's own output,
    // and cargo's staleness reference is `invoked.timestamp`, stamped BEFORE
    // the script runs — so declaring it makes the script dirty its own
    // fingerprint and re-run the pnpm build on every `cargo check` (measured:
    // 2.5s each). The residual is that deleting `ui/dist` by hand while
    // `target/` stays warm does not re-trigger this script; `cargo clean` or a
    // touch of any UI source recovers.

    // #4699: build the bundle before tauri_build, so a broken UI fails here
    // rather than as an opaque proc-macro panic in src/lib.rs.
    build_tauri_ui(&ui_dir, &dist_dir, CRATE_NAME);

    tauri_build::build();
}

// ── TAURI UI CANONICAL BLOCK BEGIN (kept in sync by scripts/check_buildrs_sync.sh) ──

/// Build the Tauri frontend bundle into `dist_dir`, or abort the crate build.
///
/// Why: `frontendDist` must exist and be current before `generate_context!()`
/// runs. Producing it here is what makes a clean clone compile (#4699); failing
/// loudly is what keeps a half-built bundle from being embedded and shipped.
/// What: honours `SKIP_UI_BUILD=1`, then requires pnpm and runs
/// `install` + `run build` inside `ui_dir`. Presence of `dist_dir` is never
/// treated as proof it is current — the build always runs, and cargo's
/// `rerun-if-changed` directives are what keep it from running needlessly.
/// Test: `cargo check -p <gui-crate> --features tauri/custom-protocol` with no
/// `ui/dist` present; and the same command with `SKIP_UI_BUILD=1`.
fn build_tauri_ui(ui_dir: &Path, dist_dir: &Path, crate_name: &str) {
    let index = dist_dir.join("index.html");

    // Step 1: the documented escape hatch for a host with no JS toolchain.
    // It still has to leave something at `frontendDist` or the proc macro
    // panics, so write a placeholder and say plainly that the UI is not real.
    if std::env::var("SKIP_UI_BUILD").as_deref() == Ok("1") {
        if !index.exists() {
            println!(
                "cargo:warning={crate_name}: SKIP_UI_BUILD=1 and {dist} is empty — \
                 embedding a PLACEHOLDER UI. The resulting binary will not show the \
                 real interface. Run `pnpm --dir ui install && pnpm --dir ui build` \
                 and rebuild without SKIP_UI_BUILD to get a working app.",
                dist = dist_dir.display()
            );
            write_placeholder(dist_dir, crate_name);
        }
        return;
    }

    // Step 2: no package.json means the checkout is incomplete. These crates are
    // `publish = false`, so there is no extracted-tarball case to tolerate.
    if !ui_dir.join("package.json").exists() {
        fail(
            crate_name,
            &format!("{ui}/package.json is missing.", ui = ui_dir.display()),
        );
    }

    // Step 3: pnpm is required, and is probed FROM `ui_dir` — corepack resolves
    // the `packageManager` pin relative to the working directory, so a probe run
    // from the workspace root selects a different pnpm (or fails outright).
    if !probe_ok("pnpm", ui_dir) {
        fail(
            crate_name,
            "`pnpm --version` did not succeed in the `ui/` directory, so pnpm is \
             unavailable or unusable there.",
        );
    }

    // Step 4: install, then build. A non-zero exit from either aborts; neither
    // result is discarded.
    let mut install_args = vec!["install"];
    if ui_dir.join("pnpm-lock.yaml").exists() {
        install_args.push("--frozen-lockfile");
    }
    run(crate_name, &install_args, ui_dir);
    run(crate_name, &["run", "build"], ui_dir);

    // Step 5: trust the artefact, not the exit code. A build that reports
    // success but emits no entry point is a failed build.
    if !index.exists() {
        fail(
            crate_name,
            &format!(
                "`pnpm run build` exited 0 but produced no {index}.",
                index = index.display()
            ),
        );
    }
}

/// Run a pnpm subcommand in `cwd`, aborting the crate build unless it exits 0.
///
/// Why: a swallowed `Command` result would let compilation proceed against a
/// stale or absent bundle — the exact failure mode #4699 asks this script not
/// to reintroduce.
/// What: spawns `pnpm <args>` with stdio inherited (so pnpm's own diagnostics
/// reach the build log) and routes both a non-zero status and a spawn error
/// into `fail`.
/// Test: covered by the `pnpm run build` leg of `build_tauri_ui`.
fn run(crate_name: &str, args: &[&str], cwd: &Path) {
    match Command::new("pnpm").args(args).current_dir(cwd).status() {
        Ok(status) if status.success() => {}
        Ok(status) => fail(
            crate_name,
            &format!("`pnpm {}` failed ({status}).", args.join(" ")),
        ),
        Err(e) => fail(
            crate_name,
            &format!("could not run `pnpm {}`: {e}", args.join(" ")),
        ),
    }
}

/// Report whether `program --version` succeeds when run from `cwd`.
fn probe_ok(program: &str, cwd: &Path) -> bool {
    Command::new(program)
        .arg("--version")
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Write the stub entry point used only under `SKIP_UI_BUILD=1`.
///
/// Why: the escape hatch has to leave `frontendDist` populated or the proc
/// macro panics anyway, defeating the point of the opt-out.
/// What: creates the parent directory and writes a minimal HTML document that
/// states, in the window itself, that the real UI was not built. Filesystem
/// errors abort rather than being discarded.
/// Test: `SKIP_UI_BUILD=1 cargo check -p <gui-crate>` on a tree with no
/// `ui/dist` leaves `ui/dist/index.html` in place.
///
/// Deliberately free of let-chains: this block is shared verbatim with
/// `trusty-agents-ui`, which is edition 2021.
fn write_placeholder(dist_dir: &Path, crate_name: &str) {
    if let Err(e) = std::fs::create_dir_all(dist_dir) {
        fail(
            crate_name,
            &format!("could not create {}: {e}", dist_dir.display()),
        );
    }
    let index = dist_dir.join("index.html");
    let html = format!(
        "<!doctype html><html><body><p>{crate_name}: the UI bundle was not built \
         (SKIP_UI_BUILD=1). Run <code>pnpm --dir ui install &amp;&amp; pnpm --dir ui \
         build</code> and rebuild.</p></body></html>"
    );
    if let Err(e) = std::fs::write(&index, html) {
        fail(
            crate_name,
            &format!("could not write {}: {e}", index.display()),
        );
    }
}

/// Abort the crate build with a diagnostic that names the crate and the fix.
///
/// Why: the failure this replaces was a proc-macro panic pointing at
/// `src/lib.rs` and complaining about a path, with no hint that a frontend
/// build was missing — several agents rediscovered the workaround
/// independently before #4699 was filed.
/// What: panics, which cargo surfaces as "failed to run custom build command
/// for <crate>" with this text attached.
/// Test: every failure leg of `build_tauri_ui` routes here.
fn fail(crate_name: &str, detail: &str) -> ! {
    let msg = format!(
        "\n{crate_name}: could not build the frontend bundle in `ui/`.\n\
         {detail}\n\
         `tauri.conf.json` points `frontendDist` at that bundle, so without it \
         `tauri::generate_context!()` fails with a bare \"this path doesn't \
         exist\" panic (#4699).\n\
         Fix: install pnpm (https://pnpm.io/installation) and rebuild, or set \
         SKIP_UI_BUILD=1 to compile against a placeholder UI.\n"
    );
    panic!("{msg}");
}

// ── TAURI UI CANONICAL BLOCK END ──
