Fixed

- `tctl sign <target>` no longer reports "signed with Developer ID" for a signature that reads back ad-hoc, without a Team ID, with a non-Developer-ID certificate, or with a Team ID other than the identity's. It exits 1 and names the cause; `codesign --verify --strict` alone accepted an ad-hoc signature (Refs #11587).
