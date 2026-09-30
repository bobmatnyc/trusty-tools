Changed
- Prebuilt and pinned downloads parse the `.sha256` sidecar and hash the artifact through trusty-common's `integrity::Sha256Digest`, the single sha256 implementation ADR-0064 requires, instead of a private copy (#8378). What is accepted and refused is unchanged; the wording of a malformed-checksum or unreadable-artifact error now comes from trusty-common.
