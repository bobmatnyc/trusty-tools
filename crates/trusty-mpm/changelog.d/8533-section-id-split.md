Changed
- Breaking (library API): `SectionId` gains nine variants and `SectionId::CANONICAL` grows from 10 to 19 entries; `InstructionBlock` gains a `pinned` field and `ValidationError` a `PinnedGeneratedBlock` variant (#8533).
- Breaking (library API): `SectionId` is now `#[non_exhaustive]`, so a downstream `match` on it needs a wildcard arm (#8533).
- Prompt change: the delivered PM prompt now opens with `## Identity`, and the `# Framework Instructions` heading is gone (#8533).
- An `IDENTITY` override now replaces the role statement where it opens the prompt; before, the identity block sat after the agent roster (#8533).
