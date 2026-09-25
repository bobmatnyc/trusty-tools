Breaking
- Library API: `SectionId` gains nine variants and `SectionId::CANONICAL` grows from 10 to 19 entries; `InstructionBlock` gains a `pinned` field and `ValidationError` a `PinnedGeneratedBlock` variant (#8533).
- Library API: `SectionId` is now `#[non_exhaustive]`, so a downstream `match` on it needs a wildcard arm (#8533).
