# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.1.4] — 2026-11-09

### Fixed

- Updated doc comments that still named `open-mpm` as the caller of the
  read-only CTO DB tool surface (`tool_list_response`, `handle_tool_call`, and
  the crate-level overview) to say `trusty-agents` (renamed in #831). Doc
  comments only — the OMPM-RPC/1 wire contract and every query are unchanged.

### Security

- Bump `rusqlite` to 0.41 so the bundled `libsqlite3-sys` is 0.39 or later, which fixes RUSTSEC-2026-0459.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

