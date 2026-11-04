Added
- `trusty_okf::format` and `trusty_okf::profile`: frontmatter, the concept model, container rules, validation and inverse-edge reconciliation move here from trusty-kb with their history (SPEC-OKF-01 §2.7.2, #10746).
- Raw-preserving round trip (SPEC-OKF-01 §2.2.1): `parse_concept` keeps the frontmatter bytes and splits them into top-level entries, and `render_concept` rewrites only the entries whose value changed, so unknown keys, comments and flow style survive a write.
- Typed OKF v0.2 view: `Frontmatter` reads each known key as absent, valid or malformed, with a warning for a malformed value; `trust_tier` (OKF §5.3) and `staleness` (OKF §5.5; a date-only or offset-less `stale_after` reads as unknown).
- `ConceptId::parse` with the eight §2.11 lexical rules, and `concept_ref_for_path`, which reads a file that fails them as read-only `ConceptRef::Foreign` with an info finding.
- `write_concept`: a strict (`WriteMode::StrictErrors` by default) atomic write that refuses an OKF §11 breach such as a missing `type` before touching any file, writes nothing when the bytes are unchanged, and refuses foreign paths and symbolic links.
- `format::v02`: the §2.8 per-concept mapping (`updated` to `generated.at`, plain-string `sources` to mappings, bare `verified` to a list) and the OKF §13 read fallbacks for `timestamp` and a body `# Citations` list.
- `OkfError`, the crate error type (SPEC-OKF-01 §4.4), with the variants this slice uses.
- Test fixtures: the four Google OKF sample bundles at `ad30107c` (Apache-2.0, see `tests/fixtures/google-okf-ad30107c/PROVENANCE.md`) and the §2.2.1 raw round-trip fixtures.
