//! The option labels a question may carry (#10127, S5 P2a).
//!
//! Why: an answer to an option question is matched against labels the
//! Architect wrote (P2b), so a label set must leave every reply at most one
//! reading. Architect ruling Q5 (df-Da7): 2 to 6 options, each 1 to 80
//! bytes, no control character, unique after case fold, and none equal to
//! another option's number.
//! What: the bounds and [`validate_options`]. `ask_owner` refuses a set it
//! rejects before anything is reserved or posted, and the table's replay
//! fails the open on a stored set it rejects.
//! Test: `ask_with_one_or_seven_options_is_refused`,
//! `ask_with_duplicate_after_case_fold_or_numeric_label_is_refused`,
//! `ask_with_overlong_or_control_char_label_is_refused`,
//! `stored_option_set_breaking_the_rules_fails_the_open`.

/// The fewest options a question may carry.
pub const MIN_OPTIONS: usize = 2;

/// The most options a question may carry.
pub const MAX_OPTIONS: usize = 6;

/// The longest option label accepted, in bytes.
pub const MAX_OPTION_BYTES: usize = 80;

/// Check `options` against the Q5 rules; the error is a content-free reason.
///
/// Why: see the module doc. One function serves the ask and the replay, so
/// a log a correct daemon never writes fails the open (#8433 r6 G5).
/// What: refuses a count outside `MIN_OPTIONS..=MAX_OPTIONS`, a label that
/// is empty, blank or over `MAX_OPTION_BYTES` bytes, a label holding a
/// control character, two labels equal once trimmed and lower-cased, and a
/// label that parses as the number of a different option. A label equal to
/// its own number is allowed: a reply naming it reads one way either way.
/// Test: the module doc's tests.
pub fn validate_options(options: &[String]) -> Result<(), &'static str> {
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
        return Err("options must number 2 to 6");
    }
    let mut keys: Vec<String> = Vec::with_capacity(options.len());
    for (index, label) in options.iter().enumerate() {
        if label.is_empty() || label.len() > MAX_OPTION_BYTES {
            return Err("an option label must be 1 to 80 bytes");
        }
        if label.chars().any(char::is_control) {
            return Err("an option label holds a control character");
        }
        let key = option_key(label);
        if key.is_empty() {
            return Err("an option label is blank");
        }
        if keys.contains(&key) {
            return Err("two option labels are equal after case fold");
        }
        // #10127 Q5: `parse` also reads `02` and `+2`, the forms a number
        // match could take.
        if let Ok(number) = key.parse::<usize>()
            && number != index + 1
            && (1..=options.len()).contains(&number)
        {
            return Err("an option label names another option's number");
        }
        keys.push(key);
    }
    Ok(())
}

/// The form two labels are compared in: trimmed, then lower-cased.
fn option_key(label: &str) -> String {
    label.trim().to_lowercase()
}
