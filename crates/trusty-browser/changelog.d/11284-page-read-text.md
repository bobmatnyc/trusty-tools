Fixed
- `page_read` with `include_text` keeps up to half of `max_chars` for the page text, so a link-heavy page no longer returns empty text, and granted cross-origin child frames now contribute their text under a `[frame <id>]` marker (#11284).
