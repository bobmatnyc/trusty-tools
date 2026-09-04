//! CWE weakness-class tagging for investigation findings (#6779).
//!
//! Why: findings in `investigation.json` carry a DD `dimension`, a severity and
//! prose, and nothing that names a weakness CLASS. ISO-5055-style structural-flaw
//! counting needs one, and the six dimensions cannot supply it — "authentication
//! & secrets" covers hardcoded credentials (CWE-798), missing authentication
//! (CWE-306) and weak crypto (CWE-327), and nothing in the finding picks between
//! them. Deriving a CWE id from the dimension alone would be a guess, and #6779
//! says never guess.
//!
//! So the weakness classes come from the model, which is the only party that saw
//! the code, and this module is the ingestion gate on that field: an id the model
//! emits is admitted only when it is well formed, and a class NAME the model
//! emits instead of an id is resolved through [`WEAKNESS_CLASSES`]. Everything
//! else is dropped, and an all-dropped list leaves the finding with an empty
//! `cwe_id` that never reaches `investigation.json` at all.
//!
//! The field is a LIST because one finding can violate more than one weakness
//! class — a hardcoded credential logged on the error path is CWE-798 and
//! CWE-532 at once, and picking one of the two would discard a real fact.
//!
//! What: [`resolve_all`], the entry point ingestion calls; [`class_checklist`],
//! which renders the same table into the prompt so the model chooses from the
//! set this module can read back rather than inventing a spelling. One table,
//! two consumers, no drift.
//!
//! Test: `super::cwe_tests`.

/// Weakness classes this crate can name a CWE id for, and the id for each.
///
/// Why: the model is asked for a `cwe_id`, and models answer that field with a
/// class name ("hardcoded credentials") about as readily as with an id. Both
/// spellings are the same claim, so both are accepted — the table is what makes
/// the name branch deterministic instead of a second guess. It is also the
/// enumeration the prompt shows (see [`class_checklist`]), which is what keeps
/// the model's vocabulary and this reader's vocabulary the same one.
///
/// What: the left column is the canonical class key — lower case, spaces, the
/// shape [`canonical_key`] reduces an incoming label to. The right column is the
/// CWE id. Several keys map to one id on purpose: they are alternate names for
/// one class, not distinct classes.
///
/// Coverage is the classes #6779 named, plus CWE-390 from the issue's own
/// missing-error-handling example. It is deliberately not the full CWE corpus:
/// a class absent here still reaches the finding when the model spells the id
/// itself, because [`resolve`] admits any well-formed id.
const WEAKNESS_CLASSES: &[(&str, &str)] = &[
    // Injection.
    ("injection", "CWE-74"),
    ("sql injection", "CWE-89"),
    ("command injection", "CWE-78"),
    ("os command injection", "CWE-78"),
    ("template injection", "CWE-1336"),
    ("cross site scripting", "CWE-79"),
    ("xss", "CWE-79"),
    ("cross site scripting xss", "CWE-79"),
    // Path handling.
    ("path traversal", "CWE-22"),
    ("directory traversal", "CWE-22"),
    // Credentials and secrets.
    ("hardcoded credentials", "CWE-798"),
    ("hardcoded credential", "CWE-798"),
    ("hardcoded secret", "CWE-798"),
    ("hardcoded secrets", "CWE-798"),
    // Deserialization.
    ("insecure deserialization", "CWE-502"),
    ("unsafe deserialization", "CWE-502"),
    // Access control.
    ("missing authentication", "CWE-306"),
    ("missing authorization", "CWE-862"),
    ("improper authorization", "CWE-285"),
    // Request forgery.
    ("ssrf", "CWE-918"),
    ("server side request forgery", "CWE-918"),
    ("server side request forgery ssrf", "CWE-918"),
    // Cryptography.
    ("weak crypto", "CWE-327"),
    ("weak cryptography", "CWE-327"),
    ("broken cryptographic algorithm", "CWE-327"),
    // Input handling.
    ("improper input validation", "CWE-20"),
    ("missing input validation", "CWE-20"),
    // Concurrency.
    ("race condition", "CWE-362"),
    ("toctou", "CWE-367"),
    ("time of check time of use", "CWE-367"),
    // Resource limits.
    ("resource exhaustion", "CWE-400"),
    ("uncontrolled resource consumption", "CWE-400"),
    ("unbounded allocation", "CWE-789"),
    // Information exposure.
    ("information exposure through logs", "CWE-532"),
    ("sensitive information in logs", "CWE-532"),
    ("information exposure through an error message", "CWE-209"),
    ("information exposure through error messages", "CWE-209"),
    // Error handling (the issue's own worked example).
    ("missing error handling", "CWE-390"),
    ("unchecked error condition", "CWE-390"),
];

/// Resolve every weakness class the model declared, dropping what it cannot.
///
/// Why: this is the single gate #6779's "never guessed" rule is enforced at, and
/// it is per element so one unreadable entry costs only itself — a list of
/// `["CWE-798", "not a class"]` still tags the finding CWE-798 rather than
/// losing both. The finding itself is never rejected for this field.
///
/// What: [`resolve`] on each entry, dropping every `None`, then de-duplicating
/// while keeping first-seen order (two spellings of one class — `cwe-79` and
/// `Cross-Site Scripting` — must not render as two tags). A list whose every
/// entry drops returns empty, which the caller serialises as no field at all.
///
/// # Postconditions
/// Every returned id matches `^CWE-\d+$` and appears once. The result is never
/// longer than `raw`.
///
/// Test: `super::cwe_tests::{several_classes_survive_together,
/// duplicate_spellings_collapse_to_one_tag,
/// an_all_unresolvable_list_becomes_empty}`.
pub fn resolve_all(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in raw {
        if let Some(id) = resolve(entry)
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

/// Resolve ONE declared weakness class into a CWE id, or `None`.
///
/// Why: the per-element half of [`resolve_all`], kept separate because the two
/// admission branches are the part worth testing directly.
///
/// What: two branches, in order. A well-formed `CWE-<digits>` id is admitted and
/// upper-cased, whatever case the model used. Otherwise the text is reduced by
/// [`canonical_key`] and looked up in [`WEAKNESS_CLASSES`]. Empty, malformed
/// (`CWE-`, `cwe89`, `SQL-89`) and unknown-class input all return `None` —
/// dropped, never repaired into a neighbouring id.
///
/// # Postconditions
/// A `Some` value always matches `^CWE-\d+$`: the id branch checks that shape
/// directly and the class branch returns a [`WEAKNESS_CLASSES`] value, which
/// `every_table_id_is_well_formed` holds to the same shape.
///
/// Test: `super::cwe_tests::{a_well_formed_id_is_admitted_and_upper_cased,
/// a_malformed_id_is_dropped, the_class_table_round_trips_a_sample,
/// every_table_id_is_well_formed}`.
fn resolve(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(id) = well_formed_id(trimmed) {
        return Some(id);
    }
    let key = canonical_key(trimmed);
    WEAKNESS_CLASSES
        .iter()
        .find(|(class, _)| *class == key)
        .map(|(_, id)| (*id).to_string())
}

/// `Some("CWE-<digits>")` when `raw` already IS an id, else `None`.
///
/// The `^CWE-\d+$` check, spelled without the `regex` crate — the shape is three
/// conditions and pulling a dependency in for it would be the heavier answer.
/// The prefix match is case-insensitive because models write `cwe-79` freely;
/// the returned id is always upper-case so two spellings never render as two
/// different tags.
fn well_formed_id(raw: &str) -> Option<String> {
    let (prefix, digits) = raw.split_at_checked(4)?;
    if !prefix.eq_ignore_ascii_case("CWE-") {
        return None;
    }
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("CWE-{digits}"))
}

/// Reduce a class label to the spelling [`WEAKNESS_CLASSES`] keys use.
///
/// Lower case, every non-alphanumeric run collapsed to one space, trimmed — so
/// `"Cross-Site Scripting (XSS)"`, `"cross-site scripting (xss)"` and
/// `"Cross Site Scripting XSS"` are one key, and punctuation the model adds
/// cannot miss a row that is present. Words are NOT dropped: a parenthesised
/// acronym is part of the label, so that key is its own row in the table rather
/// than something this function guesses away.
fn canonical_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
}

/// The class enumeration the investigation prompt shows the model.
///
/// Why: a model told only "emit a CWE id" invents both ids and spellings. Naming
/// the classes this crate reads back — from the same table it reads them back
/// WITH — makes the field mechanically resolvable instead of hopefully so.
/// What: one `class (CWE-n)` entry per DISTINCT id, in table order, comma
/// separated. Alternate names for an id are omitted: they exist so an incoming
/// label resolves, and repeating them in the prompt would only spend tokens.
/// Test: `super::cwe_tests::the_checklist_names_each_id_once`.
pub fn class_checklist() -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for (class, id) in WEAKNESS_CLASSES {
        if seen.contains(id) {
            continue;
        }
        seen.push(id);
        out.push(format!("{class} ({id})"));
    }
    out.join(", ")
}

#[cfg(test)]
#[path = "cwe_tests.rs"]
mod cwe_tests;
