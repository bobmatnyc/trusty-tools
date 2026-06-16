//! Demonstrates the rustup-style progress + status display.
//!
//! Run with:
//! ```text
//! cargo run -p trusty-progress --example rustup_style
//! ```
//!
//! When stderr is a terminal you get animated spinners; piped or in CI the
//! output degrades to plain append-only lines (try `… 2>&1 | cat`).

use std::thread::sleep;
use std::time::Duration;

use trusty_progress::{Component, ComponentTracker, Narrator, Output, ProgressHandle};

fn main() -> Result<(), trusty_progress::ProgressError> {
    // Autodetect the terminal: animated interactively, plain when piped/CI.
    let output = Output::to_stderr();
    let narrator = Narrator::new(output.clone());

    narrator.info("downloading 6 components")?;

    // Indeterminate phase: a spinner while we "resolve" the toolchain.
    let spinner = ProgressHandle::spinner(&output, "resolving stable-aarch64-apple-darwin");
    sleep(Duration::from_millis(400));
    spinner.finish_and_clear();

    // Determinate phase: a bar over the known component count.
    let components = [
        Component::new("cargo", 8_870_953),
        Component::new("clippy", 3_030_384),
        Component::new("rust-std", 27_472_691),
    ];
    let bar = ProgressHandle::bar(&output, components.len() as u64, "installing");
    let mut table = ComponentTracker::new(output.clone());
    for component in components {
        sleep(Duration::from_millis(200));
        bar.inc(1);
        table.add(component);
    }
    bar.finish_and_clear();

    // The aligned "name installed size" table, rustup-style.
    table.print()?;

    narrator.info("default toolchain set to stable-aarch64-apple-darwin")?;
    Ok(())
}
