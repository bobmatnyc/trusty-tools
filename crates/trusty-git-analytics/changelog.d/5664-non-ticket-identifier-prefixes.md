Fixed

- `tga collect`: Linear enrichment no longer spends a GraphQL lookup on
  documentation, standard, digest and advisory tokens that merely share a ticket's
  shape (#5664). `LinearClient::extract_issue_ids` now drops `UTF-8`, `SHA-256`,
  `ADR-0029`, `RFC-2119`, `ISO-8601`, `ECMA-48`, `RUSTSEC-2026` and their families
  before any request is issued; a live 52-week collect on this repository sent 369
  such lookups, none of which could ever resolve. The decision is made offline by
  `collect::ticket::is_non_ticket_identifier`, which `extract_ticket_id`,
  `is_ticketed` and `branch_ticket_key` already used under its former name — one
  prefix list, so the pre-lookup gate and the subject-position rule cannot drift
  apart. That list widened from four documentation prefixes to the measured
  families, so a subject led by `CVE-2024-3094` or `SHA-256` no longer counts as a
  declared ticket key either. Ticket-shaped tokens that simply do not resolve
  (`WI-1`, `AC-1`, `CREDPANEL-01`) are deliberately still looked up: nothing
  separates them from another organization's real board keys, and DOC-70 §9.1
  reads an unresolved key as the signal it is.
