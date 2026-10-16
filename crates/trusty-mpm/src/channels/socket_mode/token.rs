//! The Socket Mode app token, through the host's `app_ref` only (#9923,
//! plan item 2).
//!
//! Why: the app-level token opens the trusty app's socket, so it is read the
//! one way the host owner chose it: the host ceiling's Slack `app_ref`
//! (`trusty-channels` `HostChannel::app_ref`). A user token (`slack-user`)
//! and the bot token never serve here, and no code here reads an environment
//! variable by name.
//! What: [`app_ref`] reads the enabled Slack channel's `app_ref`;
//! [`check_app_ref`] accepts only [`SLACK_APP_REF`]; [`resolve_app_token`]
//! maps it to its registry variable and reads it through the daemon's one
//! credential door, `secret_source::resolve_bounded_gated` (nothing in a
//! sandbox, #9121).
//! Test: `daemon_holds_app_token_via_app_ref_only`,
//! `no_user_token_loaded_anywhere_in_channels_path`.

use std::path::Path;

use trusty_common::credential_registry::env_var_for;
use trusty_common::credentials::{Secret, SecretResolveError};

use crate::channels::slack_bot::{SLACK_BOT_REF, SLACK_USER_REF, host_slack};

/// The one credential key whose token may open the socket (#8454 S2c).
pub const SLACK_APP_REF: &str = "slack-app";

/// The host ceiling's Slack `app_ref` under `home`. Blocking.
///
/// What: `Err` naming the fault when the host file is missing or refused,
/// Slack is absent or disabled, or no `app_ref` is set.
/// Test: `daemon_holds_app_token_via_app_ref_only`.
pub fn app_ref(home: Option<&Path>) -> Result<String, String> {
    host_slack(home)?
        .app_ref()
        .map(str::to_owned)
        .ok_or_else(|| "the slack connection names no app_ref".to_owned())
}

/// `Ok` only for [`SLACK_APP_REF`]; the refusal names the key's role.
///
/// Test: `no_user_token_loaded_anywhere_in_channels_path`.
pub fn check_app_ref(app_ref: &str) -> Result<(), String> {
    match app_ref {
        SLACK_APP_REF => Ok(()),
        SLACK_USER_REF => Err("app_ref names the slack user token, which is never used".into()),
        SLACK_BOT_REF => {
            Err("app_ref names the slack bot token, which cannot open a socket".into())
        }
        other => Err(format!("app_ref {other:?} is not the slack app token")),
    }
}

/// Resolve `app_ref`'s token through the credential door.
///
/// What: [`resolve_app_token_with`] over `resolve_bounded_gated`. Blocking.
/// Test: `daemon_holds_app_token_via_app_ref_only`.
pub fn resolve_app_token(app_ref: &str) -> Result<Secret<String>, String> {
    resolve_app_token_with(app_ref, crate::secret_source::resolve_bounded_gated)
}

/// [`resolve_app_token`] with the credential door a parameter.
///
/// What: refuses any key but [`SLACK_APP_REF`] before `resolve` runs, maps
/// the key to its registry variable, then calls `resolve` with that name
/// alone. The error names the kind, never a value.
/// Test: `daemon_holds_app_token_via_app_ref_only`,
/// `no_user_token_loaded_anywhere_in_channels_path`.
pub fn resolve_app_token_with(
    app_ref: &str,
    resolve: impl FnOnce(&str) -> Result<String, SecretResolveError>,
) -> Result<Secret<String>, String> {
    check_app_ref(app_ref)?;
    let var = env_var_for(app_ref)
        .ok_or_else(|| format!("app_ref {app_ref:?} is not in the credential registry"))?;
    resolve(var)
        .map(Secret::new)
        .map_err(|e| format!("{var} could not be resolved: {}", e.kind()))
}

/// The app token for the daemon home `home`: [`app_ref`], then
/// [`resolve_app_token`]. Blocking.
pub fn app_token(home: Option<&Path>) -> Result<Secret<String>, String> {
    resolve_app_token(&app_ref(home)?)
}
