//! Section-level deduplication for prompts.
//!
//! Why: System prompts assembled from multiple skills/templates can include
//! the same `## Heading` block twice. Sending it twice wastes tokens and may
//! confuse the model.
//! What: Splits on `\n## ` boundaries, hashes each section's content with
//! FNV-1a, and drops repeats while preserving first occurrence order.
//! Test: See module-level `tests` — covers no-duplicates, exact-duplicate,
//! multiple duplicates, order preservation, and edge cases.

use std::collections::HashSet;

/// Deduplicate `## Heading` sections in a prompt.
///
/// Why: Concatenated skill bundles often contain repeats; one pass of
/// hash-based dedup is cheap and catches the common case.
/// What: Splits on `\n## ` (preserving the leading prefix before the first
/// section). Computes FNV-1a over each section's body. Drops sections whose
/// hash has been seen. Returns the rebuilt prompt. If no duplicates were
/// found, returns the original string unchanged.
/// Test: `dedup_*` cases in `tests` module.
pub fn dedup_sections(prompt: &str) -> String {
    if prompt.is_empty() {
        return String::new();
    }
    // Find boundaries: the first section may not have `## ` at the start;
    // `\n## ` is the canonical splitter.
    let parts: Vec<&str> = split_keep_delim(prompt, "\n## ");
    if parts.len() <= 1 {
        // Only one section (or none) — nothing to dedup.
        return prompt.to_string();
    }
    let mut seen: HashSet<u64> = HashSet::new();
    let mut kept: Vec<&str> = Vec::with_capacity(parts.len());
    let mut had_duplicate = false;
    // The first part may itself be a section if the prompt starts with `## `;
    // otherwise it's a prefix and is always kept.
    let first_is_section = prompt.starts_with("## ");
    for (i, part) in parts.iter().enumerate() {
        if i == 0 && !first_is_section {
            kept.push(part);
            continue;
        }
        // Normalize trailing whitespace and leading delimiter for hashing so
        // sections that differ only by leading `\n` or trailing `\n` still
        // match.
        let normalized = part.trim_start_matches('\n').trim_end();
        let h = fnv1a(normalized);
        if seen.insert(h) {
            kept.push(part);
        } else {
            had_duplicate = true;
        }
    }
    if !had_duplicate {
        return prompt.to_string();
    }
    let mut result = kept.concat();
    // Preserve trailing newline if the original had one, since it may have
    // been carried only on the last (now-dropped) duplicate section.
    if prompt.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Split `s` on `delim`, retaining `delim` as a prefix on each non-first part.
///
/// Why: We need to reconstruct the original string from kept parts; the
/// standard `split` loses the delimiter.
/// What: Returns parts where `out[0]` is the prefix before the first `delim`
/// (possibly empty) and each subsequent part begins with `delim`.
fn split_keep_delim<'a>(s: &'a str, delim: &str) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::new();
    let mut cursor = 0usize;
    let mut first = true;
    while cursor <= s.len() {
        // For the first segment, search from cursor=0.
        // For subsequent segments, search starting AFTER the current delim
        // (cursor + delim.len()) so we don't immediately re-match it.
        let search_start = if first { cursor } else { cursor + delim.len() };
        if search_start > s.len() {
            // Last segment is just the remainder.
            out.push(&s[cursor..]);
            break;
        }
        match s[search_start..].find(delim) {
            Some(rel) => {
                let abs = search_start + rel;
                out.push(&s[cursor..abs]);
                cursor = abs;
                first = false;
            }
            None => {
                out.push(&s[cursor..]);
                break;
            }
        }
    }
    out
}

/// FNV-1a 64-bit hash.
///
/// Why: Cheap, dependency-free hash sufficient for in-process section dedup.
/// What: Standard FNV-1a over the byte stream.
/// Test: Indirect via dedup tests.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_no_duplicates_unchanged() {
        let input = "preamble\n## Section A\nbody A\n## Section B\nbody B\n";
        let out = dedup_sections(input);
        assert_eq!(out, input);
    }

    #[test]
    fn dedup_exact_duplicate_section_dropped() {
        let input = "preamble\n## Section A\nbody A\n## Section A\nbody A\n";
        let out = dedup_sections(input);
        // Second occurrence of Section A is dropped.
        assert_eq!(out, "preamble\n## Section A\nbody A\n");
    }

    #[test]
    fn dedup_keeps_first_occurrence() {
        let input = "## A\nfirst body\n## A\nfirst body\n## B\nb body\n";
        let out = dedup_sections(input);
        assert!(out.contains("first body"));
        assert!(out.contains("b body"));
        // Only one occurrence of "A\nfirst body" segment.
        let count = out.matches("## A\nfirst body").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn dedup_multiple_duplicates() {
        let input = "## A\nA body\n## B\nB body\n## A\nA body\n## C\nC body\n## B\nB body\n";
        let out = dedup_sections(input);
        assert_eq!(out.matches("A body").count(), 1);
        assert_eq!(out.matches("B body").count(), 1);
        assert_eq!(out.matches("C body").count(), 1);
    }

    #[test]
    fn dedup_preserves_order() {
        let input = "## A\na\n## B\nb\n## C\nc\n## B\nb\n## D\nd\n";
        let out = dedup_sections(input);
        let ai = out.find("## A").unwrap();
        let bi = out.find("## B").unwrap();
        let ci = out.find("## C").unwrap();
        let di = out.find("## D").unwrap();
        assert!(ai < bi);
        assert!(bi < ci);
        assert!(ci < di);
    }

    #[test]
    fn dedup_empty_string() {
        let out = dedup_sections("");
        assert_eq!(out, "");
    }

    #[test]
    fn dedup_no_sections() {
        let input = "this prompt has no h2 headings at all\njust plain text\n";
        let out = dedup_sections(input);
        assert_eq!(out, input);
    }

    #[test]
    fn dedup_whitespace_only_sections() {
        let input = "## A\n   \n## A\n   \n## B\nreal\n";
        let out = dedup_sections(input);
        assert_eq!(out.matches("## A").count(), 1);
        assert!(out.contains("## B"));
    }

    #[test]
    fn dedup_returns_original_when_no_dups() {
        // Verify the "return original unchanged" guarantee preserves trailing
        // newlines, etc.
        let input = "## X\nhello\n";
        let out = dedup_sections(input);
        assert_eq!(out, input);
    }
}
