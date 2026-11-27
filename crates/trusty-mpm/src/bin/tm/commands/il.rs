//! `tm il config` — show and edit the Architect IL schedule (#11001).
//!
//! Why: ADR-0077 D3 — the CLI is the only writer of `local/il-schedule.json`.
//! What: [`run`] resolves the Architect directory (`--dir`, else the recorded
//! Architect, as `tm fleet` does) and calls `trusty_mpm::core::il_schedule`.
//! `show` prints the schedule on stdout and its source on stderr; an edit
//! prints one report line; `diff` prints the preset comparison (#11002).
//! Every refusal or write failure is an error.
//! Test: `cli_parses_every_il_config_verb`; the behaviour in
//! `core::il_schedule::tests`.

use anyhow::{Context, bail};
use trusty_mpm::core::il_schedule::{self, Schedule};

use crate::cli::{IlAction, IlConfigAction};

/// Dispatch `tm il <action>`.
pub(crate) fn run(action: IlAction) -> anyhow::Result<()> {
    let IlAction::Config { dir, action } = action;
    let home = dirs::home_dir().context("cannot resolve the home directory")?;
    let dir = crate::commands::fleet::resolve_dir(dir.as_deref(), &home)?;
    if !dir.is_dir() {
        bail!(
            "no Architect directory at {}; run `tm fleet init` first, or pass --dir",
            dir.display()
        );
    }
    let path = il_schedule::path(&dir);
    let outcome = match action {
        IlConfigAction::Show => {
            let loaded = il_schedule::load(&path)?;
            if loaded.from_file {
                eprintln!("source: {}", path.display());
            } else {
                eprintln!("source: built-in preset ({} is absent)", path.display());
            }
            print!("{}", loaded.text);
            return Ok(());
        }
        IlConfigAction::Enroll {
            session,
            project,
            code,
        } => il_schedule::update(&path, |s: &mut Schedule| {
            s.enroll(&session, &project, code.as_deref())
        })?,
        IlConfigAction::Remove { session } => il_schedule::update(&path, |s| s.remove(&session))?,
        IlConfigAction::Set {
            key,
            value,
            session,
        } => il_schedule::update(&path, |s| s.set(session.as_deref(), &key, &value))?,
        // #11002: ADR-0077 D6, the preset diff and the versioned migration.
        IlConfigAction::Diff => {
            print!("{}", il_schedule::diff(&path)?.render(&path));
            return Ok(());
        }
        IlConfigAction::Update => il_schedule::upgrade(&path)?,
        IlConfigAction::Reset { force } => il_schedule::reset(&path, force)?,
    };
    println!("{}", outcome.message);
    Ok(())
}

#[cfg(test)]
#[path = "il_tests.rs"]
mod tests;
