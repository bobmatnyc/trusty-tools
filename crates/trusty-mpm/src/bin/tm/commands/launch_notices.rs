//! Asset-notice output for `tm launch` and `tm connect` (#8406).

/// Writes one `  assets: <notice>` line per unclean-asset notice to `out`.
///
/// Why: #8406 — `tm launch --detach` promises exactly one `session=… pane=…`
/// stdout line, so the diagnostic notice must never reach stdout.
/// What: callers pass stderr; the writer parameter lets a test observe the text.
/// Test: `asset_notices_are_written_to_the_given_writer`,
/// `launch_never_prints_asset_notices_to_stdout`.
pub(super) fn write_asset_notices(out: &mut impl std::io::Write, notices: &[String]) {
    notices
        .iter()
        .for_each(|n| drop(writeln!(out, "  assets: {n}")));
}

/// Writes the notices to stderr (#8406); the launch and connect paths call this.
pub(super) fn emit_asset_notices(notices: &[String]) {
    write_asset_notices(&mut std::io::stderr(), notices);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notice writer emits one prefixed line per notice (#8406).
    /// Test: itself.
    #[test]
    fn asset_notices_are_written_to_the_given_writer() {
        let mut buf = Vec::new();
        write_asset_notices(&mut buf, &["a".to_string(), "b".to_string()]);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "  assets: a\n  assets: b\n"
        );
    }

    /// No launch path prints asset notices to stdout (#8406).
    ///
    /// Why: a stdout notice made `--detach` print a second line on an unclean
    /// worktree; no deterministic run exists without a real tmux, so this scans
    /// the source for the stdout form.
    /// Test: itself.
    #[test]
    fn launch_never_prints_asset_notices_to_stdout() {
        let src = include_str!("launch.rs");
        let needle = ["println!(\"  ", "assets:"].concat();
        assert!(!src.contains(&needle), "asset notices must go to stderr");
    }
}
