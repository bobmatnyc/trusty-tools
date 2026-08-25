//! The word and phrase tables the grounding guardrail matches against.
//!
//! Why: split out of `synthesize_grounding.rs` when #6191 gave that file a
//! collection-side evidence source and pushed it to 514 SLOC, over the 500
//! production cap. This is the natural seam — everything here is DATA the guard
//! reads, with no logic and no dependency on the guard's own types, and the two
//! consumers (`synthesize_grounding` and `synthesize_grounding_text`) already
//! imported it as a table rather than calling into it.
//!
//! What: the loopback and remote markers, the reachability rewrite table and the
//! vocabulary that gates it, the clean-signal words, the load-bearing phrases,
//! and the subject-token filters. Every entry keeps the issue history that put
//! it there.
//!
//! Test: exercised through both consumers — `synthesize_grounding_tests.rs`.
//!
//! [`Grounding::local_only`]: super::synthesize_grounding::Grounding

/// Text markers that scope a finding to a loopback-only surface.
///
/// Deliberately short and literal. A finding whose own remediation or evidence
/// says "localhost" is stating its reachability; inferring one from softer
/// words would make the check fire on findings it was never about.
///
/// #6082 lap 6: the socket spellings state the same fact without the word
/// localhost. The graded report's RED finding 2 shipped "a remote code execution
/// risk" because its remediation proposed "token or local-socket verification"
/// and none of the four original markers matched — the finding never entered
/// [`Grounding::local_only`], so the sentence had no owner and the check never
/// ran on it. A remediation that proposes verifying the peer over a local socket
/// is stating that the peer is on this host, which is a reachability statement
/// as literal as "localhost".
pub(super) const LOOPBACK_MARKERS: &[&str] = &[
    "localhost",
    "loopback",
    "127.0.0.1",
    "::1",
    "local-socket",
    "local socket",
    "unix socket",
    "unix-socket",
    "unix domain socket",
    "unix-domain socket",
];

/// Text markers that scope a finding to a network-reachable surface.
///
/// A finding carrying one of these vetoes the contradiction check for every
/// subject token it shares, so a genuinely remote endpoint in the same
/// repository can still be described as remote.
pub(super) const REMOTE_MARKERS: &[&str] = &[
    "0.0.0.0",
    "internet-facing",
    "publicly reachable",
    "publicly accessible",
    "all interfaces",
];

/// Phrases that assert reachability beyond the host, with the host-local form
/// each is rewritten to.
///
/// Longest first: `remote-code-execution` must be matched before a bare
/// `remote`, and every plural before its singular, or the rewrite would leave a
/// broken fragment behind (`remote attackers` → `any local processs`).
///
/// This table is a CONVENIENCE, not the gate. [`REACHABILITY_WORDS`] decides
/// what gets checked and what may ship; a claim this table cannot rewrite is
/// rejected rather than passed through.
pub(super) const REACHABILITY_REWRITES: &[(&str, &str)] = &[
    // #6082 lap 5: the hedged form. RED finding 3's business impact read
    // "enabling remote/local code execution" — no pattern below matches it,
    // because the `/local` sits between the two words every other pattern
    // expects to be adjacent, so the sentence never even triggered the check.
    (
        "remote/local code execution",
        "local-process-reachable code execution",
    ),
    (
        "remote/local code-execution",
        "local-process-reachable code-execution",
    ),
    ("remote/local", "local-process-reachable"),
    (
        "unauthenticated remote-code-execution",
        "unauthenticated local-process-reachable code-execution",
    ),
    (
        "unauthenticated remote code execution",
        "unauthenticated local-process-reachable code execution",
    ),
    (
        "remote-code-execution",
        "local-process-reachable code-execution",
    ),
    (
        "remote code execution",
        "local-process-reachable code execution",
    ),
    (
        "remote code-execution",
        "local-process-reachable code-execution",
    ),
    ("remotely exploitable", "exploitable by any local process"),
    ("remote attackers", "any local process"),
    ("remote attacker", "any local process"),
    ("remote control over", "control over"),
    ("remote unauthenticated", "unauthenticated local-process"),
    // #6082 lap 7: the network family. RED finding 2's business impact shipped
    // "An unauthenticated actor on the network or host can launch arbitrary
    // claude commands" — a reachability claim carrying no `remote` token, so
    // the pattern table above never matched it and the check never ran.
    ("on the network or on the host", "on the host"),
    ("on the network or host", "on the host"),
    ("over the network", "on the host"),
    ("across the network", "on the host"),
    ("from the network", "from any process on the host"),
    ("network-reachable", "reachable by any process on the host"),
    ("network reachable", "reachable by any process on the host"),
    ("network attackers", "any local process"),
    ("network attacker", "any local process"),
    ("unauthenticated network", "unauthenticated local-process"),
    ("network-facing", "host-local"),
    ("on the network", "on the host"),
];

/// Every word that asserts reachability beyond this host.
///
/// This list is the guardrail, and [`REACHABILITY_REWRITES`] only softens it.
/// A sentence about a local-only finding is CHECKED when it carries one of these
/// words, and may SHIP only when none survives the rewrite pass — so a spelling
/// this module has never seen is rejected and disclosed rather than passed
/// through unexamined.
///
/// #6082 lap 7 replaced a phrase blocklist with this vocabulary. Three laps in a
/// row shipped the same defect in a new spelling (`RCE`, then `remote/local`,
/// then `on the network`), each time because the trigger was a list of known
/// phrases: an unrecognised phrase was not corrected AND not checked. Keying
/// both halves off one word list inverts that — an unrecognised phrase is now
/// the case that fails closed.
///
/// The cost is real and deliberate: a sentence about a local-only finding that
/// mentions a network for an unrelated reason ("network I/O", "network calls to
/// the embedder") is rejected too, and its field falls back to the honesty
/// marker with a note in Synthesis Status. A due-diligence report claiming
/// network reach for a loopback-bound daemon is the worse of the two errors.
pub(super) const REACHABILITY_WORDS: &[&str] = &["remote", "network", "internet"];

/// The two words every "this dimension has no clean signal" sentence is built
/// around (#6082 lap 7).
///
/// Two words rather than one phrase, because the model puts the dimension
/// between them — the graded report wrote "No clean security signal is credited
/// here", and a literal `clean signal` match misses that. Paired with
/// [`CLEAN_SIGNAL_NEGATIONS`], which is what turns the noun phrase into a claim
/// of absence.
pub(super) const CLEAN_SIGNAL_WORDS: (&str, &str) = ("clean", "signal");

/// The negations that turn [`CLEAN_SIGNAL_WORDS`] into a claim of absence.
pub(super) const CLEAN_SIGNAL_NEGATIONS: &[&str] = &[
    "no ", "not ", "none", "nothing", "never", "n't", "zero ", "absent", "without",
];

/// Phrases that claim a crate is what the rest of the estate is built on.
pub(super) const LOAD_BEARING_PHRASES: &[&str] = &[
    "load-bearing",
    "load bearing",
    "most depended",
    "most-depended",
    "depended on by the rest",
];

/// Path and title words too generic to identify a finding's subject.
pub(super) const GENERIC_TOKENS: &[&str] = &[
    "crates", "src", "main", "lib", "mod", "test", "tests", "with", "this", "that", "from", "into",
    "have", "http", "https", "file", "line", "code", "data", "self", "type", "used", "when",
    "over", "path", "call", "rust",
];

/// Shortest path/title word admitted as a subject token.
pub(super) const MIN_TOKEN_LEN: usize = 4;
