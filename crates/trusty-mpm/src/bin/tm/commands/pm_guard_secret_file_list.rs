//! The #9791 rule: `file` told to read a secret-bearing file as a list.
//!
//! Red-first stub: the tests compile against it and fail; the fix commit
//! fills in [`file_list_flag_text`].
//! Test: `pm_guard_secret_file_list_tests.rs`.

/// The #9791 deny for a Bash `command`; the stub allows everything.
pub(crate) fn file_list_flag_text(_command: &str) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "pm_guard_secret_file_list_tests.rs"]
mod tests;
