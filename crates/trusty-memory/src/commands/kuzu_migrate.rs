//! Deprecated `trusty-memory migrate kuzu-data` (#277).
//!
//! Why: this command used to open a kuzu-memory `store.redb` with `redb`. Real
//! kuzu-memory stores are KuzuDB databases (`.kuzu-memory/memories.db`), so
//! that reader never matched real data. `trusty-memory import kuzu` replaces
//! it; this alias stays so existing scripts get a warning instead of a clap
//! error.
//! What: [`handle_kuzu_data_migrate`] prints a deprecation warning and runs
//! `import kuzu --from <path> --palace <name>` with the same `--dry-run`.
//! `--limit` has no equivalent and is ignored with a warning.
//! Test: `deprecated_kuzu_data_forwards_to_import`.

use anyhow::Result;
use std::path::Path;

use super::kuzu_import::{handle_import_kuzu, KuzuImportArgs, DEFAULT_MAX_DEPTH};

/// Forward `migrate kuzu-data` to `import kuzu`, with a deprecation warning.
///
/// Why: see the module doc.
/// What: builds the equivalent [`KuzuImportArgs`] and runs the async import on
/// a dedicated thread with its own runtime, so this stays a plain `fn` that
/// works whether or not the caller is already inside a Tokio runtime.
/// Test: `deprecated_kuzu_data_forwards_to_import`.
pub fn handle_kuzu_data_migrate(
    from: &Path,
    palace_name: &str,
    dry_run: bool,
    limit: Option<usize>,
) -> Result<()> {
    // #277: the redb reader is gone; this is a forwarding alias now.
    eprintln!(
        "warning: `migrate kuzu-data` is deprecated and will be removed; use \
         `trusty-memory import kuzu --from <.kuzu-memory dir> [--palace <name>]`"
    );
    if limit.is_some() {
        eprintln!("warning: --limit is not supported by `import kuzu` and is ignored");
    }
    let args = deprecated_args(from, palace_name, dry_run);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?
                    .block_on(handle_import_kuzu(args))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("kuzu import thread panicked"))?
    })
}

/// The `import kuzu` flags equivalent to the old `migrate kuzu-data` ones.
pub(crate) fn deprecated_args(from: &Path, palace_name: &str, dry_run: bool) -> KuzuImportArgs {
    KuzuImportArgs {
        from: Some(from.to_path_buf()),
        palace: Some(palace_name.to_string()),
        dry_run,
        max_depth: DEFAULT_MAX_DEPTH,
        ..KuzuImportArgs::default()
    }
}
