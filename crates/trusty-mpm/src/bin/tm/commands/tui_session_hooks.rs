//! The `tm ls` session actions, registered as the tui's session hooks.
//!
//! #10754: S3 tm ls actions plumbing (keys wait on De1).
//!
//! Why: the lib's `tm tui` cannot import this binary, so it takes the `tm ls`
//! operations as function pointers
//! ([`trusty_mpm::tui::session_hooks::SessionHooks`]); see that module.
//! What: each hook wraps the function `tm ls` itself calls, with no logic of
//! its own beyond the lookup that turns the tui's argument into that
//! function's input: open → `guided_resume::resume_guided_session`, rename →
//! `rename::do_rename_request`, new → `guided_launch::launch_new_session_and_attach`
//! on the project's checkout from `new_session::request_for_registered`,
//! delete → `picker_delete::route_delete_for_state` (state from
//! `managed_state_for_delete`, as `tm session delete` does; no `force`).
//! The tui's `d` decommission is a different call (it removes the workspace),
//! so delete is a hook of its own.
//! Test: `tui_session_hooks_register_every_operation`.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::project::local_checkout_for_url;
use trusty_mpm::tui::session_hooks::{
    SessionHooks, SessionOpFuture, SessionOpOutcome, set_session_hooks,
};

use super::picker_launch_new::LaunchIsolation;
use super::session_tui::new_session::request_for_registered;
use super::session_tui::new_session_name::created_message;
use super::tmux_attach::AttachOutcome;

/// Every `tm ls` operation the tui borrows. A struct literal names every
/// field, so a hook the lib adds cannot go unregistered here.
pub(crate) const HOOKS: SessionHooks = SessionHooks {
    open,
    rename,
    new_session,
    delete,
};

/// Register [`HOOKS`] with the tui; called once at startup.
pub(crate) fn install_tui_session_hooks() -> bool {
    set_session_hooks(HOOKS)
}

/// Map an attach hand-off onto the tui's outcome, as `tm ls` reads it (#2678).
fn after_attach(outcome: AttachOutcome, notice: String) -> SessionOpOutcome {
    if outcome.ends_interactive_loop() {
        SessionOpOutcome::LeftTerminal(notice)
    } else {
        SessionOpOutcome::Done(notice)
    }
}

fn open(client: DaemonClient, id: String) -> SessionOpFuture {
    Box::pin(async move {
        let session = client.get_managed_session(&id).await?;
        let outcome = super::guided_resume::resume_guided_session(&client, &session).await?;
        Ok(after_attach(outcome, format!("opened {}", session.name)))
    })
}

fn rename(client: DaemonClient, id: String, name: String) -> SessionOpFuture {
    Box::pin(async move {
        let msg = super::rename::do_rename_request(&client, &id, &id, name).await?;
        Ok(SessionOpOutcome::Done(msg))
    })
}

fn new_session(client: DaemonClient, name: String, repo_url: String) -> SessionOpFuture {
    Box::pin(async move {
        let checkout = local_checkout_for_url(&repo_url);
        let request = request_for_registered(&name, &repo_url, checkout.as_deref())
            .map_err(anyhow::Error::msg)?;
        let created = created_message(&request);
        let outcome = super::guided_launch::launch_new_session_and_attach(
            &client,
            &request.repo,
            request.name_hint.as_deref(),
            LaunchIsolation::default(),
        )
        .await?;
        Ok(after_attach(outcome, created))
    })
}

fn delete(client: DaemonClient, id: String) -> SessionOpFuture {
    Box::pin(async move {
        let state = super::picker_delete::managed_state_for_delete(&client, &id).await?;
        let report =
            super::picker_delete::route_delete_for_state(&client, &id, state.as_deref(), false)
                .await;
        let (text, _severity) = super::session_tui::delete_outcome(&id, report);
        Ok(SessionOpOutcome::Done(text))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_session_hooks_register_every_operation() {
        // Whichever call registered first, a second one is refused.
        install_tui_session_hooks();
        assert!(!install_tui_session_hooks(), "registration happens once");
        let hooks = trusty_mpm::tui::session_hooks::session_hooks()
            .expect("the binary registers its hooks");
        // Each field is the wrapper above, not a stand-in.
        assert!(std::ptr::fn_addr_eq(hooks.open, HOOKS.open));
        assert!(std::ptr::fn_addr_eq(hooks.rename, HOOKS.rename));
        assert!(std::ptr::fn_addr_eq(hooks.new_session, HOOKS.new_session));
        assert!(std::ptr::fn_addr_eq(hooks.delete, HOOKS.delete));
    }

    #[test]
    fn after_attach_stops_the_tui_only_when_the_client_moved() {
        assert_eq!(
            after_attach(AttachOutcome::Attached, "x".into()),
            SessionOpOutcome::Done("x".into())
        );
        assert_eq!(
            after_attach(AttachOutcome::Switched, "x".into()),
            SessionOpOutcome::LeftTerminal("x".into())
        );
    }
}
