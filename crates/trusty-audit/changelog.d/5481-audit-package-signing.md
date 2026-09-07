Added

- The return package now carries `manifest.sha256.toml` — one row per delivered
  file with its SHA-256 and size — and, when the engagement config sets
  `[signing] private_key`, a detached ed25519 signature over that manifest in
  `manifest.sha256.sig`. Both are written last, so the manifest covers every
  other member; each digest is taken from the bytes as they reach the archive,
  in the same pass that scans them for credentials. `package::signing::verify`
  checks a received package against the retained public key and tells its
  failures apart rather than reporting one "verification failed": a member
  altered after signing, a removed signature, a signature made by another key, a
  manifest-listed member that is gone, and a member the manifest never listed.
  Before any of those it establishes the member set from the RAW central
  directory — the `zip` crate keys its entry table by name, so an archive
  repeating a name would present one member here and another to `zipfile` or
  Info-ZIP — refusing a repeated name and any disagreement with the parser.
  Member bytes are streamed through a fixed buffer, never into an allocation
  sized from the archive's own declared size. An engagement with no key still
  packages —
  the manifest ships without a `[signature]` table, `verify` answers `Unsigned`,
  and the CLI and the package README both say so. The signature is
  tamper-evidence in transit only: the key is on hardware the recipient
  controls, so it proves nothing about what happened there. The signing key
  joins the outbound credential scan, and `config::generate_for_new_engagement`
  drops a `[signing]` table so one engagement's key cannot reach another's
  package.
