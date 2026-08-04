Fixed

- `memory_core::filter::find_secret_token` now treats the backtick as a token delimiter alongside whitespace, so adjacent Markdown inline-code spans joined by a bare `/` are no longer misread as one high-entropy token and rejected as a credential (closes [#4312](https://github.com/bobmatnyc/trusty-tools/issues/4312))
  - fixes backtick-joined identifiers (`` `stale_skills`/`doctor_staleness.rs` ``), backtick-joined paths, and backtick-wrapped issue/PR lists (`` `#4601`/`#4602`/`#4603` ``) — the last a regression of the [#4216](https://github.com/bobmatnyc/trusty-tools/issues/4216) exemption that recurred whenever the list was quoted
  - structural, not another allowlist: a backtick appears in no credential alphabet or provider key format, so it can never split a genuine credential. Detection is strictly tightened — a credential written flush against a backtick was previously missed because the backtick defeated the plausible-credential-charset gate
