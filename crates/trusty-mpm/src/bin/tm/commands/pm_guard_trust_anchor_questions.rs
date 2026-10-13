//! The channel question table as a trust anchor (#9815, P2a of #8433, G7).
//!
//! Seam: the verdict the red tests reach; the seal lands in the fix commit.

use std::path::Path;

use super::{AnchorWrite, UnplaceableWrite};

/// The deny for a call that could write the question table, for every
/// session, taint or none; `None` when the call misses it.
pub(super) fn questions_verdict(
    _writes: &Result<Vec<AnchorWrite>, UnplaceableWrite>,
    _cwd: Option<&Path>,
    _home: Option<&Path>,
) -> Option<String> {
    None
}
