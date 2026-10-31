//! The durable retirement marker a Tier C retiree carries (#10609, ADR-0074).
//!
//! Why: default recall must leave out a fact a newer fact replaced. The
//! `superseded_by` KG edge says so, but reading it takes a KG lookup with a
//! time budget, and a lookup that fails or runs late returns the retiree as if
//! it were current, which is the #10609 failure. A slot retirement already
//! rewrites the incumbent in one atomic commit, so the marker rides in that
//! same write and recall reads it off the drawer row with no lookup.
//! What: one reserved tag, `superseded-by:<replacement uuid>`, on the retired
//! drawer. A tag, not a new row field: drawer rows are positional postcard, so
//! a new field would need a palace-format bump (ADR-0067 D3) and would make an
//! older 1.x binary unable to decode any rewritten row. An older binary reads
//! this as an ordinary tag. [`Drawer::retired_by`] reads it,
//! [`Drawer::mark_retired_by`] writes it, [`is_retirement_tag`] names the
//! namespace so a dedup merge never copies it onto a live survivor.
//! The namespace is reserved: [`strip_retirement_tags`] removes it from
//! caller-supplied tags at write time, so only [`Drawer::mark_retired_by`]
//! ever adds it. Strip, not reject: an agent that re-saves the tags of a
//! recalled historical row would otherwise see its write fail, and the stripped
//! tag was never the caller's to set. The strip is logged at WARN.
//! Test: the inline `tests` module; the commit-time stamp in
//! `retrieval::tier_c_tests::a_retired_slot_incumbent_carries_a_superseded_by_marker`.

use uuid::Uuid;

use super::palace::Drawer;

/// Tag namespace of the retirement marker: `superseded-by:<uuid>`.
pub(crate) const RETIRED_BY_TAG_PREFIX: &str = "superseded-by:";

/// Whether `tag` is a retirement marker (any value after the prefix).
///
/// Why: the marker belongs to one drawer. A merge that unions tags must skip it.
/// Test: `only_the_marker_namespace_is_a_retirement_tag`.
pub(crate) fn is_retirement_tag(tag: &str) -> bool {
    tag.starts_with(RETIRED_BY_TAG_PREFIX)
}

/// Remove every retirement-marker tag from caller-supplied `tags`.
///
/// Why: the marker hides a drawer from default recall. A caller-supplied
/// `superseded-by:<uuid>` would hide a live drawer, and re-saving the tags of a
/// recalled historical row would hide the new memory (#10609 review).
/// What: drops the tags [`is_retirement_tag`] names, WARNs once with the count
/// and the palace, and returns the count removed. Nothing else is touched.
/// Test: `user_supplied_marker_tags_are_stripped`.
pub(crate) fn strip_retirement_tags(tags: &mut Vec<String>, palace: &str) -> usize {
    let before = tags.len();
    tags.retain(|t| !is_retirement_tag(t));
    let removed = before - tags.len();
    if removed > 0 {
        tracing::warn!(
            palace,
            removed,
            "#10609: `{RETIRED_BY_TAG_PREFIX}*` is a reserved tag namespace; \
             dropped from the supplied tags"
        );
    }
    removed
}

impl Drawer {
    /// The drawer that replaced this one, when it carries a retirement marker.
    ///
    /// Why: a local read, so recall can exclude a retiree with no KG lookup.
    /// What: the first marker tag whose value parses as a UUID. A marker whose
    /// value is not a UUID is ignored: it names no replacement, so it hides
    /// nothing.
    /// Test: `a_marked_drawer_reports_its_replacement`,
    /// `a_marker_with_no_uuid_hides_nothing`.
    pub(crate) fn retired_by(&self) -> Option<Uuid> {
        self.tags
            .iter()
            .filter_map(|t| t.strip_prefix(RETIRED_BY_TAG_PREFIX))
            .find_map(|v| Uuid::parse_str(v).ok())
    }

    /// Stamp the marker naming `replacement`, replacing any earlier one.
    ///
    /// Why: written on the incumbent inside the retirement's atomic commit.
    /// What: removes every marker tag, then appends the new one, so a drawer
    /// never carries two replacements.
    /// Test: `marking_twice_keeps_one_marker`.
    pub(crate) fn mark_retired_by(&mut self, replacement: Uuid) {
        self.tags.retain(|t| !is_retirement_tag(t));
        self.tags
            .push(format!("{RETIRED_BY_TAG_PREFIX}{replacement}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawer() -> Drawer {
        Drawer::new(Uuid::new_v4(), "fact")
    }

    #[test]
    fn a_marked_drawer_reports_its_replacement() {
        let (mut d, new) = (drawer(), Uuid::new_v4());
        assert_eq!(d.retired_by(), None);
        d.mark_retired_by(new);
        assert_eq!(d.retired_by(), Some(new));
    }

    #[test]
    fn marking_twice_keeps_one_marker() {
        let (mut d, first, second) = (drawer(), Uuid::new_v4(), Uuid::new_v4());
        d.tags.push("keep".into());
        d.mark_retired_by(first);
        d.mark_retired_by(second);
        assert_eq!(d.retired_by(), Some(second));
        assert_eq!(d.tags.iter().filter(|t| is_retirement_tag(t)).count(), 1);
        assert!(d.tags.contains(&"keep".to_string()));
    }

    #[test]
    fn a_marker_with_no_uuid_hides_nothing() {
        let mut d = drawer();
        d.tags.push(format!("{RETIRED_BY_TAG_PREFIX}not-a-uuid"));
        assert_eq!(d.retired_by(), None);
    }

    #[test]
    fn user_supplied_marker_tags_are_stripped() {
        let mut tags = vec![
            "keep".to_string(),
            format!("{RETIRED_BY_TAG_PREFIX}{}", Uuid::new_v4()),
            format!("{RETIRED_BY_TAG_PREFIX}not-a-uuid"),
        ];
        assert_eq!(strip_retirement_tags(&mut tags, "p"), 2);
        assert_eq!(tags, vec!["keep".to_string()]);
        assert_eq!(strip_retirement_tags(&mut tags, "p"), 0);
    }

    #[test]
    fn only_the_marker_namespace_is_a_retirement_tag() {
        assert!(is_retirement_tag("superseded-by:abc"));
        assert!(!is_retirement_tag("supersedes-abc"));
        assert!(!is_retirement_tag("ruling"));
    }
}
