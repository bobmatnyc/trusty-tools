//! The `permissions:` frontmatter block: captured and re-emitted verbatim (#8199).
//!
//! Why: the composer re-emits frontmatter from the keys it models, so a
//! `permissions:` block on a source card was dropped on deploy, and a deployed
//! trusty-code card ran with every tool allowed. The block is a nested map of
//! tool globs to decisions or argument tables, which the composer's flat
//! `key: value` reader cannot model — and its indented rule keys were being
//! read as top-level fields. trusty-code owns the grammar
//! (`trusty_code::permissions::parse_permissions`); this module only carries
//! the bytes through. `builder.rs` sits at the 500-SLOC cap, so it lives here.
//! What: [`PermissionsBlock`] collects a top-level `permissions:` line and
//! every blank or indented line under it, the same extent trusty-code's
//! extractor reads, and [`PermissionsBlock::finish`] returns it as one
//! newline-terminated string for `merge_frontmatter` to emit unchanged.
//! Test: `permissions_block_survives_compose_verbatim`,
//! `permissions_block_is_leaf_only_and_absent_emits_nothing` in
//! builder_tests.rs; `stock_tcode_roster_deploys_every_frontmatter_key` in
//! deployer_tests.rs.

/// Line-by-line capture of one top-level `permissions:` block.
#[derive(Debug, Default)]
pub(crate) struct PermissionsBlock {
    lines: Vec<String>,
    open: bool,
}

impl PermissionsBlock {
    /// Offer one frontmatter line; `true` when it belongs to the block.
    ///
    /// Why: the caller must skip every line the block owns, so none reaches
    /// the flat key parser, and must still parse the line that ends it.
    /// What: a line starting `permissions:` at column 0 opens the block,
    /// replacing any earlier one. While open, blank and indented lines are
    /// recorded; any other line closes the block and returns `false`.
    /// Test: `permissions_block_survives_compose_verbatim`.
    pub(crate) fn consume(&mut self, line: &str) -> bool {
        if self.open {
            if line.trim().is_empty() || line.starts_with([' ', '\t']) {
                self.lines.push(line.to_string());
                return true;
            }
            self.open = false;
        }
        if line.starts_with("permissions:") {
            self.lines = vec![line.to_string()];
            self.open = true;
            return true;
        }
        false
    }

    /// The captured block with trailing blank lines dropped, each line
    /// newline-terminated; `None` when the document declared no block.
    /// Test: `permissions_block_is_leaf_only_and_absent_emits_nothing`.
    pub(crate) fn finish(mut self) -> Option<String> {
        while self.lines.last().is_some_and(|l| l.trim().is_empty()) {
            self.lines.pop();
        }
        (!self.lines.is_empty()).then(|| self.lines.iter().map(|l| format!("{l}\n")).collect())
    }
}
