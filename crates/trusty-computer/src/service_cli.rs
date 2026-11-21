//! The `install-service` and `uninstall-service` verbs.
//!
//! Why: the CLI wires the real `launchctl`, `codesign` and CuaDriver version
//! probe into [`crate::launchd::manage`]; only this module touches them
//! (issue #11460).
//! What: [`signing`] resolves `--no-sign`, then `--sign-identity`, then
//! [`crate::codesign::IDENTITY_ENV`], then auto-detect. [`run`] parses
//! `[--state DIR] [--sign-identity ID | --no-sign]` and calls `manage`.
//! Test: `signing_flag_beats_env_and_no_sign_skips`, `service_args_parse_and_refuse_conflicts`.

use std::path::PathBuf;

use crate::codesign::{IDENTITY_ENV, Signer, Signing, SystemCodesign};
use crate::launchd::{self, Layout, SystemLaunchctl};
use crate::launcher::{CuaAppLauncher, Launcher as _};

/// Parsed flags of one service verb.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceArgs {
    /// `--state DIR`.
    pub state: Option<PathBuf>,
    /// `--sign-identity ID`.
    pub sign_identity: Option<String>,
    /// `--no-sign`.
    pub no_sign: bool,
}

/// Parse the flags after the verb; unknown or conflicting flags are `Err`.
pub fn parse(args: &[String]) -> Result<ServiceArgs, String> {
    let mut out = ServiceArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
        match a.as_str() {
            "--state" => out.state = Some(PathBuf::from(val("--state")?)),
            "--sign-identity" => out.sign_identity = Some(val("--sign-identity")?),
            "--no-sign" => out.no_sign = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if out.no_sign && out.sign_identity.is_some() {
        return Err("--sign-identity and --no-sign conflict".to_owned());
    }
    Ok(out)
}

/// Flag, then `env`, then auto-detect (`None`); `--no-sign` skips.
pub fn signing(args: &ServiceArgs, env: Option<String>) -> Signing {
    if args.no_sign {
        return Signing::Skip;
    }
    let env = env.filter(|v| !v.is_empty());
    Signing::Sign(args.sign_identity.clone().or(env))
}

/// Run `install-service` (`install`) or `uninstall-service`; returns the exit code.
pub fn run(install: bool, rest: &[String]) -> u8 {
    let args = match parse(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("trusty-computer: {e}");
            return 2;
        }
    };
    let layout = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())
        .and_then(|exe| {
            let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
            Ok(Layout {
                home: PathBuf::from(home),
                state: args
                    .state
                    .clone()
                    .unwrap_or_else(crate::operator_cli::default_state),
                exe,
                uid: crate::private::uid(),
            })
        });
    let layout = match layout {
        Ok(l) => l,
        Err(e) => {
            eprintln!("trusty-computer: {e}");
            return 1;
        }
    };
    let signer = Signer {
        signing: signing(&args, std::env::var(IDENTITY_ENV).ok()),
        tool: &SystemCodesign,
    };
    let version = || CuaAppLauncher.binary_version().map_err(|e| e.to_string());
    match launchd::manage(&layout, install, &SystemLaunchctl, &signer, &version) {
        Ok(()) => {
            let verb = if install { "installed" } else { "removed" };
            println!("{verb} {}", launchd::plist_path(&layout.home).display());
            0
        }
        Err(e) => {
            eprintln!("trusty-computer: {e}");
            1
        }
    }
}
