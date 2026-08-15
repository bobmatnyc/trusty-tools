//! The one credential-masking implementation (issue #2401).
//!
//! Why: `memory_core::filter` had its own private `redact_token` (issue
//! #1481) with the exact masking shape this ticket's `config` clap module
//! (Wave 2) also needs: "never echo the secret back verbatim, but show
//! enough of it that a human can identify *which* credential tripped a
//! check". Two independent implementations of the same shape is exactly the
//! duplication BASE-ENGINEER's consolidation rule targets — this module is
//! now the single implementation; `memory_core::filter::redact_token`
//! delegates to it.
//! What: [`redact_secret`] returns `{head}…({N} chars)` where `head` is the
//! first 4 characters and `N` is the input's byte length — the exact shape
//! `memory_core::filter`'s prior private implementation produced, verified
//! by the pre-existing `redact_token_masks_tail` test which now exercises
//! this function transitively. Inputs no longer than the head length are
//! fully masked (`…({N} chars)`, no head shown) rather than echoed in full —
//! hardened per issue #2475, which caught the original implementation
//! disclosing the entire value for any secret of 4 characters or fewer.
//! Test: `redact_tests` (sibling file, this module) and
//! `memory_core::filter_tests::redact_token_masks_tail` (format stability).
//!
//! Two shapes live here, for two different jobs. [`redact_secret`] masks a
//! value you are deliberately naming (`config keys list` printing which
//! credential it found). [`scrub_secrets`] removes values you did NOT intend
//! to print from text you do not control — a provider's error body, a child
//! process's stderr. Both are "never echo the secret", so they share a module;
//! [`resolved_secret_values`] sits with them because its only purpose is
//! feeding the second, and keeping the one function that materialises every
//! credential the process can reach next to its consumer keeps that hazard in
//! a single auditable file (#4321).

/// Head length used by [`redact_secret`]'s default (4 characters), matching
/// the format `memory_core::filter`'s prior private `redact_token` produced.
const DEFAULT_HEAD_LEN: usize = 4;

/// What [`scrub_secrets`] leaves behind where a secret was.
const REDACTED: &str = "[REDACTED]";

/// Shortest value [`scrub_secrets`] will treat as a secret, in characters.
///
/// Why: a needle shorter than this destroys more than it protects. The empty
/// string matches at every position, so replacing it blanks the message
/// entirely; a one- or two-character value (a placeholder, a `FOO=x` typo in a
/// shell profile, a store entry someone was testing with) shreds unrelated
/// prose into `[REDACTED]` confetti and destroys the diagnostic the caller was
/// trying to surface. Eight is below every real credential in
/// [`super::REGISTRY`] — the shortest provider tokens in circulation
/// (`sk-…`, `xoxb-…`, `ghp_…`, a 20-character AWS access-key id) are all well
/// clear of it — so the guard costs no coverage.
const MIN_SCRUBBABLE_SECRET_CHARS: usize = 8;

/// Produce a short, non-reversible preview of `secret`.
///
/// Why: callers (rejection messages, `config list --show-status`, log lines)
/// need to name *which* credential they're referring to without ever
/// echoing the value back. See module docs for the format-stability
/// rationale.
/// What: for secrets longer than [`DEFAULT_HEAD_LEN`] characters, returns the
/// first [`DEFAULT_HEAD_LEN`] characters followed by `…` and
/// `(<byte length> chars)`. Secrets AT OR UNDER [`DEFAULT_HEAD_LEN`]
/// characters are fully masked — `…(<byte length> chars)` with no head shown
/// — since echoing the head of a short secret would disclose the entire
/// value (issue #2475). There is no minimum-length guard beyond that;
/// callers gate on "is this actually secret-shaped" before calling, same as
/// `memory_core::filter::looks_like_secret` did for its 20-char floor.
/// Test: `redact_tests::redact_secret_masks_tail`,
/// `redact_tests::redact_secret_handles_short_input`,
/// `redact_tests::redact_secret_short_inputs_table`,
/// `redact_tests::contract_redact_secret_never_echoes_a_short_secret`.
///
/// # Code Contract
/// Preconditions:
/// - None. Every `&str` is accepted, including the empty string and non-ASCII
///   input. The caller decides whether the value is secret-shaped; this
///   function does not gate on length.
///
/// Postconditions:
/// - The result is NON-REVERSIBLE: it never contains more than the first
///   [`DEFAULT_HEAD_LEN`] characters of `secret`.
/// - When `secret` is at or under [`DEFAULT_HEAD_LEN`] CHARACTERS, no head is
///   shown at all (#2475) — showing the head of a short secret discloses the
///   whole value.
/// - The reported length is `secret.len()`, a BYTE count, while the head is
///   taken in CHARACTERS. The two units differ deliberately and the char-wise
///   `take` is what keeps a multi-byte secret from panicking.
/// - Total: never panics, for any input.
///
/// Invariants:
/// - Pure: no I/O, no logging, no environment access.
/// - The output format is depended on by `memory_core::filter`; it is a
///   compatibility surface, not a cosmetic choice.
pub fn redact_secret(secret: &str) -> String {
    let byte_len = secret.len();
    if secret.chars().count() <= DEFAULT_HEAD_LEN {
        return format!("…({byte_len} chars)");
    }
    let head: String = secret.chars().take(DEFAULT_HEAD_LEN).collect();
    format!("{head}…({byte_len} chars)")
}

/// Remove every occurrence of each known `secrets` value from `text`.
///
/// Why: text the process did not author — a provider's non-2xx HTTP body, a
/// child process's stderr — can echo back a credential the process holds
/// ("your key `sk-…` is invalid"), and that text then lands somewhere a
/// credential must never be: a chat bubble, an unencrypted state file, an API
/// response. This is the one implementation of that removal; it was
/// `inference::config::ops::scrub_key`, module-private, until #4321 needed the
/// same guard for `trusty-agents`' subprocess-failure narrative.
///
/// **What this cannot do.** It removes only values the caller already holds.
/// A secret the process does not know passes through untouched: a token a
/// child process read out of its own config file, a customer credential quoted
/// inside a provider's error body, a key the child derived or fetched over the
/// network, a credential stored under an environment variable no registry
/// entry names, or a value rotated since the caller resolved it. Scrubbed text
/// is therefore *lower-risk, not proven secret-free* — it is not a licence to
/// route untrusted text into a sink that could not otherwise be allowed to
/// hold a secret.
///
/// What: replaces each needle with `[REDACTED]`, longest needle first so that a
/// secret which is a prefix of another (an OAuth token and the API key it was
/// derived from, say) cannot leave the longer one's tail behind. Values under
/// [`MIN_SCRUBBABLE_SECRET_CHARS`] characters — including the empty string —
/// are skipped rather than applied; see that constant for why. Returns `text`
/// unchanged when no needle survives the guard or none occurs.
/// Test: `redact_tests::scrub_secrets_removes_every_occurrence`,
/// `scrub_secrets_removes_multiple_distinct_secrets`,
/// `scrub_secrets_ignores_empty_and_short_values`,
/// `scrub_secrets_prefers_the_longest_overlapping_secret`,
/// `scrub_secrets_is_noop_when_nothing_matches`,
/// `redact_tests::contract_scrub_secrets_removes_every_qualifying_needle`.
///
/// # Code Contract
/// Preconditions:
/// - None. `secrets` may be empty and may contain empty or short values; those
///   are skipped by the guard rather than rejected.
///
/// Postconditions:
/// - For every needle of at least [`MIN_SCRUBBABLE_SECRET_CHARS`] characters,
///   the result contains NO occurrence of that needle.
/// - A needle under that length — the empty string included — is left
///   unapplied, and text that happens to contain it is returned unchanged.
/// - Overlapping needles are applied longest-first, so a secret that is a
///   prefix of another cannot leave the longer one's tail behind.
/// - Returns `text` unchanged when no needle survives the guard or none occurs.
///
/// Invariants:
/// - Pure: no I/O, no environment access, `secrets` is not mutated.
/// - The result is LOWER-RISK, NOT PROVEN SECRET-FREE. It removes only values
///   the caller already holds; a secret the process does not know passes
///   through untouched. This is a bound on what the postconditions above claim,
///   and it is why scrubbed text is not a licence to route untrusted text into
///   a sink that could not otherwise hold a secret.
pub fn scrub_secrets<S: AsRef<str>>(text: &str, secrets: &[S]) -> String {
    let mut needles: Vec<&str> = secrets
        .iter()
        .map(AsRef::as_ref)
        .filter(|s| s.chars().count() >= MIN_SCRUBBABLE_SECRET_CHARS)
        .collect();
    needles.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));

    let mut out = text.to_string();
    for needle in needles {
        if out.contains(needle) {
            out = out.replace(needle, REDACTED);
        }
    }
    out
}

/// Every credential in [`super::REGISTRY`] that currently resolves, as raw
/// values, for feeding [`scrub_secrets`].
///
/// Why: a caller that wants to scrub "whatever this process is holding" must
/// not re-derive the provider→env-var table or read `std::env::var` ad hoc —
/// that is how a consumer ends up scrubbing `OPENROUTER_API_KEY` and missing
/// the `CLAUDE_CODE_OAUTH_TOKEN` sitting beside it. Walking the same registry
/// and the same 3-tier [`super::resolve_key`] precedence every other consumer
/// uses means a credential added to the registry is scrubbed without the
/// caller changing.
/// What: loads `.env.local` once, opens the secure store once, and maps every
/// registered provider through [`super::resolve_key_with`] — the same
/// env > `.env.local` > store precedence [`super::resolve_key`] applies, minus
/// the per-provider store construction that calling it in a loop would incur.
/// Returns raw secrets: the ONLY correct use is as [`scrub_secrets`]' needle
/// set; never log, serialise, or display the result. Resolution touches the
/// filesystem (and, where compiled in, the OS keychain), so call it once per
/// operation and reuse the result rather than once per line of text.
/// Test: `redact_tests::resolved_secret_values_are_scrubbable_by_scrub_secrets`
/// (registry-walk shape; the resolver tiers are covered by `resolver_tests`).
pub fn resolved_secret_values() -> Vec<String> {
    super::load_env_local_once();
    let store = super::default_store();
    super::registered_providers()
        .iter()
        .filter_map(|(provider, _)| super::resolve_key_with(provider, store.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Code Contract tests (#5724, ADR-0047) ────────────────────────────────

    /// Why: the non-reversibility postcondition is the whole point of
    /// [`redact_secret`], and #2475 caught the original implementation
    /// disclosing a short secret in full. A table over the boundary proves the
    /// claim rather than one example of it.
    /// What: for every length up to and past [`DEFAULT_HEAD_LEN`], the output
    /// never contains more of the secret than the contract permits, and a
    /// secret at or under the head length contributes no head at all.
    /// Test: itself.
    #[test]
    fn contract_redact_secret_never_echoes_a_short_secret() {
        for n in 0..=12usize {
            let secret: String = "abcdefghijkl".chars().take(n).collect();
            let out = redact_secret(&secret);

            // Postcondition: the byte length is reported.
            assert!(
                out.contains(&format!("({} chars)", secret.len())),
                "n={n}: byte length must be reported, got {out}"
            );

            if n <= DEFAULT_HEAD_LEN {
                // Postcondition: no head at all for a short secret (#2475).
                // Exact equality is the whole claim — the output is the mask
                // and the byte count, with no character of `secret` in it. A
                // `!out.contains(&secret)` check would be WRONG here, not
                // merely redundant: the literal "chars" contains "a", so a
                // one-character secret "a" trips it against a correct mask.
                assert_eq!(out, format!("…({} chars)", secret.len()), "n={n}");
            } else {
                let head: String = secret.chars().take(DEFAULT_HEAD_LEN).collect();
                assert!(out.starts_with(&head), "n={n}");
                // Postcondition: never more than DEFAULT_HEAD_LEN characters.
                let disclosed: String = secret.chars().take(DEFAULT_HEAD_LEN + 1).collect();
                assert!(
                    !out.contains(&disclosed),
                    "n={n}: disclosed more than the head, got {out}"
                );
            }
        }

        // Totality: multi-byte input must not panic, and the char/byte split in
        // the contract is what keeps it from doing so.
        let multi = "héllo-wörld-secret";
        let out = redact_secret(multi);
        assert!(out.contains(&format!("({} chars)", multi.len())));
    }

    /// Why: `scrub_secrets` is a security boundary, and its contract has two
    /// halves that pull in opposite directions — remove every qualifying
    /// needle, but leave short ones alone so a placeholder cannot shred an
    /// unrelated diagnostic. A test of either half alone would let the other
    /// regress.
    /// What: qualifying needles are removed everywhere including overlaps;
    /// sub-threshold needles are left unapplied; text with no match is
    /// returned unchanged.
    /// Test: itself.
    #[test]
    fn contract_scrub_secrets_removes_every_qualifying_needle() {
        let long = "sk-abcdefghijklmnop"; // pragma: allowlist secret
        let prefix = "sk-abcdefgh"; // a prefix of `long`, itself over the floor

        // Postcondition: no occurrence of a qualifying needle survives.
        let text = format!("first {long} then {long} again");
        let out = scrub_secrets(&text, &[long]);
        assert!(!out.contains(long), "every occurrence must go: {out}");
        assert_eq!(out.matches(REDACTED).count(), 2);

        // Postcondition: longest-first, so the longer secret cannot leave its
        // tail behind when a prefix of it is also a needle.
        let out = scrub_secrets(long, &[prefix, long]);
        assert!(!out.contains(prefix), "prefix leaked: {out}");
        assert!(
            !out.contains("ijklmnop"),
            "tail of the longer secret leaked: {out}"
        );

        // Postcondition: a needle under MIN_SCRUBBABLE_SECRET_CHARS is skipped,
        // empty string included — otherwise it would blank the message.
        let short = "abc";
        assert!(short.chars().count() < MIN_SCRUBBABLE_SECRET_CHARS);
        let diagnostic = "connection to abc-host refused";
        assert_eq!(scrub_secrets(diagnostic, &[short, ""]), diagnostic);

        // Postcondition: unchanged when nothing matches.
        assert_eq!(scrub_secrets(diagnostic, &[long]), diagnostic);
        assert_eq!(scrub_secrets(diagnostic, &[] as &[&str]), diagnostic);
    }

    /// Why: pins the exact format `memory_core::filter` depends on.
    /// Test: itself.
    #[test]
    fn redact_secret_masks_tail() {
        let r = redact_secret("AbCd1234EfGh5678IjKl9012"); // pragma: allowlist secret
        assert!(r.starts_with("AbCd"));
        assert!(r.contains('…'));
        assert!(r.contains("chars"));
        assert!(!r.contains("9012"), "tail must be masked: {r}");
    }

    /// Why: short inputs must not panic (no out-of-bounds slicing) AND must
    /// not disclose their entire value — the issue #2475 fix. Previously
    /// `redact_secret("ab")` returned `"ab…(2 chars)"`, echoing the full
    /// secret; it must now be fully masked.
    /// Test: itself.
    #[test]
    fn redact_secret_handles_short_input() {
        assert_eq!(redact_secret(""), "…(0 chars)");
        assert_eq!(redact_secret("ab"), "…(2 chars)");
    }

    /// Why: table-driven coverage of the masking threshold boundary — every
    /// length at or under [`DEFAULT_HEAD_LEN`] must be fully masked (no
    /// prefix disclosed), and the first length above it must reveal only the
    /// head, never the tail.
    /// Test: itself.
    #[test]
    fn redact_secret_short_inputs_table() {
        let cases: &[(&str, &str)] = &[
            ("", "…(0 chars)"),
            ("a", "…(1 chars)"),
            ("ab", "…(2 chars)"),
            ("abc", "…(3 chars)"),
            ("abcd", "…(4 chars)"),
            ("abcde", "abcd…(5 chars)"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                redact_secret(input),
                *expected,
                "mismatch for input {input:?}"
            );
        }
    }

    /// Why: the behaviour promoted from `inference::config::ops::scrub_key` —
    /// a provider error body that echoes the key back must lose EVERY
    /// occurrence, not the first one.
    /// Test: itself.
    #[test]
    fn scrub_secrets_removes_every_occurrence() {
        let key = "sk-or-verysecret1234"; // pragma: allowlist secret
        let msg = format!("inference API error 400: bad key {key}, retry without {key}");
        let scrubbed = scrub_secrets(&msg, &[key]);
        assert!(!scrubbed.contains(key), "leaked: {scrubbed}");
        assert_eq!(scrubbed.matches(REDACTED).count(), 2);
    }

    /// Why: the #4321 case — a process holds several provider credentials at
    /// once and a single block of captured text can quote more than one.
    /// Scrubbing must not stop at the first match.
    /// Test: itself.
    #[test]
    fn scrub_secrets_removes_multiple_distinct_secrets() {
        let openrouter = "sk-or-v1-aaaaaaaaaaaaaaaa"; // pragma: allowlist secret
        let anthropic = "sk-ant-api03-bbbbbbbbbbbb"; // pragma: allowlist secret
        let oauth = "sk-ant-oat01-cccccccccccc"; // pragma: allowlist secret
        let text = format!(
            "auth failed for {openrouter}\nfallback {anthropic} rejected\nand {oauth} expired"
        );
        let scrubbed = scrub_secrets(
            &text,
            &[openrouter.to_string(), anthropic.into(), oauth.into()],
        );
        for leaked in [openrouter, anthropic, oauth] {
            assert!(!scrubbed.contains(leaked), "leaked {leaked}: {scrubbed}");
        }
        assert_eq!(scrubbed.matches(REDACTED).count(), 3);
        assert!(scrubbed.contains("auth failed for"), "{scrubbed}");
    }

    /// Why: the classic footgun in this pattern. An empty needle matches at
    /// every position, so replacing it blanks the message; a one- or two-char
    /// needle shreds unrelated prose. Neither may reach the replacement.
    /// Test: itself.
    #[test]
    fn scrub_secrets_ignores_empty_and_short_values() {
        let msg = "no `.trusty-agents/agents/` found in /.";
        assert_eq!(scrub_secrets(msg, &[""]), msg);
        assert_eq!(scrub_secrets(msg, &["a"]), msg);
        assert_eq!(scrub_secrets(msg, &["nt"]), msg);
        assert_eq!(scrub_secrets(msg, &["found"]), msg);
        // The boundary itself: one char under is ignored, exactly at it applies.
        assert_eq!(scrub_secrets("xx1234567 yy", &["1234567"]), "xx1234567 yy");
        assert_eq!(
            scrub_secrets("xx12345678 yy", &["12345678"]),
            "xx[REDACTED] yy"
        );
        // A real secret alongside a junk short value still gets removed.
        let real = "sk-or-v1-realsecret0001"; // pragma: allowlist secret
        let text = format!("bad key {real}");
        assert_eq!(scrub_secrets(&text, &["", "x", real]), "bad key [REDACTED]");
    }

    /// Why: `CLAUDE_CODE_OAUTH_TOKEN` and `ANTHROPIC_API_KEY` share a `sk-ant-`
    /// lineage and one configured value can be a prefix of another. Replacing
    /// the shorter first would leave the longer one's tail in the text.
    /// Test: itself.
    #[test]
    fn scrub_secrets_prefers_the_longest_overlapping_secret() {
        let short = "sk-ant-prefix000"; // pragma: allowlist secret
        let long = format!("{short}-with-a-longer-tail");
        let text = format!("rejected {long}");
        let scrubbed = scrub_secrets(&text, &[short.to_string(), long.clone()]);
        assert_eq!(scrubbed, "rejected [REDACTED]");
        assert!(!scrubbed.contains("-with-a-longer-tail"), "{scrubbed}");
    }

    /// Why: the common case is text with no secret in it; it must come back
    /// byte-identical rather than subtly rewritten.
    /// Test: itself.
    #[test]
    fn scrub_secrets_is_noop_when_nothing_matches() {
        let msg = "Error: no `.trusty-agents/agents/` found in /.";
        let empty: &[&str] = &[];
        assert_eq!(scrub_secrets(msg, empty), msg);
        assert_eq!(scrub_secrets(msg, &["sk-or-v1-notpresent"]), msg);
    }

    /// Why: [`resolved_secret_values`] exists only to feed [`scrub_secrets`],
    /// so what it must guarantee is that whatever it hands back is actually
    /// usable as a needle set — never that a particular provider is
    /// configured, which depends on the machine running the test.
    /// Test: itself.
    ///
    /// [`resolved_secret_values`] calls `load_env_local_once`, which folds the
    /// machine's real `.env.local` into the PROCESS environment. Held no lock
    /// until now, so it raced `credentials::resolver::tests`: firing the loader
    /// between that test's `remove_var` and its `load_env_from_path` republished
    /// the real `OPENROUTER_API_KEY`, `dotenvy` then declined to override it,
    /// and the assertion printed a live key into test output. Join the same
    /// `dotenv_credential_env` group as every other test that reads or writes a
    /// credential env var — this test is a WRITER of them, via the loader.
    #[test]
    #[serial_test::serial(dotenv_credential_env)]
    fn resolved_secret_values_are_scrubbable_by_scrub_secrets() {
        let values = resolved_secret_values();
        assert!(
            values.len() <= super::super::registered_providers().len(),
            "cannot resolve more secrets than there are registered providers"
        );
        for v in &values {
            assert!(!v.is_empty(), "the resolver never yields an empty value");
        }
        // Whatever resolved, feeding it back must remove it from a message.
        let probe: Vec<String> = values.into_iter().collect();
        let text = probe
            .iter()
            .map(|v| format!("saw {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        let scrubbed = scrub_secrets(&text, &probe);
        for v in &probe {
            if v.chars().count() >= MIN_SCRUBBABLE_SECRET_CHARS {
                assert!(!scrubbed.contains(v.as_str()), "leaked a resolved value");
            }
        }
    }
}
