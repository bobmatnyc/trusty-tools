Added

- `credentials::CredentialRef` — the opaque, durable, **non-secret** handle that
  names a credential without carrying it (closes
  [#4565](https://github.com/bobmatnyc/trusty-tools/issues/4565), epic
  [#4040](https://github.com/bobmatnyc/trusty-tools/issues/4040), DOC-45
  `C-2.1`–`C-2.7`). `credential_ref` had zero hits under `crates/*/src/**`
  despite being an acceptance bullet of the closed-as-completed #2808, so
  anything that needed to *refer* to a credential had to *hold* one — which is
  why `McpService.env` is a map of literal API keys in a hand-editable TOML. A
  ref is safe in a git-tracked file, a config row, a log line, an audit record,
  and a model-visible tool result: its grammar is lowercase-kebab segments,
  ≤ 64 bytes, at most one `/`, which no realistic API key, JWT, OAuth token, or
  PEM body can satisfy. Pinned by
  `realistic_credentials_are_rejected_by_the_grammar` against specimens shaped
  like every credential format the registry names. Stable across rotation, and
  shape-agnostic: one type, one entry point, and no code path that exists only
  for OAuth or only for a plain API key.
- `credentials::Secret<T>` — the wrapper a resolved credential comes back in.
  Its `Debug`/`Display` render a constant that is not merely redacted but
  *independent of the value* (the impls carry no `T: Debug` bound, so they
  cannot read it), and it implements neither `Serialize`, `Deserialize`,
  `Clone`, `Deref`, nor `PartialEq`. Each omission is a closed leak path; the
  absent `Serialize` is the load-bearing one, because every config struct in the
  workspace derives it, so a `Secret` cannot compile inside one. Three
  compile-time assertions (the `assert_not_impl` coherence trick, inlined rather
  than adding a dependency) fail the build if any of the three traits is ever
  added.
- `credentials::resolve(&CredentialRef, &Principal, &Scope) -> Result<Secret<String>, CredentialError>`
  — the single resolution entry point, called where the credential is consumed
  rather than at config load (`C-3.3`, `C-8.4`). `resolve_client` is the same
  entry point with the value consumed in place, handing back an
  already-authenticated handle so the caller never sees the string (`C-3.4`,
  DOC-63 `S-5.1`). A ref naming a provider absent from the registry fails with
  `Missing` carrying a remediation that names the registry (`C-2.7`).
- `credentials::CredentialError` — `Missing` / `Denied` / `Expired` /
  `ZeroScope` / `ScopeUnavailable`. Every variant is recoverable, carries the
  `CredentialRef` and the `Principal`, renders an actionable remediation, and
  can hold no secret material by construction. The fifth variant is DOC-45
  `C-5.5`'s deliberate addition to #4040's stated four, kept distinct from
  `ZeroScope` by `C-5.6` because "widen the grant" is advice that cannot be
  followed for a provider that has no such scope.
- `credentials::Principal`, `ServiceId`, `Scope`, `Access` — the vocabulary
  `resolve`'s final signature is written in. `Principal` is a closed,
  `#[non_exhaustive]` enumeration carrying `Operator` and `Service` only:
  DOC-45 `C-1.3` is PROVISIONAL pending owner question Q-B and says in terms
  not to implement the `Assistant` variant until it is answered, so #4566 adds
  `Assistant` and `SubAgent` without a breaking change.
- `SecretString::into_secret` — the one-line migration from the older inference
  wrapper to the canonical `Secret<String>`.

Changed

- `inference::types::SecretString` is documented as superseded by
  `credentials::Secret`. Its behaviour is unchanged; note that its
  four-character head preview does **not** meet DOC-45 `C-8.2`, and collapsing
  the two types is left to a follow-up because it requires changing the existing
  test that pins that preview's shape.

Note: this change adds **no authorization**. `resolve` accepts a `Principal` and
checks no grant against it — grants, the ACL, default-deny and the code-owned
floor are [#4566](https://github.com/bobmatnyc/trusty-tools/issues/4566). The
signature is final so no consumer is migrated twice.
