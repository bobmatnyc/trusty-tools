Fixed
- A hashless `verified` event that lives only in frontmatter now counts in the verification `history` of `verification_state`, `revision_state` and retrieval hits; it still never counts as Verified, and an event also in the record counts once (#10746, owner ruling D64).
