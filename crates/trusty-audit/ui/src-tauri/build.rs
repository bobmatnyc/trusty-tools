//! build.rs — builds the Svelte frontend bundle, then runs `tauri_build::build()`.
//!
//! Why: `src/main.rs`'s `tauri::generate_context!()` embeds `../dist` (this
//! crate lives in `src-tauri/`, so that is `crates/trusty-audit/ui/dist`), the
//! `frontendDist` declared in `tauri.conf.json`. That directory is gitignored,
//! so on a fresh clone or worktree it does not exist and the proc macro panics
//! with a bare "this path doesn't exist" (#4699). Building the bundle here is
//! what makes a clean clone compile.
//!
//! What: emits `cargo:rerun-if-changed` for the UI sources, then runs
//! `pnpm install` + `pnpm run build` in the parent `ui/` directory unless
//! `SKIP_UI_BUILD=1` is set. Every failure path aborts the crate build naming
//! this crate and the escape hatch; a stale or empty `dist/` is never embedded
//! silently.
//!
//! NOTE: the block between the CANONICAL BLOCK markers is byte-identical across
//! all four Tauri crates — this one, `crates/trusty-agents/ui/src-tauri`,
//! `crates/trusty-code-gui`, and `crates/trusty-mpm-gui`;
//! `scripts/check_buildrs_sync.sh` asserts it. Two of the four are edition
//! 2021, which is why the block uses no let-chains.
//!
//! Test: `cargo check -p trusty-audit-ui` on a tree with no
//! `crates/trusty-audit/ui/dist`, and the same command with `SKIP_UI_BUILD=1`.

use std::path::Path;
use std::process::Command;

/// Names this crate in every diagnostic the shared block emits.
const CRATE_NAME: &str = "trusty-audit-ui";

fn main() {
    let crate_root = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    // This manifest sits in `src-tauri/` inside the frontend package, so the UI
    // root is the PARENT directory (the trusty-agents-ui layout).
    let ui_dir = crate_root
        .parent()
        .expect("crate root always has a parent")
        .to_path_buf();
    let dist_dir = ui_dir.join("dist");

    println!("cargo:rerun-if-env-changed=SKIP_UI_BUILD");
    // Only paths that EXIST may be declared: cargo treats a declared-but-absent
    // path as changed, so one stale entry re-runs the whole pnpm build on every
    // `cargo check`.
    for rel in [
        "../package.json",
        "../pnpm-lock.yaml",
        "../pnpm-workspace.yaml",
        "../index.html",
        "../vite.config.ts",
        "../svelte.config.js",
        "../tsconfig.json",
        "../src",
    ] {
        println!("cargo:rerun-if-changed={rel}");
    }
    // `../dist` is deliberately NOT declared. It is this script's own output,
    // and cargo's staleness reference is `invoked.timestamp`, stamped BEFORE
    // the script runs — so declaring it makes the script dirty its own
    // fingerprint and re-run the pnpm build on every `cargo check`.

    // #4699: build the bundle before tauri_build, so a broken UI fails here
    // rather than as an opaque proc-macro panic in src/main.rs.
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
