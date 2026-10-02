//! `tm watch poll|listen` dispatch, split out of `main.rs` to keep the entry
//! point under the 500-SLOC production cap (#8261).

use crate::{cli, commands};

/// Dispatch a `tm watch poll|listen` invocation to its handler.
///
/// Why: keeps `main`'s match arm thin by folding the flattened `WatchArgs` into
/// the [`commands::watch`] entry points in one place, mapping the shared CLI flags
/// onto the module's `RawWatchArgs` and the safety-gate booleans.
/// What: builds a `RawWatchArgs` from the parsed flags and calls
/// [`commands::watch::poll`] or [`commands::watch::listen`](mod@crate::commands::watch::listen) accordingly, threading
/// the `--execute`/`--dry-run` safety flags and the spawn runtime through.
/// Test: the resolution/safety logic is unit-tested in `commands::watch::tests`;
/// CLI parsing in `tests.rs` (`cli_parses_watch_*`).
pub(crate) async fn dispatch_watch(
    client: &reqwest::Client,
    url: &str,
    cmd: cli::WatchCmd,
) -> anyhow::Result<()> {
    use cli::{WatchArgs, WatchCmd};
    use commands::watch::args::RawWatchArgs;

    fn raw(args: &WatchArgs) -> RawWatchArgs {
        RawWatchArgs {
            project: args.project.clone(),
            label: args.label.clone(),
            interval_secs: args.interval_secs,
            state: args.state,
        }
    }

    match cmd {
        WatchCmd::Poll { args } => {
            commands::watch::poll(
                client,
                url,
                raw(&args),
                args.execute,
                args.dry_run,
                args.runtime,
            )
            .await
        }
        WatchCmd::Listen { args } => {
            commands::watch::listen(
                client,
                url,
                raw(&args),
                args.execute,
                args.dry_run,
                args.runtime,
            )
            .await
        }
    }
}
