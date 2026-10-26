//! How many rotated `stderr.log` archives to keep (issue #10297).
//!
//! Why: the count was the constant `ROTATION_KEEP = 7`, and `doctor --fix`
//! rewrote the newsyslog conf from it, so an operator who wanted more history
//! lost the edit on the next fix.
//! What: the pure rules for choosing the count. Order: `log_rotation.keep` in
//! `config.yaml`, then the count already in the installed newsyslog conf, then
//! the default of 7. A value outside `1..=MAX_KEEP` is an error, never a
//! silent fallback.
//! Test: the tests in this module, plus `write_rotation_files_*` in
//! `log_rotation.rs` for the `doctor --fix` write path.

/// Archives kept when nothing is configured.
pub const DEFAULT_KEEP: u32 = 7;

/// Largest accepted keep count. At ~1 MiB per compressed archive this is
/// "unbounded in practice" for an operator who wants to keep everything,
/// while still rejecting a typo such as `10000000`.
pub const MAX_KEEP: u32 = 1000;

/// Check one candidate keep count.
///
/// What: `Ok` for `1..=MAX_KEEP`; otherwise an error naming the value and the
/// accepted range. Zero is refused because newsyslog treats a count of 0 as
/// "keep no archives", which is the opposite of what a keep setting asks for.
/// Test: `validate_keep_rejects_out_of_range`.
pub fn validate_keep(n: i64) -> Result<u32, String> {
    match u32::try_from(n) {
        Ok(v) if (1..=MAX_KEEP).contains(&v) => Ok(v),
        _ => Err(format!(
            "log_rotation.keep = {n} is invalid; use a whole number from 1 to {MAX_KEEP}"
        )),
    }
}

/// The count on the first data line of a newsyslog conf, when it is valid.
///
/// What: column 3 of the first non-comment line. A missing, non-numeric or
/// out-of-range count yields `None`, so a damaged conf falls through to the
/// default instead of propagating the damage.
/// Test: `keep_from_conf_reads_the_count_column`.
pub fn keep_from_conf(conf: &str) -> Option<u32> {
    let line = conf.lines().find(|l| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    })?;
    let n: i64 = line.split_whitespace().nth(2)?.parse().ok()?;
    validate_keep(n).ok()
}

/// Choose the keep count.
///
/// Why: `doctor --fix` must keep what the operator set, not reset it to 7.
/// What: a configured value wins (and must be valid); else the installed
/// conf's count; else `DEFAULT_KEEP`.
/// Test: `resolve_keep_default`, `resolve_keep_configured_wins`,
/// `resolve_keep_uses_installed_when_unconfigured`,
/// `validate_keep_rejects_out_of_range`.
pub fn resolve_keep(configured: Option<i64>, installed_conf: Option<&str>) -> Result<u32, String> {
    match configured {
        Some(n) => validate_keep(n),
        None => Ok(installed_conf
            .and_then(keep_from_conf)
            .unwrap_or(DEFAULT_KEEP)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_keep_default() {
        assert_eq!(resolve_keep(None, None), Ok(7));
    }

    #[test]
    fn resolve_keep_configured_wins() {
        let conf = "/x/stderr.log    644  45  1024  $D0  JN\n";
        assert_eq!(resolve_keep(Some(30), Some(conf)), Ok(30));
        assert_eq!(resolve_keep(Some(1000), None), Ok(1000));
    }

    #[test]
    fn resolve_keep_uses_installed_when_unconfigured() {
        let conf = "# header\n/x/stderr.log    644  45  1024  $D0  JN\n";
        assert_eq!(resolve_keep(None, Some(conf)), Ok(45));
    }

    #[test]
    fn validate_keep_rejects_out_of_range() {
        for bad in [0, -1, i64::from(MAX_KEEP) + 1, i64::MAX] {
            let err = validate_keep(bad).expect_err("must reject");
            assert!(err.contains("log_rotation.keep"), "{err}");
            assert!(resolve_keep(Some(bad), None).is_err());
        }
    }

    #[test]
    fn keep_from_conf_reads_the_count_column() {
        assert_eq!(keep_from_conf("/x 644 12 1024 $D0 JN"), Some(12));
        for junk in [
            "",
            "# only a comment",
            "/x 644 abc 1024 $D0 JN",
            "/x 644 0 1 $D0 JN",
        ] {
            assert_eq!(keep_from_conf(junk), None, "{junk}");
        }
    }
}
