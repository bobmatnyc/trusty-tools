Breaking
- Library API: `SectionId` gains nine variants and `SectionId::CANONICAL` grows from 10 to 19 entries; `InstructionBlock` gains a `pinned` field and `ValidationError` a `PinnedGeneratedBlock` variant (#8533).
- Library API: `SectionId` is now `#[non_exhaustive]`, so a downstream `match` on it needs a wildcard arm (#8533).
- Library API: `ProjectLevelConfig` gains a public `style: Option<ProjectStyleConfig>` field for the `.trusty-mpm.toml` `[style]` table, so a downstream struct literal must set it or use `..Default::default()` (#8533).
