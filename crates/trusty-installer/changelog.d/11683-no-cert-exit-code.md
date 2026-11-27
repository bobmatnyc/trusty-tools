Changed

- `tctl sign <target>` exits 3 when no Developer ID Application certificate is available, and keeps exit 1 for every other signing failure, so a caller can tell the two apart. `scripts/install-trusty-memory-signed.sh` and `scripts/install-trusty-assistants-signed.sh` now soft-pass only exit 3, exiting 0 with `NOT SIGNED` as the last output line; a codesign error, a locked keychain or a wrong-Team-ID signature prints an ERROR line and exits 1 (Refs #11683).
