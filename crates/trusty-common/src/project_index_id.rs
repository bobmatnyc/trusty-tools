//! Project-derived trusty-search index identity — a PARTITIONING key
//! (epic #4207, replacing the approach closed as won't-do in #4063).
//!
//! Why: the index id in use today is either the bare directory basename
//! ([`crate::index_id::derive_index_id`]) — which collides for any two
//! unrelated checkouts that happen to share a directory name — or, for a
//! tm-managed session, the session/worktree UUID, which binds a service
//! identity to ephemeral writer isolation: N concurrent writers on one repo
//! produce N independently-stale indexes, none shareable, and the id is
//! unguessable so BASE_PM's instruction to pass the project name silently
//! 404s. Both defects need ONE deterministic id derived from the PROJECT.
//!
//! **The contract that sank the previous attempt — state it before reading
//! any code.** trusty-search's registry is one-`root_path`-per-id (see
//! `trusty-mpm`'s `session_manager::search_gc`), so an index id must
//! *partition*: one id maps to exactly ONE content tree. [`RepoIdentity`]'s
//! own module doc says it is a *grouping* key — deliberately shared by every
//! facet (live checkout, `.base` clone, each session worktree) of one repo.
//! #4063 promoted that grouping key into the partitioning slot, and each
//! review round found another pair of genuinely different roots sharing it:
//! linked worktrees, then sibling clones of one repo, then the
//! daemon-unreachable path resolving forward onto the new id. This module
//! partitions **by construction** instead of by patching cases: the canonical
//! content-tree root path is a hashed component of every id, so two distinct
//! trees can never collide no matter how their git metadata relates. The
//! grouping key is retained as a *component* (and as the public
//! [`ProjectIdentity::origin`] field), not as the whole id.
//!
//! What: [`ProjectIdentity`] is the three-part identity the owner specified on
//! #4207 — origin (which repository), root (which content tree of it), and
//! operator (which account operates it — the triple's "gh user", realised
//! offline as git's configured `user.email`, not a GitHub login).
//! [`ProjectIdentity::derive`] is the one
//! I/O entry point (git shell-outs + one `canonicalize`);
//! [`ProjectIdentity::index_id`] is a pure, dependency-free function over
//! those three fields, so every collision case is testable without a daemon,
//! a registry, or live state. [`derive_project_index_id`] is the one-call
//! convenience wrapper. Nothing here is wired into `ensure_project_indexed`,
//! `trusty-search serve`, or the daemon's resolution path — this slice is the
//! derivation only.
//!
//! **Constraint inherited by the migration slice (do NOT implement here).**
//! The ~7 currently-registered indexes must be migrated by rename/alias, never
//! by re-index (this workspace alone is ~105k chunks / 2.1 GB redb). Whatever
//! resolver performs that migration MUST fail **safe**: when the daemon or the
//! persisted registry cannot be consulted, fall back to the known-good LEGACY
//! id. It must never fail *forward* onto an id that does not exist yet —
//! #4063 did exactly that at `search_index.rs:106-108` and silently orphaned a
//! session's search for the session's entire life, with no self-heal.
//!
//! Discoverability note: because the id is always suffixed, it is not
//! guessable from the project name alone. That is deliberate — a guessable id
//! cannot partition. The #4207 self-description slice (MCP `instructions` at
//! initialize, `404` → available-index signpost) is what makes ids findable;
//! the two changes are complements, and neither substitutes for the other.
//!
//! Test: `cargo test -p trusty-common -- project_index_id` — the sibling
//! `project_index_id_tests.rs` covers sibling clones, linked worktrees,
//! differing operators, remoteless directories, symlinked roots, determinism,
//! the id charset, and the origin/operator drift cases enumerated in
//! [`ProjectIdentity::index_id`]'s `Known limitation` block.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::repo_identity::RepoIdentity;
use crate::slug::slugify_string;

/// Version tag mixed into every digest preimage.
///
/// Why: the derivation rule will eventually change (a new component, a
/// different framing). Mixing a version tag into the digest preimage means a
/// future `v2` can never re-point an ALREADY-REGISTERED id at different
/// content — every id moves wholesale into a disjoint space. That is the
/// property the migration slice actually needs.
/// What: the literal `"v1"`. Note what this does NOT give you: the emitted id
/// carries no version marker, so a v1 id and a v2 id are indistinguishable by
/// inspection. A migration that must classify ids has to track the scheme
/// out-of-band (or a future version must emit the tag in the id itself).
/// Test: `index_id_is_pinned_for_a_known_input` fails if this value changes.
const SCHEME_VERSION: &str = "v1";

/// Maximum length of the human-readable label prefix of an index id.
///
/// Why: an index id is a single URL path segment and appears in filenames and
/// log lines; an unbounded owner/repo pair makes both unwieldy. Truncation is
/// safe because uniqueness lives entirely in the digest suffix, never in the
/// label.
/// What: 48 characters.
const MAX_LABEL_LEN: usize = 48;

/// Placeholder label used when a project yields no slug-able name at all.
///
/// Why: `index_id` must never return a string that starts with `-` or is only
/// a digest; a stable placeholder keeps the id readable and well-formed.
/// What: the literal `"project"`.
const FALLBACK_LABEL: &str = "project";

/// The three-part identity of one indexable project.
///
/// Why: #4207 specifies identity as origin + root + gh user. Modelling those as
/// explicit fields — rather than folding them into an opaque derivation — makes
/// each component's job checkable in isolation and lets [`Self::index_id`] stay
/// a pure function, so the collision cases that killed #4063 are testable
/// without any live daemon or registry state.
/// What: `origin` is the repo-level GROUPING key ([`RepoIdentity`], `None`
/// outside a git repo); `root` is the canonical absolute content-tree root and
/// is the component that makes the derived id a PARTITIONING key; `operator` is
/// the account operating this checkout. All three are hashed; only `origin`
/// (or the root basename) contributes to the readable label.
///
/// Only `root` is immutable for the life of a content tree. `origin` and
/// `operator` are both mutable git state — see the `Known limitation` block on
/// [`Self::index_id`] for the enumerated drift cases and what they cost.
/// Test: `sibling_clones_of_same_repo_derive_distinct_ids`,
/// `linked_worktrees_of_same_repo_derive_distinct_ids`,
/// `different_operators_derive_distinct_ids`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// Repo-level grouping key from the git origin remote, `None` outside a
    /// git repo. Deliberately NOT sufficient on its own to identify an index.
    /// MUTABLE: `git remote add origin` and the first commit both change it.
    pub origin: Option<RepoIdentity>,
    /// Canonical absolute root of THIS content tree (a clone root, or a linked
    /// worktree root — each is its own tree). The partitioning component, and
    /// the only component that does not move under ordinary git operations.
    pub root: PathBuf,
    /// The #4207 identity triple's "gh user" component, realised offline as
    /// git's configured operator identity.
    ///
    /// VALUE SHAPE: this is a `git config user.email` value (e.g.
    /// `bob@example.com`), NOT a GitHub login. Do not compare it against
    /// `gh api user`. `None` when git resolves no identity. See
    /// [`resolve_operator_identity`] for precedence and stability caveats.
    pub operator: Option<String>,
}

impl ProjectIdentity {
    /// Derive the identity of the project containing `start`.
    ///
    /// Why: the three components must be gathered by ONE rule wherever an id is
    /// computed (trusty-mpm at session launch, trusty-search's `detect_project`,
    /// trusty-review's report enrichment) or the callers silently diverge — the
    /// same failure mode `index_id` was centralised here to prevent (#1373).
    /// What: canonicalises `start` — which resolves **symlinks**, so a symlinked
    /// path and its target derive one id — then walks to the enclosing git root
    /// via [`crate::index_id::resolve_project_root`], which lands on the *linked
    /// worktree* root for a worktree because its `.git` is a file there, so each
    /// worktree is correctly treated as its own content tree. Finally reads the
    /// origin via [`RepoIdentity::derive`] and the operator via
    /// [`resolve_operator_identity`]. Best-effort and offline: every component
    /// degrades to `None` rather than failing, and `canonicalize` falls back to
    /// the input path when it cannot resolve. No network, no daemon, no registry.
    ///
    /// Known limitation — canonicalisation resolves symlinks ONLY. macOS
    /// firmlinks and Linux bind mounts are NOT resolved (`/Users/x` and
    /// `/System/Volumes/Data/Users/x` each canonicalise to themselves), so one
    /// content tree reached through both derives two ids and therefore two
    /// indexes over identical content. This is inherent to any path-based
    /// partitioning key, not specific to this derivation.
    /// Test: `derivation_is_deterministic_across_calls`,
    /// `directory_without_git_origin_derives_stable_id`,
    /// `symlinked_root_derives_same_id_as_real_root`.
    pub fn derive(start: &Path) -> ProjectIdentity {
        // #4207: canonicalise BEFORE the git-root walk so a symlinked start
        // path and the real path resolve to the same content tree.
        let canonical_start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
        let root = crate::index_id::resolve_project_root(&canonical_start);
        ProjectIdentity {
            origin: RepoIdentity::derive(&root),
            operator: resolve_operator_identity(&root),
            root,
        }
    }

    /// Render this identity as a trusty-search index id.
    ///
    /// Why: this is the PARTITIONING key defined at the top of this module —
    /// one id, exactly one content tree — and it must hold *by construction*,
    /// for sibling clones and linked worktrees alike, because trusty-search's
    /// registry stores a single `root_path` per id and silently degrades a
    /// second registration to a *find* against the first tree's root.
    /// What: `"<label>-<digest>"`, where `label` is the slugified
    /// `owner-repo` (falling back to the root basename, then [`FALLBACK_LABEL`],
    /// truncated to [`MAX_LABEL_LEN`]) and `digest` is 16 lowercase hex digits
    /// over an injectively-framed, version-tagged encoding of ALL THREE fields.
    /// The label is cosmetic; every bit of uniqueness is in the digest, so
    /// two projects sharing a label still get different ids. Pure — no I/O, no
    /// global state, no daemon lookup — which is what makes the collision cases
    /// testable and keeps resolution independent of mutable live state.
    /// Guarantees (exactly these, and no more): non-empty; a single URL path
    /// segment matching `[a-z0-9][a-z0-9-]*`; and a pure function of the three
    /// fields — identical fields yield an identical id in every process, on
    /// every machine, across daemon restarts, forever. Stability of the *id for
    /// a given project* is therefore only as strong as the stability of the
    /// three inputs, and two of them move. Read the next block before assuming
    /// permanence.
    ///
    /// **Known limitation — which inputs are mutable, and what changes an id.**
    /// Only `root` is fixed for the life of a content tree. Both other
    /// components are live git/user state, so ordinary actions re-derive a
    /// different id for the SAME tree. Each row is pinned by a test:
    ///
    /// | Action | Component | Test |
    /// |---|---|---|
    /// | The repo's first commit (`None` → `ContentHash`) | `origin` | `id_changes_when_first_commit_lands` |
    /// | `git remote add origin …` (`ContentHash` → `GitHub`) | `origin` | `id_changes_when_origin_remote_is_added` |
    /// | A new root commit (`git checkout --orphan`) on a remoteless repo | `origin` | `id_changes_on_orphan_root_commit` |
    /// | Changing the configured `user.email` | `operator` | `different_operators_derive_distinct_ids` |
    ///
    /// Row 2 is the everyday `git init` → work → `gh repo create` sequence. It
    /// changes both the label and the digest, so under a
    /// one-`root_path`-per-id registry an index registered before the remote is
    /// added is ORPHANED the moment it is added, with no self-heal — the same
    /// silent-orphan shape this module's header warns the migration slice about.
    /// This is a strictly weaker hazard than the #4063 failure (one tree
    /// deriving two ids over time wastes an index; two trees deriving one id
    /// merges unrelated codebases), and it cannot bite while the module has no
    /// call sites — but the wiring slice MUST NOT read this function as
    /// permanent. Recommended shape for that slice: reconcile on `root`, which
    /// is the only component that does not move, and treat an origin/operator
    /// change as an alias-and-carry-forward rather than a new index.
    ///
    /// Also machine-local: `root` is an absolute path, so the same repo cloned
    /// on two machines derives two ids. That is correct for a partitioning key
    /// — each machine holds its own physical index.
    /// Test: `index_id_is_a_single_url_safe_segment`,
    /// `label_ambiguity_does_not_collide`, `empty_label_falls_back_to_placeholder`,
    /// `index_id_is_pinned_for_a_known_input`,
    /// `long_label_is_truncated_without_losing_uniqueness`.
    pub fn index_id(&self) -> String {
        format!("{}-{:016x}", self.label(), self.digest())
    }

    /// Build the cosmetic, human-readable prefix of the id.
    ///
    /// Why: an id of pure hex is unreadable in logs and `tm` output; a label
    /// keeps `bobmatnyc-trusty-tools-…` recognisable at a glance. It carries no
    /// uniqueness guarantee — deliberately, so a label collision is harmless.
    /// What: `owner-repo` for a GitHub origin, else the root basename; slugified
    /// via [`slugify_string`], truncated to [`MAX_LABEL_LEN`] on a character
    /// boundary with any trailing `-` trimmed, and replaced by
    /// [`FALLBACK_LABEL`] when empty.
    /// Test: `label_ambiguity_does_not_collide`,
    /// `empty_label_falls_back_to_placeholder`.
    fn label(&self) -> String {
        let raw = match &self.origin {
            Some(RepoIdentity::GitHub(gp)) => format!("{}-{}", gp.owner, gp.repo),
            // A content-hash origin is opaque hex; the directory name reads
            // better and the digest still carries the uniqueness.
            _ => self
                .root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        };
        let mut label: String = slugify_string(&raw).chars().take(MAX_LABEL_LEN).collect();
        while label.ends_with('-') {
            label.pop();
        }
        if label.is_empty() {
            FALLBACK_LABEL.to_string()
        } else {
            label
        }
    }

    /// Hash all three identity components into a stable 64-bit digest.
    ///
    /// Why: the digest is the whole partitioning guarantee. Its preimage must be
    /// *injective* — #4063 shipped a bare `format!("{owner}-{repo}")` join where
    /// `foo-bar`/`baz` and `foo`/`bar-baz` produced one string — so every field
    /// here is length-prefixed and every `Option` carries a presence tag, making
    /// two different field tuples impossible to encode identically.
    /// What: length-framed `<len>:<bytes>` records for the scheme version, the
    /// origin's canonical form (`+`/`-` presence tag), the root path's raw
    /// bytes (not lossy UTF-8, so non-UTF-8 paths stay distinct), and the
    /// operator, run through [`fnv1a_64`].
    /// Test: `label_ambiguity_does_not_collide`,
    /// `unrelated_projects_sharing_a_basename_derive_distinct_ids`.
    fn digest(&self) -> u64 {
        let mut buf: Vec<u8> = Vec::with_capacity(256);
        push_field(&mut buf, SCHEME_VERSION.as_bytes());
        push_opt(&mut buf, self.origin.as_ref().map(|o| o.canonical()));
        push_field(&mut buf, &path_bytes(&self.root));
        push_opt(&mut buf, self.operator.clone());
        fnv1a_64(&buf)
    }
}

/// Derive the trusty-search index id for the project containing `start`.
///
/// Why: nearly every call site wants the id, not the identity; a one-call
/// wrapper keeps them from re-implementing the two-step derive-then-render and
/// drifting apart. Callers that also need the repo-level grouping key (to relate
/// several indexes of one repo) should use [`ProjectIdentity::derive`] and read
/// [`ProjectIdentity::origin`] instead of deriving it a second time.
/// What: [`ProjectIdentity::derive`] followed by [`ProjectIdentity::index_id`].
/// Performs the same best-effort git I/O as `derive`; never panics.
/// Test: `derivation_is_deterministic_across_calls`,
/// `sibling_clones_of_same_repo_derive_distinct_ids`.
pub fn derive_project_index_id(start: &Path) -> String {
    ProjectIdentity::derive(start).index_id()
}

/// Resolve the operator identity for the checkout at `root`, offline.
///
/// Why: #4207 specifies the account as an identity component, but the id must be
/// derivable with no network — `gh api user` would make every derivation a round
/// trip and a flake. Git's own configured identity is the offline signal that
/// actually varies per account, and a repo-local `user.email` override is
/// precisely how a second account's checkout is configured in practice.
///
/// **Deliberately reads NO environment variable.** An earlier revision honoured
/// a `TRUSTY_GH_USER` override; it was removed rather than merely tested,
/// because it made derivation non-hermetic in the one way that matters here —
/// two live callers on the SAME tree at the SAME instant deriving different ids
/// purely from differing process environments. The trusty-mpm daemon (launchd
/// plist env) and a `tm` CLI (shell env) are exactly that pair, so the override
/// would have re-created the "callers silently diverge" failure (#1373) that is
/// this module's whole reason to exist. Covering both branches with tests would
/// have pinned the divergence, not removed it. Eliminating the branch removes
/// the failure class.
/// What: `git -C <root> config --get user.email`, which already applies git's
/// repo-local-over-global precedence. `None` when git resolves no identity or is
/// unavailable. Returns a git email address, NOT a GitHub login.
///
/// Residual non-hermeticity, disclosed rather than claimed away: git's own
/// config resolution still depends on `HOME` / `GIT_CONFIG_GLOBAL`, so callers
/// running under materially different environments can still disagree when the
/// repo sets no local `user.email`; and `--get` on a multi-valued key returns
/// the LAST entry. The wiring slice must therefore guarantee uniform
/// environment inheritance across daemon and CLI — a partial rollout partitions
/// one tree into two indexes.
/// Test: `resolve_operator_identity_prefers_repo_local_git_identity`,
/// `resolve_operator_identity_ignores_ambient_env_override`.
pub fn resolve_operator_identity(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", "user.email"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

/// Append one length-framed record to a digest preimage.
///
/// Why: concatenating fields with a separator is not injective — the exact
/// defect flagged on #4063's `owner-repo` join. An explicit byte length makes
/// the boundary unambiguous regardless of the field's own contents.
/// What: writes `<decimal len>:<bytes>` to `buf`.
/// Test: covered by `label_ambiguity_does_not_collide`.
fn push_field(buf: &mut Vec<u8>, value: &[u8]) {
    buf.extend_from_slice(value.len().to_string().as_bytes());
    buf.push(b':');
    buf.extend_from_slice(value);
}

/// Append an optional field with an explicit presence tag.
///
/// Why: `None` and `Some("")` must not encode identically, or a project with no
/// origin would share a digest with one whose origin resolved to an empty
/// string.
/// What: `Some(v)` writes `+` then [`push_field`]; `None` writes a bare `-`.
/// Test: covered by `directory_without_git_origin_derives_stable_id`.
fn push_opt(buf: &mut Vec<u8>, value: Option<String>) {
    match value {
        Some(v) => {
            buf.push(b'+');
            push_field(buf, v.as_bytes());
        }
        None => buf.push(b'-'),
    }
}

/// Return a path's raw bytes for hashing.
///
/// Why: `to_string_lossy` maps every invalid byte sequence to the same
/// replacement character, so two genuinely different non-UTF-8 paths could hash
/// identically — a partitioning-key violation, however rare. Hashing the raw
/// OS bytes removes the case entirely on unix.
/// What: the underlying `OsStr` bytes on unix; the lossy UTF-8 encoding
/// elsewhere (Windows `OsStr` is UTF-16 and has no byte view).
/// Test: covered by `sibling_clones_of_same_repo_derive_distinct_ids`.
fn path_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().as_bytes().to_vec()
    }
}

/// FNV-1a 64-bit digest with a final avalanche mix.
///
/// Why: the id must be byte-identical across processes, machines, and toolchain
/// versions, which rules out `std`'s `DefaultHasher` (explicitly not
/// stability-guaranteed). A cryptographic digest would work but `sha2` is an
/// *optional* dependency of this crate, and an identity that changes with a
/// feature flag is worse than no identity at all — so this stays dependency-free
/// and unconditional. Not a security boundary: nothing here defends against a
/// chosen-preimage attacker, only against accidental collision.
/// (`memory_core::analytics::fnv1a_hash` is the same algorithm but lives behind
/// the `memory-core` feature and takes `&str`, so it is unusable here.)
/// What: standard FNV-1a over `bytes`, then a splitmix64 finalizer so that
/// paths differing in a single character still differ in most output bits.
/// Test: `digest_is_stable_for_identical_inputs`.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    // splitmix64 finalizer — FNV-1a alone avalanches poorly on inputs that
    // differ only in their final bytes, which is exactly the sibling-path case.
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^ (hash >> 31)
}

#[cfg(test)]
#[path = "project_index_id_tests.rs"]
mod tests;
