//! Tests for [`super`]: the fail-closed arms of the roster loader, the
//! repository roster itself, and the content assertions that used to sit
//! beside the compiled-in consts (#9011).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use trusty_common::content::{ContentLock, DEV_CLASS_SOURCES, LOCK_FILE_NAME};
use trusty_common::integrity::Sha256Digest;

use super::*;

/// The trusty-tools checkout this test runs for (#9298: runtime, not the
/// compile-time build path).
pub(crate) fn repo_root() -> PathBuf {
    trusty_common::test_harness::test_repo_root().expect("resolve the checkout")
}

/// The repository's own content, through the dev override.
pub(crate) fn repo_content() -> ResolvedContent {
    checkout_content(&repo_root()).expect("repo content")
}

/// The repository roster, loaded once per test binary.
fn roster() -> &'static AgentRoster {
    static ROSTER: OnceLock<AgentRoster> = OnceLock::new();
    ROSTER.get_or_init(|| AgentRoster::load(&repo_content()).expect("repo roster"))
}

/// One roster file's body; panics with the loader's error when absent.
fn asset(name: &str) -> &'static str {
    roster().require(name).unwrap_or_else(|e| panic!("{e}"))
}

/// Builds a trusted checkout under `root`: every class directory of the dev
/// table, a `.git` directory and a `[workspace]` manifest, and no files.
pub(crate) fn fake_checkout(root: &Path) {
    for (_, rel) in DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("Cargo.toml");
}

/// #9011: with no checkout and no lock, resolution fails and names the fix.
/// It never yields an empty source a caller could read as "no agents".
/// #9396: the fix named first is `tm content update`; the offline
/// `tm content install --from` stays as the alternative, for every
/// not-installed arm.
#[test]
fn not_installed_names_tm_content_install() {
    let cache = tempfile::tempdir().expect("tempdir");
    let err = resolve_content_in(cache.path(), DevOverride::Off).expect_err("nothing installed");
    assert!(
        matches!(err, AgentContentError::NotInstalled { .. }),
        "got {err:?}"
    );
    let fetch_failed = AgentContentError::FetchFailed {
        reason: "could not reach api.github.com".to_string(),
    };
    assert!(fetch_failed.is_not_installed());
    for err in [err, AgentContentError::NoCacheDir, fetch_failed] {
        let msg = err.to_string();
        assert!(msg.contains("tm content install"), "{msg}");
        assert!(msg.contains("run `tm content update`"), "{msg}");
        assert!(
            msg.contains("tm content install --from <bundle.tar.gz>"),
            "{msg}"
        );
    }
}

/// #9012: a present file this binary cannot use names both remedies — a
/// content refresh and a tm upgrade — and the path.
#[test]
fn an_invalid_file_names_both_remedies() {
    let err = AgentContentError::Invalid {
        origin: "content-v0.2.0".to_string(),
        path: "instructions/pm-instruction-package.json".to_string(),
        reason: "unknown variant `newer-section`".to_string(),
    };
    let shown = err.to_string();
    for needle in [
        "instructions/pm-instruction-package.json",
        "unknown variant `newer-section`",
        "tm content update",
        "upgrade tm",
    ] {
        assert!(shown.contains(needle), "{needle} missing: {shown}");
    }
    assert!(!err.is_not_installed());
}

/// #9011: an installed bundle whose bytes do not match the lock is an error,
/// not a fallback to some other source.
#[test]
fn an_unverifiable_bundle_is_a_content_error() {
    let cache = tempfile::tempdir().expect("tempdir");
    let lock = ContentLock::new("content-v0.1.0", Sha256Digest::of_bytes(b"pinned")).expect("lock");
    std::fs::write(cache.path().join(lock.bundle_file_name()), b"tampered").expect("bundle");
    lock.store(&cache.path().join(LOCK_FILE_NAME))
        .expect("store");
    let err = resolve_content_in(cache.path(), DevOverride::Off).expect_err("checksum mismatch");
    assert!(
        matches!(
            err,
            AgentContentError::Content {
                source: ContentError::ChecksumMismatch { .. }
            }
        ),
        "got {err:?}"
    );
}

/// #9011: only the not-installed case is claimed by the once-per-process
/// reporter; any other content error stays with its caller, which reports it.
#[test]
fn only_a_not_installed_error_is_claimed() {
    let cache = tempfile::tempdir().expect("tempdir");
    let missing = resolve_content_in(cache.path(), DevOverride::Off).expect_err("nothing");
    assert!(missing.is_not_installed());
    assert!(AgentContentError::NoCacheDir.is_not_installed());
    // A repeat report is still claimed: the caller adds no line either time.
    assert!(report_not_installed(&missing, "test"));
    assert!(report_not_installed(&missing, "test"));
    let empty = AgentContentError::EmptyRoster {
        origin: "test".to_string(),
    };
    assert!(!empty.is_not_installed());
    assert!(!report_not_installed(&empty, "test"));
}

/// #9011: a source with no agents is `EmptyRoster`, never `Ok` with zero files.
#[test]
fn an_empty_roster_is_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    fake_checkout(root.path());
    let content = checkout_content(root.path()).expect("fake checkout resolves");
    let err = AgentRoster::load(&content).expect_err("no agents");
    assert!(
        matches!(err, AgentContentError::EmptyRoster { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("tm content update"), "{err}");
}

/// #9011: a roster without `BASE-AGENT.md` cannot compose anything, so it is
/// refused at load rather than at the first compose.
#[test]
fn a_roster_without_the_foundation_file_is_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    fake_checkout(root.path());
    std::fs::write(
        root.path().join("content/agents/engineer.md"),
        "---\nname: e\n---\n",
    )
    .expect("agent");
    let content = checkout_content(root.path()).expect("fake checkout resolves");
    let err = AgentRoster::load(&content).expect_err("no foundation file");
    match err {
        AgentContentError::Missing { path, .. } => assert_eq!(path, "agents/BASE-AGENT.md"),
        other => panic!("expected Missing, got {other:?}"),
    }
}

/// `require` names the file it could not find.
#[test]
fn require_names_the_missing_file() {
    let err = roster().require("no-such-agent.md").expect_err("absent");
    assert!(err.to_string().contains("agents/no-such-agent.md"), "{err}");
}

/// The repository roster carries all 43 agent files, `BASE-*` first, each
/// byte-identical to the file it names, and exactly the `content/agents/*.md`
/// listing — a file nothing loads, or a loaded name with no file, fails here.
#[test]
fn the_repository_roster_carries_every_agent() {
    let dir = repo_root().join("content/agents");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("content/agents")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    on_disk.sort();
    let mut loaded: Vec<String> = roster().iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(loaded.len(), 43, "the roster must carry all 43 agent files");
    let bases = loaded.iter().take_while(|n| n.starts_with("BASE-")).count();
    assert_eq!(bases, 5, "the five BASE-* templates must come first");
    loaded.sort();
    assert_eq!(on_disk, loaded);
    for (name, body) in roster().iter() {
        assert!(!body.trim().is_empty(), "`{name}` is empty");
        let file = std::fs::read_to_string(dir.join(name)).expect("read");
        assert_eq!(file, body, "`{name}` does not hold the file it names");
    }
}

/// `materialize` writes every roster file, byte-identical, and names them.
#[test]
fn materialize_writes_every_roster_file() {
    let out = tempfile::tempdir().expect("tempdir");
    let dir = out.path().join("agents");
    let written = roster().materialize(&dir).expect("materialize");
    assert_eq!(written.len(), roster().len());
    for (name, body) in roster().iter() {
        assert_eq!(std::fs::read_to_string(dir.join(name)).expect("read"), body);
    }
}

/// A blocked agent must not reach for a more-privileged `gh` credential.
/// See #5680 — a `version-control` agent hit a `BEHIND` branch-protection
/// block, borrowed the repo owner's token, and force-merged with `--admin`.
/// The prohibition ships in two places on purpose: `BASE-AGENT.md` binds
/// every composed agent, and `version-control.md` puts it beside the
/// `gh auth status` check the acting agent actually read.
#[test]
fn credential_switching_is_forbidden_in_the_shipped_assets() {
    for (name, body) in [
        ("BASE-AGENT.md", asset("BASE-AGENT.md")),
        ("version-control.md", asset("version-control.md")),
    ] {
        let flat = body.replace('\n', " ");
        assert!(
            flat.contains("Never switch") && flat.contains("credential"),
            "`{name}` must forbid switching to another `gh` \
             account/token/credential to obtain a missing permission (#5680)"
        );
    }

    // #2842 went the other way — the agent refused an authorized
    // admin-merge — so the ban above must not swallow that path.
    assert!(
        asset("version-control.md")
            .contains("When the PM relays operator authorization to merge directly"),
        "the credential-switching ban must leave the PM-relayed \
         admin-merge authorization intact (#2842)"
    );
}

/// The credential rule names the sanctioned route, so a refusal says where the
/// owner binds a per-repo identity instead of leaving the agent to improvise.
/// See #8557 part (b).
#[test]
fn credential_rule_points_at_the_per_repo_identity_route() {
    let flat = asset("BASE-AGENT.md").replace('\n', " ");
    let rule_start = flat.find("One credential rule").expect("credential rule");
    let rule = &flat[rule_start..];
    let rule = &rule[..rule.find("**A PM `SendMessage`").unwrap_or(rule.len())];
    assert!(
        rule.contains("docs/reference/environment-variables.md#per-repo-gh-identity"),
        "BASE-AGENT's credential rule must point at the per-repo gh identity \
         route in environment-variables.md (#8557)"
    );
}

/// The ops agents that handle credentials name the non-printing form.
/// See #8596 (`local-ops` printed Keychain values while "checking" them)
/// and #8248 (`gcp-ops` ran `print-access-token` bare to see it work).
#[test]
fn ops_agents_state_the_non_printing_credential_forms() {
    let local = asset("local-ops.md").replace('\n', " ");
    assert!(
        local.contains("find-generic-password -s <service> >/dev/null 2>&1")
            && local.contains("Never add `-w` or `-g`")
            && local.contains("#8596"),
        "`local-ops.md` must state the exit-status-only Keychain check (#8596)"
    );
    // #8596 round 2: the value is consumed inline, as the `tm-secrets`
    // skill says, never parked in a variable.
    assert!(
        local.contains("--password-stdin") && !local.contains("=$(security"),
        "`local-ops.md` must consume a Keychain value inline, not via `FOO=$(…)`"
    );
    let gcp = asset("gcp-ops.md").replace('\n', " ");
    assert!(
        gcp.contains("Never run `gcloud auth [application-default] print-access-token`")
            && gcp.contains("Bearer $(gcloud auth print-access-token)")
            && gcp.contains("#8248"),
        "`gcp-ops.md` must forbid a bare `print-access-token` run (#8248)"
    );
}

/// #9158: a Vercel env listing filters to names at the source, so no value
/// reaches the transcript. `local-ops` ran the listing with no such rule.
#[test]
fn vercel_env_listing_prints_names_only() {
    let skill = std::fs::read_to_string(repo_root().join("content/skills/tm-secrets.md"))
        .expect("tm-secrets skill");
    let bodies = [
        ("local-ops.md", asset("local-ops.md")),
        ("vercel-ops.md", asset("vercel-ops.md")),
        ("tm-secrets.md", skill.as_str()),
    ];
    let needles = [
        "vercel env ls <env> | awk 'NR>1{print $1}'",
        "never `--json`",
        "never the unfiltered table",
        "#9158",
    ];
    for (name, raw) in bodies {
        // Case- and wrap-insensitive: the rule may open a sentence or a line.
        let body = raw
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        for needle in needles {
            assert!(
                body.contains(&needle.to_lowercase()),
                "`{name}` must require a names-only Vercel env listing (#9158): \
                 missing {needle:?}"
            );
        }
    }
}

/// `ticketing` and `version-control` are sonnet-tier, never haiku.
///
/// Why (#7274, owner ruling 2026-09-09): both agents carry judgment the
/// haiku tier does not reach. `ticketing` weighs a deduplication
/// disposition and a scope boundary; `version-control` now derives a PR's
/// component labels from its own diff and inherits the project and
/// milestone of the issue its `Refs #N` names. `version-control.md`
/// declared `model: haiku` until that ruling, so this test pins the tier at
/// the one place the harness actually reads it — the asset's frontmatter.
/// A tier lives nowhere else per DOC-61 §3.3, so a prose table that drifts
/// cannot flip the shipped default, and this assertion cannot be satisfied
/// by editing a table.
#[test]
fn ticketing_and_version_control_are_not_haiku() {
    for (name, body) in [
        ("ticketing.md", asset("ticketing.md")),
        ("version-control.md", asset("version-control.md")),
    ] {
        let frontmatter = body
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---"))
            .map(|(fm, _)| fm)
            .unwrap_or_else(|| panic!("`{name}` must open with YAML frontmatter"));
        assert!(
            !frontmatter.lines().any(|l| l.trim() == "model: haiku"),
            "`{name}` frontmatter must not declare `model: haiku` (#7274); \
             got:\n{frontmatter}"
        );
        assert!(
            frontmatter.lines().any(|l| l.trim() == "model: sonnet"),
            "`{name}` frontmatter must declare `model: sonnet` (#7274); \
             got:\n{frontmatter}"
        );
    }
}

/// Worktree removal is PM-executed, and the shipped prompt must say so.
///
/// Why (#5791, owner ruling 2026-08-19): this bullet used to tell every
/// composed agent to "remove your worktree" after a merge, and neither
/// dispatch path could actually carry that out — an unisolated dispatch is
/// denied on the shared HEAD (#4480) and an isolated agent cannot touch the
/// shared registry. `tm hook --pm-guard` now denies the command; this test
/// keeps the prompt from drifting back to instructing what the guard
/// refuses, which is the state that produced the deadlock.
#[test]
fn worktree_removal_is_the_pms_in_the_shipped_base_agent() {
    let flat = asset("BASE-AGENT.md").replace('\n', " ");
    assert!(
        flat.contains("Never remove a worktree") && flat.contains("#5791"),
        "`BASE-AGENT.md` must forbid an agent removing a worktree (#5791)"
    );
    assert!(
        flat.contains("the PM removes the task-owned path")
            && flat.contains("git worktree remove /absolute/repo/.claude/worktrees/task-name")
            && flat.contains("ownership, clean state, merged status and no other live"),
        "`BASE-AGENT.md` must name the PM's replacement command, not just \
         refuse (#5791)"
    );
}

/// Self-analysis is a core property, and the shipped prompt must carry
/// every part of it.
///
/// Why (#6935, owner ruling 2026-09-06): a recommendation is worthless if
/// it lands in the wrong repository, and a subagent that reaches for
/// `ticketing` itself breaks "No Subagent Fan-Out". The facts below decide
/// where a finding goes and whether one is filed at all, so a reword that
/// drops one turns the section back into advice.
///
/// Why (#6937, owner ruling 2026-09-07): the fast loop's memory tag is the
/// contract the scheduled post-mortem queries by. Renaming it in this file
/// without renaming it in `tm-postmortem` breaks the join silently — the
/// query returns nothing and the post-mortem reports a clean fleet. The
/// heuristics heading is asserted for the same reason the section heading
/// is: it is what makes the detection list findable.
#[test]
fn self_analysis_reporting_ships_in_the_base_agent() {
    // #7723 (epic #7681): the full self-analysis/fast-loop prose (~7KB)
    // moved to the on-demand `self-improvement-loop` skill — see
    // `self_improvement_loop_skill_carries_the_moved_anchors` in
    // `trusty-mpm`'s `bundle_tests.rs` for the ten anchors this test used
    // to assert directly. BASE-AGENT keeps only the resident trigger: the
    // section heading, the skill name (named in prose, never a modelled
    // `Skill(...)` call — most agents' `tools:` allowlist omits `Skill`,
    // see `base_agent_prose_rules_survive_composition`), and the memory
    // tag the fast loop records under.
    let flat = asset("BASE-AGENT.md").replace('\n', " ");
    for (fact, needle) in [
        (
            "the resident section exists",
            "## Self-Improvement Reporting",
        ),
        (
            "the trigger names the point in the run",
            "before your final report",
        ),
        // #7727: a Read path, not a skill name — most agents lack `Skill`.
        (
            "the skill file is named",
            "{{TM_SKILLS}}/self-improvement-loop/SKILL.md",
        ),
        (
            "the memory tag survives as a resident fact",
            "`self-improvement-hypothesis`",
        ),
        ("a subagent never files directly", "No Subagent Fan-Out"),
    ] {
        assert!(
            flat.contains(needle),
            "`BASE-AGENT.md` must state that {fact} (#6935, #6937, #7723)"
        );
    }
    assert!(
        !flat.contains("Skill(skill=\"self-improvement-loop\")"),
        "BASE-AGENT must name `self-improvement-loop` in prose, not a \
         modelled `Skill(...)` call — most agents' `tools:` omit `Skill` \
         (#7683, #7723)"
    );
}

/// #7723 fix round (code-critic WARN): 35 of 39 roster agents' `tools:`
/// allowlist omits `Skill` (#7683), so moving the `tm wait` exit-code
/// semantics and the two self-improvement closing-block headings fully
/// into skills left those agents with the prohibitions ("don't narrate a
/// wait") but no resident way to reconstruct the compliance mechanism. A
/// Skill-less agent must be able to emit a correct `tm wait` retry and a
/// correctly-headed closing report from `BASE_AGENT` alone.
///
/// #8107: the retry rule is pinned by the command it names and by the word
/// `verbatim` case-insensitively. The earlier `VERBATIM` needle pinned the
/// emphasis casing, so #8075's rewrite to prose case reddened the test
/// without weakening the rule it guards.
#[test]
fn wait_exit_codes_and_improvement_block_headings_are_resident() {
    let flat = asset("BASE-AGENT.md").replace('\n', " ");
    for (fact, needle) in [
        ("exit 0 (met) is documented", "`0`"),
        ("exit 75 (pending) is documented", "`75`"),
        ("exit 1 (timeout) is documented", "`1`"),
        ("exit 2 (error) is documented", "`2`"),
        ("the command to re-issue on exit 75 is named", "`rerun=`"),
        (
            "the Improvement recommendations heading is named",
            "Improvement recommendations",
        ),
        ("its Symptom field is named", "Symptom"),
        ("its Cause field is named", "Cause"),
        ("its Evidence field is named", "Evidence"),
        ("the Prompt feedback heading is named", "Prompt feedback"),
        (
            "the fast-loop memory tag is named",
            "self-improvement-hypothesis",
        ),
    ] {
        assert!(
            flat.contains(needle),
            "`BASE-AGENT.md` must state that {fact} (#7723 fix round)"
        );
    }
    // #8107: the rule, not its capitalization.
    assert!(
        flat.to_lowercase().contains("verbatim"),
        "`BASE-AGENT.md` must require the `rerun=` command to be re-issued \
         verbatim (#7723 fix round)"
    );
}

/// An engineer's shipped prompt must not name a doc-gate script that only
/// `trusty-tools` contains.
///
/// Why (#7247): `BASE-AGENT.md` asserted "This project: CLAUDE.md sets
/// 500/3000 SLOC via `scripts/check_line_cap.sh`" as a fact about whatever
/// project the agent had been dispatched into, and `rust-engineer.md` told
/// every run to execute all three gates. In `bobmatnyc/trusty-things` none
/// of the three exists, and two `rust-engineer` runs on 2026-09-09 each
/// spent a round trip discovering that. These assets deploy unchanged into
/// every project, so a filename written here is a claim about a checkout
/// this crate has never seen.
/// What: rejects the three literals in the assets an engineer composes
/// from. `version-control.md` is deliberately out of scope — its two
/// mentions describe what `tm pr open` does internally, and that command
/// already degrades to a skip when the script is absent.
/// Test: this IS the assertion.
#[test]
fn engineer_assets_name_no_repo_specific_doc_gate_script() {
    /// Gates that exist in `trusty-tools` and cannot be assumed elsewhere.
    const REPO_SPECIFIC_GATES: [&str; 3] = [
        "check_test_pointers.sh",
        "check_line_cap.sh",
        "check_changelog_fragment.sh",
    ];

    for (name, body) in [
        ("BASE-AGENT.md", asset("BASE-AGENT.md")),
        ("BASE-ENGINEER.md", asset("BASE-ENGINEER.md")),
        ("rust-engineer.md", asset("rust-engineer.md")),
    ] {
        for gate in REPO_SPECIFIC_GATES {
            assert!(
                !body.contains(gate),
                "`{name}` names `{gate}`, a script that exists in \
                 trusty-tools and not in every project (#7247) — tell the \
                 agent to read the project's own CLAUDE.md and `scripts/` \
                 instead of naming a filename"
            );
        }
    }
}

/// A general-purpose agent deploys unchanged into every project, so a
/// `trusty-tools` crate name, script path or repository slug written into
/// one is a claim about a checkout this crate has never seen.
///
/// Why (#7270, #7271, #7287): the fix above covered three doc-gate scripts
/// in three assets, and the same class survived one paragraph higher —
/// `rust-engineer.md` still told every project that `trusty-common` needs
/// `--features` and that `scripts/test_trusty_common_lanes.sh` exists,
/// `documentation.md` required `scripts/check_sld.sh` to pass, and
/// `version-control.md` read branch protection from a hardcoded
/// `bobmatnyc/trusty-tools`. A per-file, per-literal list cannot keep up;
/// this sweeps the whole roster instead so the class cannot return.
/// What: rejects each literal in every bundled asset except the four whose
/// SUBJECT is this framework or this repo's delivery chain, where naming
/// them is the point. `BASE-AGENT.md` keeps one exemption: the
/// self-improvement destination is owner-ruled to be `bobmatnyc/trusty-tools`
/// whatever project the agent ran in (#6935), and
/// `self_analysis_reporting_ships_in_the_base_agent` asserts it.
/// Test: this IS the assertion.
#[test]
fn general_purpose_assets_state_no_repo_specific_literal() {
    /// Crate names, gate scripts, feature names and paths that exist in
    /// `trusty-tools` and cannot be assumed anywhere else.
    const REPO_SPECIFIC_LITERALS: [&str; 12] = [
        "trusty-common",
        "test_trusty_common_lanes.sh",
        "check_sld.sh",
        "check_line_cap.sh",
        "check_test_pointers.sh",
        "check_changelog_fragment.sh",
        "check-pr-version-bump.sh",
        "required-checks.sh",
        "is-branch-caused.sh",
        "memory-core",
        "bobmatnyc/trusty-tools",
        "crates/",
    ];
    /// Assets whose subject IS this framework or this repo's delivery
    /// chain; a literal there names what the agent actually maintains.
    const FRAMEWORK_SCOPED: [&str; 4] = [
        "mpm-agent-manager.md",
        "mpm-skills-manager.md",
        "ticketing.md",
        "version-control.md",
    ];

    for (name, body) in roster().iter() {
        if FRAMEWORK_SCOPED.contains(&name) {
            continue;
        }
        for literal in REPO_SPECIFIC_LITERALS {
            // #6935: the self-improvement destination is fixed by owner
            // ruling, not a claim about the project under work.
            if name == "BASE-AGENT.md" && literal == "bobmatnyc/trusty-tools" {
                continue;
            }
            assert!(
                !body.contains(literal),
                "`{name}` states `{literal}`, which exists in trusty-tools \
                 and not in every project (#7270) — these assets deploy \
                 unchanged everywhere, so point the agent at the project's \
                 own CLAUDE.md and `scripts/` instead of naming a literal"
            );
        }
    }
}

/// The `BASE-*` templates are what every other asset's `extends:` chain
/// roots at. Shipping the roster without them would make composition
/// impossible for every consumer, which is precisely why all 42 moved
/// together rather than only the 30 that were duplicated.
#[test]
fn base_templates_travel_with_the_roster() {
    for base in [
        "BASE-AGENT.md",
        "BASE-ENGINEER.md",
        "BASE-OPS.md",
        "BASE-QA.md",
        "BASE-RESEARCH.md",
    ] {
        assert!(
            roster().get(base).is_some(),
            "`{base}` must ship alongside the agents that extend it"
        );
    }
}

/// The regression-proof recipe must revert to the branch's own PRE-FIX
/// commit, and must spell a gate script as an executable path.
///
/// Why (#7705): `origin/main` moves while an engineer works, so a revert to
/// it can swap in someone else's change and make the "fails before" run
/// prove the wrong thing — or nothing. And an engineer who writes
/// `bash scripts/<name>.sh` inside a Claude Code isolation worktree can have
/// the command refused as unverifiable, which reads as a broken gate rather
/// than a wrong spelling. Both rules are one sentence each in a long file,
/// exactly the shape a later rewrite drops without noticing.
/// What: pins the two load-bearing phrases and rejects the bare
/// `git checkout origin/main --` recipe they replaced.
/// Test: this IS the assertion.
#[test]
fn regression_proof_reverts_against_the_pre_fix_commit() {
    assert!(
        asset("BASE-ENGINEER.md").contains("git merge-base origin/main HEAD"),
        "BASE-ENGINEER must name the pre-fix commit the revert targets (#7705)"
    );
    assert!(
        asset("BASE-ENGINEER.md").contains("never to a bare"),
        "BASE-ENGINEER must forbid reverting to a bare `origin/main` (#7705)"
    );
    assert!(
        !asset("BASE-ENGINEER.md").contains("`git checkout origin/main -- <paths>`"),
        "the moving-ref revert recipe must be gone, not merely warned about (#7705)"
    );
    assert!(
        asset("BASE-ENGINEER.md").contains("`./scripts/<name>.sh`"),
        "BASE-ENGINEER must state the executable gate-script spelling (#7705)"
    );
}
