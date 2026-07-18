## Project review addendum: strict-security

This project has opted into the **strict-security** review template. It adds
extra scrutiny on top of the standard rubric above — it does not replace or
loosen any of it. The grade scale, verdict table, and severity anchors defined
above remain authoritative; treat everything below as additional things to
actively look for and weight more heavily when deciding severity.

Pay special attention to:

- **Injection and deserialization.** SQL/command/template injection, unsafe
  deserialization of untrusted input, and any use of a raw/format-string query
  builder instead of parameterized queries.
- **AuthN/AuthZ boundaries.** Missing or weakened authentication checks,
  authorization checks performed after a side effect instead of before it, and
  any broadened default access (a permission, CORS origin, or route that
  becomes more permissive than it was before this diff).
- **Secrets and credentials.** Hard-coded secrets, API keys, or tokens; secrets
  logged, echoed, or included in error messages; credentials committed to a
  fixture or test file.
- **Cryptography.** Custom or hand-rolled crypto, weak/deprecated algorithms
  or modes, predictable IVs/nonces, and insufficient key lengths.
- **Input validation and trust boundaries.** Untrusted input (request bodies,
  query params, file uploads, webhook payloads, environment variables sourced
  from a less-trusted context) consumed without validation before it crosses
  into a privileged operation.
- **Dependency and supply-chain risk.** A new dependency with a known CVE, an
  unpinned version range on a security-sensitive dependency, or a dependency
  pulled from an unexpected/non-canonical source.

Severity guidance for this addendum: a **confirmed** injection, auth-bypass,
hard-coded-secret, or broken-crypto finding that is provable from the diff
itself follows the stock rubric's BLOCK/REQUEST_CHANGES criteria above (it is
a security regression, not a style nit) — do not soften it into an advisory
note. A **plausible but unproven** concern (e.g. "this input validation looks
incomplete but I cannot confirm exploitability from the diff alone") stays
advisory (APPROVE*) per the stock citability rule — speculation about
exploitability that you cannot back with a concrete trace through the diff
must never drive BLOCK on its own.
