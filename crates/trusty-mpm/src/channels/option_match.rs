//! Reading an owner's reply as one option of a closed-option question
//! (#10127, S5 P2b).
//!
//! Why: a reply is untrusted text. Architect ruling df-Da7: a reply reaches
//! the asker only as the option it names, typed from the stored question, so
//! no reply byte reaches the pane and nothing is tainted. Any other reply,
//! and every reply to a question with no options (Q2 c), is held.
//! What: [`match_reply`] reads a reply as one [`Choice`] or none, under the
//! P2a label rules (Q5); [`HoldReason`] names why a reply is held. The
//! decision that acts on them is `crate::channels::inbound::decide`.
//! Test: `reply_equal_to_a_label_matches_that_option`,
//! `reply_equal_to_a_number_matches_that_option`,
//! `match_folds_case_and_trims_whitespace`,
//! `number_out_of_range_or_label_with_extra_text_does_not_match`.

use crate::channels::question_table::option_key;

/// The option a reply named: its number from 1 and its stored label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    number: usize,
    label: String,
}

impl Choice {
    /// The option's number, from 1.
    pub fn number(&self) -> usize {
        self.number
    }

    /// The option's label as the asker stored it, never the reply's text.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// The option of `options` that `reply` names, if exactly one does.
///
/// Why: the label rules of P2a (`validate_options`) leave every reply at
/// most one reading, so a match is never a guess.
/// What: `reply` trimmed and lower-cased equals one label's same form, or
/// parses as a number from 1 to the option count (`2`, `02` and `+2` alike,
/// the forms the label rules already refuse as a label). Anything else is
/// `None`: a number out of range, a label with extra text, and every reply
/// when `options` is empty. The [`Choice`] copies the stored label.
/// Test: the module doc's tests.
pub fn match_reply(options: &[String], reply: &str) -> Option<Choice> {
    let key = option_key(reply);
    let index = options
        .iter()
        .position(|label| option_key(label) == key)
        .or_else(|| {
            key.parse::<usize>()
                .ok()
                .filter(|n| (1..=options.len()).contains(n))
                .map(|n| n - 1)
        })?;
    Some(Choice {
        number: index + 1,
        label: options[index].clone(),
    })
}

/// Why a reply was held instead of delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// The question has options and the reply named none of them.
    Unmatched,
    /// The question has no options, so no reply is delivered (df-Da7 Q2 c).
    FreeText,
}

impl HoldReason {
    /// The reason for a reply that matched nothing in `options`.
    pub fn for_options(options: &[String]) -> Self {
        if options.is_empty() {
            Self::FreeText
        } else {
            Self::Unmatched
        }
    }

    /// The audit reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Unmatched => "option_unmatched",
            Self::FreeText => "free_text_held",
        }
    }
}
