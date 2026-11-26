//! `tm architect restore [--version V] [--dir DIR]`: the launch deploy's undo (#11368).
//!
//! Why: #11368 AC "The previous release restores from the kept copy";
//! Architect Q2 names this verb.
//! What: [`run`] resolves the Architect directory as `drift` does, takes the
//! `up` lock without waiting (so no deploy runs meanwhile), calls
//! `fleet::launch_backup::restore` and prints one line per kept file.
//! Exit: 0 restored, 1 a file kept as edited, a failed write, no kept copy or
//! the lock held, 2 the directory cannot be resolved.
//! Test: `restore_brings_back_the_previous_release_bytes_11368`,
//! `restore_never_overwrites_an_edited_file_11368`.

use std::time::Duration;

use super::fleet::{launch_backup, up};

/// `tm architect restore`; returns the exit code.
pub(crate) fn run(version: Option<&str>, dir: Option<&str>) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("tm architect restore: cannot resolve the home directory");
        return 2;
    };
    let dir = match super::fleet::resolve_dir(dir, &home) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tm architect restore: {e:#}");
            return 2;
        }
    };
    // Read-only first: a directory with no kept copy gets no lock file either.
    if !dir.join(launch_backup::ROOT).is_dir() {
        eprintln!(
            "tm architect restore: no kept copy under {}",
            dir.join(launch_backup::ROOT).display()
        );
        return 1;
    }
    let _lock = match up::lock_within(&dir, Duration::ZERO) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tm architect restore: {e}; nothing restored");
            return 1;
        }
    };
    let restored = match launch_backup::restore(&dir, version) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tm architect restore: {e:#}");
            return 1;
        }
    };
    for step in &restored.steps {
        print!("{}", step.render_line());
    }
    let exit = restored.exit();
    println!(
        "tm architect restore: release {} kept copy, exit {exit}; the poller and dashboard \
         keep the code they loaded until `tm architect restart`, and the next `tm architect \
         up` with tm {} deploys its bundle again",
        restored.version,
        env!("CARGO_PKG_VERSION")
    );
    exit
}
