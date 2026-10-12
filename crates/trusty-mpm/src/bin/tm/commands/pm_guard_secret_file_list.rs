//! The #9791 rule: `file` told to read a secret-bearing file as a list.
//!
//! Why: `file -f NAMEFILE` reads NAMEFILE as a list of names and `file -m
//! MAGICFILE` parses it as magic rules; both echo every line they cannot use
//! in their error output, so `file -f .env` prints each credential the file
//! holds. [`SAFE_HANDLING_VERBS`] grants `file` by its name and never reads
//! its flags. Owner ruling D5b: "Mistakes only; switch to a text rule."
//! What: [`file_list_flag_text`] reads the whole Bash command as normalised
//! text, the way [`trusty_memory_kuzu_text`] does, so a wrapper, a pipe, a
//! redirection or a quote join changes nothing. Plain `file .env` is untouched.
//! Test: `pm_guard_secret_file_list_tests.rs` —
//! `the_list_flags_deny_on_a_secret_file`, `plain_file_and_non_secret_lists_allow`,
//! `quoted_text_slots_are_exempt`.
//!
//! [`SAFE_HANDLING_VERBS`]: crate::commands::pm_guard_secret_read::SAFE_HANDLING_VERBS
//! [`trusty_memory_kuzu_text`]: crate::commands::pm_guard_kuzu_import::trusty_memory_kuzu_text

use crate::commands::pm_guard_kuzu_import::without_text_slots;
use crate::commands::pm_guard_secret_read::{Scan, command_basename, secret_files_named_in};

/// The program whose list-reading flags this rule refuses.
const FILE_PROGRAM: &str = "file";

/// `file`'s long options that read their operand as a list or a magic file.
const LIST_LONG_FLAGS: &[&str] = &["--files-from", "--magic-file"];

/// The short options that do the same: `-f` (names) and `-m` (magic).
const LIST_SHORT_FLAGS: [char; 2] = ['f', 'm'];

/// The #9791 deny for a Bash `command` that runs `file -f`/`-m` on a
/// secret-bearing file; `None` when it does not.
///
/// Why: see the module doc.
/// What: [`without_text_slots`] removes each quoted `--body`, `--title`,
/// `--message`, `--note` or `-m` value; the text then loses every `'`, `"`,
/// `$` and `\` and is cut at `;`, `|`, `&` and newlines. A segment denies
/// when it holds a word whose basename is `file` (any case), a later
/// list-reading flag ([`reads_a_list`]), and a later word in the #7266
/// secret-bearing class. Flags compare case-sensitively: `-F` is the
/// separator option, not `-f`.
/// Test: `the_list_flags_deny_on_a_secret_file`,
/// `wrappers_redirections_and_quote_joins_still_deny`,
/// `plain_file_and_non_secret_lists_allow`, `quoted_text_slots_are_exempt`,
/// `a_lone_echo_of_the_call_denies_with_the_body_file_hint`.
pub(crate) fn file_list_flag_text(command: &str) -> Option<String> {
    // #9791 (D5b): the threat model is mistakes only, not deliberate evasion.
    let normal: String = without_text_slots(command)
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '$' | '\\'))
        .collect();
    normal
        .split([';', '|', '&', '\n'])
        .find_map(secret_list_operand)
        .map(|name| deny_reason(&name))
}

/// The first secret-bearing name a `file` list flag in `segment` would read.
fn secret_list_operand(segment: &str) -> Option<String> {
    let mut words = segment.split_whitespace();
    words.find(|w| command_basename(w).eq_ignore_ascii_case(FILE_PROGRAM))?;
    let mut flagged = false;
    let mut scanned: Vec<&str> = Vec::new();
    for word in words {
        if let Some(glued) = reads_a_list(word) {
            flagged = true;
            // #9791: `-f.env` glues the operand to the flag.
            scanned.push(glued);
        }
        scanned.push(word);
    }
    if !flagged {
        return None;
    }
    secret_files_named_in(&scanned.join(" "), Scan::Argv)
        .into_iter()
        .next()
}

/// Whether `word` is a `file` flag that reads its operand as a list.
///
/// What: `Some` for an exact [`LIST_LONG_FLAGS`] spelling or its `=` form,
/// and for a single-dash cluster carrying a [`LIST_SHORT_FLAGS`] letter
/// (`-f`, `-bf`, `-fb`, `-m`, `-bm`). The value is whatever follows the
/// letter in the same word (`-f.env`), empty when the operand is the next
/// word; the `=` form's value is split off by the classifier itself.
fn reads_a_list(word: &str) -> Option<&str> {
    if word.starts_with("--") {
        let long = LIST_LONG_FLAGS.iter().any(|flag| {
            word.strip_prefix(flag)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
        });
        return long.then_some("");
    }
    let cluster = word.strip_prefix('-')?;
    let at = cluster.find(LIST_SHORT_FLAGS)?;
    Some(&cluster[at + 1..])
}

/// The deny text for the #9791 rule, naming the secret-bearing `name`.
fn deny_reason(name: &str) -> String {
    format!(
        "secret-file-read: `file -f`/`-m` reads `{name}`, a secret-bearing file, as a list of \
         names or magic rules and echoes every line in its error output (#9791). The same holds \
         for `--files-from` and `--magic-file`. Plain `file {name}` is allowed and prints only \
         the type. To write about the call, quote it in a `--body`, `--title`, `--message`, \
         `--note` or `-m` value, or use `--body-file` with a file the Write tool made."
    )
}

#[cfg(test)]
#[path = "pm_guard_secret_file_list_tests.rs"]
mod tests;
