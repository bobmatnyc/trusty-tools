# Evidence-based testing and guard proportionality

Task ar-T74 (goal ar-G01). Research and proposal only. No guard, test or source file was changed. Date: 2026-11-06.

## 0. Conclusion

1. The owner's arithmetic is right. 100 calls at 0.5% per call, independent and constant: expected errors 0.5; P(at least 1) = 1 - 0.995^100 = 39.42% (Poisson approximation 39.35%). The two numbers answer different questions and should not be mixed: 0.5 is a mean, 39.4% is a tail event.
2. The model is a useful sanity check, not an evidence base. The 0.5% is itself an estimate, calls are correlated, and the number of lifetime calls is uncertain. Observing zero errors in 100 calls cannot tell 0.5% from about 3% (section 2).
3. Probability is one input. A rubric needs reachability, severity, recoverability, detectability and control cost as separate axes (section 3). No single numeric cutoff survives contact with this: a 0.1% event that destroys credentials outranks a 20% event that costs a retry.
4. Compare four controls per risk: test, refusing guard, log-only guard plus recovery, no added control (section 4). Trusty already has the log-only mode and a denial store, so most of the pilot is measurement, not new machinery.
5. The pm-guard defends a non-adversarial, error-prone agent. That makes "would an agent produce this input unprompted" the main reachability question. Most shapes found in the #10706 review rounds are contrived compounds; the one ordinary-use finding (kill words inside a commit message) was left unfixed (section 6).
6. Pilot: 6 to 8 weeks, observation-first, using data that already exists (section 7).

## 1. Primary sources

Each entry was checked against Crossref (DOI resolves to the stated author, year, title, venue) or a publisher/repository page on 2026-11-06. Status noted per entry.

### 1.1 Risk-based testing

- ISO/IEC/IEEE 29119-1:2022, *Software and systems engineering - Software testing - Part 1: General concepts*. https://www.iso.org/standard/81291.html. Existence and edition verified via several standards-body listings (SCC, IEC, AFNOR); the ISO page itself blocked automated fetch. Listings show a section on risks and risk management and say risks and requirements are the basis of a test strategy. I could not read the normative text (paywalled), so I do not quote clause content. Clause numbers (a search summary suggested 4.2) are UNVERIFIED.
- Amland, S. (2000). Risk-based testing: risk analysis fundamentals and metrics for software testing including a financial application case study. *Journal of Systems and Software* 53(3). doi:10.1016/S0164-1212(00)00019-4. Verified (Crossref). Risk exposure = probability of failure x cost of failure, used to order testing effort.
- Felderer, M. and Schieferdecker, I. (2014). A taxonomy of risk-based testing. *Int. J. Software Tools for Technology Transfer* 16(5). doi:10.1007/s10009-014-0332-3. Verified (Crossref). Survey of how risk is assessed and used in test planning.

### 1.2 Operational profiles and software reliability engineering

- Musa, J. D. (1993). Operational profiles in software-reliability engineering. *IEEE Software* 10(2). doi:10.1109/52.199724. Verified (Crossref). Allocate test effort in proportion to how the system is actually used.
- Musa, J. D. (2004). *Software Reliability Engineering: More Reliable Software Faster and Cheaper*, 2nd ed. AuthorHouse. ISBN 978-1-4184-9387-5 (paperback) per retailer listings. Existence verified by listings only; the page count and ISBN vary across sellers. No DOI.
- Anderson et al. is not cited; an earlier guessed DOI for it was wrong and is discarded.

### 1.3 Bayesian reliability and rare-event bounds

- Hanley, J. A. and Lippman-Hand, A. (1983). If nothing goes wrong, is everything all right? Interpreting zero numerators. *JAMA* 249(13). doi:10.1001/jama.1983.03330370053031. Title, year, journal and Hanley as first author verified (Crossref). Crossref lists only Hanley; the second author is from memory and is UNVERIFIED. This is the origin of the rule of three: after n events with zero failures, the 95% upper bound is about 3/n.
- Jovanovic, B. D. and Levy, P. S. (1997). A look at the rule of three. *The American Statistician* 51(2). doi:10.1080/00031305.1997.10473947. Verified (Crossref).
- Brown, L. D., Cai, T. T. and DasGupta, A. (2001). Interval estimation for a binomial proportion. *Statistical Science* 16(2). doi:10.1214/ss/1009213286. Verified (Crossref). Shows the normal-approximation interval fails at small counts; use Wilson, Jeffreys or exact intervals.
- Littlewood, B. and Strigini, L. (1993). Validation of ultrahigh dependability for software-based systems. *Communications of the ACM* 36(11). doi:10.1145/163359.163373. Verified (Crossref). Failure-free operation or testing can only support modest claims; ultra-high claims cannot be demonstrated from operating history.
- Butler, R. W. and Finelli, G. B. (1993). The infeasibility of quantifying the reliability of life-critical real-time software. *IEEE Trans. Software Engineering* 19(1). doi:10.1109/32.210303. Verified (Crossref).
- Strigini, L. and Povyakalo, A. (2013). Software fault-freeness and reliability predictions. *SAFECOMP 2013*, LNCS 8153, pp. 106-117. https://openaccess.city.ac.uk/id/eprint/2457. Verified (repository page; the DOI I first guessed was wrong, and the unpaywall link suggests 10.1007/978-3-642-40793-2_10, which I did not confirm against Crossref, so the DOI is UNVERIFIED). Conservative Bayesian prediction without a precise prior.

### 1.4 Correlated and non-independent failures

- Knight, J. C. and Leveson, N. G. (1986). An experimental evaluation of the assumption of independence in multiversion programming. *IEEE Trans. Software Engineering* SE-12(1). doi:10.1109/TSE.1986.6312924. Verified (Crossref). Independently written programs failed together more often than independence predicts.
- Eckhardt, D. E. and Lee, L. D. (1985). A theoretical basis for the analysis of multiversion software subject to coincident errors. *IEEE Trans. Software Engineering* SE-11(12). doi:10.1109/TSE.1985.231895. Verified (Crossref). Failure probability varies by input, which makes failures positively correlated.

### 1.5 Cost-benefit and decision methods

- Howard, R. A. (1966). Information value theory. *IEEE Trans. Systems Science and Cybernetics* 2(1). doi:10.1109/TSSC.1966.300074. Verified (Crossref). Expected value of information: what a measurement is worth before you buy it.
- Raiffa, H. and Schlaifer, R. (1961). *Applied Statistical Decision Theory*. Harvard Business School. Book cited from memory; UNVERIFIED (not checked in this session).
- Beyer, B., Jones, C., Petoff, J. and Murphy, N. R. (eds.) (2016). *Site Reliability Engineering*, ch. 3 "Embracing risk". O'Reilly. https://sre.google/sre-book/embracing-risk/. Verified (page loads). Accept a stated error budget rather than seek zero errors; trade reliability against cost.
- Saltzer, J. H. and Schroeder, M. D. (1975). The protection of information in computer systems. *Proceedings of the IEEE* 63(9). doi:10.1109/PROC.1975.9939. Verified (Crossref). Fail-safe defaults and psychological acceptability: a control people route around fails its purpose.

### 1.6 Unverified summary

UNVERIFIED: ISO 29119-1 clause numbers and normative wording; Hanley and Lippman-Hand second author; Strigini and Povyakalo DOI; Raiffa and Schlaifer (book). Everything else above was checked on 2026-11-06.

## 2. Limitations of the owner's example

Arithmetic, checked:

| Quantity | Value |
|---|---|
| Expected errors, 100 x 0.005 | 0.5 |
| P(at least 1), 1 - 0.995^100 | 0.3942 |
| Poisson approximation, 1 - e^-0.5 | 0.3935 |
| Same, 1000 calls | 0.9933 |
| Same, 10 calls | 0.0489 |
| Same, p = 0.1%, 100 calls | 0.0952 |

Where the assumptions fail:

- **The rate is an estimate.** If 0.5% came from a count, it has an interval. With zero errors in 100 calls the 95% upper bound is 1 - 0.05^(1/100) = 2.95%, close to 3/n. A sample that small does not separate 0.5% from 3%. Propagating the uncertainty means P(at least 1) = 1 - E[(1-p)^n] over a posterior for p (for example Beta), not a point plug-in. The result is wider than 39.4% in both directions.
- **Independence.** Agent calls share context, a session, a habit, a prompt and a repository. A model that writes one risky compound this session is more likely to write another. Positive correlation raises the chance of a cluster and lowers the chance of "at least one" for the same mean, so 39.4% is neither a bound nor an average. Knight-Leveson and Eckhardt-Lee are the standard evidence that independence is unsafe even for separately built programs.
- **Constant rate.** The rate varies with input type (Eckhardt-Lee), with the task, with prompt and model changes, and with each guard or fix that ships. Lifetime pooling hides that.
- **Uncertain exposure.** "100 lifetime calls" must be counted. In Trusty the hook runs on every Bash call, so n is large and may be knowable from the hook path, but I found no exposure counter in the denial store. Without n, a rate cannot be computed from denials alone.
- **Rare-event limit.** Littlewood-Strigini and Butler-Finelli show failure-free history supports only modest claims. For an event that must be rarer than about 1/n, the history cannot show it. That argues for design-time arguments (a structural guard, a minimum protection) rather than waiting for data.
- **Expected count is the wrong yardstick for severity.** 0.5 expected minor retries and 0.5 expected deleted repositories have the same count and very different cost.

## 3. Axes the rubric must keep separate

- **Probability** of the event, with an interval and an exposure basis.
- **Severity** of the loss if it occurs.
- **Recoverability**: can the effect be undone, how fast, at what cost.
- **Detectability**: will anyone notice, and how soon. An undetected, unrecoverable event is worse than a loud, recoverable one at the same probability.
- **Reachability**: can the input arise in ordinary use by this population, or only by a constructed compound.
- **Ordinary defect frequency vs adversarial risk.** Frequency comes from usage statistics. Adversarial risk is a property of what a motivated actor can reach, and does not shrink with observed frequency: absence of attacks in history is weak evidence about attacks to come. The pm-guard population is an error-prone agent, not an adversary, so it should be judged on the ordinary axis. The adversarial axis still matters when the same guard also protects against prompt-injected content (the CLAUDE.md "PM Authority" split treats tool output as untrusted). Keep the two columns separate in every record.

## 4. Costs and the four controls

Costs on each side, per risk:

| Side | Cost |
|---|---|
| Guard | False positives (blocked legitimate work, retries), review rounds to build, parser complexity, maintenance as the shell grammar and tooling change, and guard-bypass search (#10706 spent three review rounds) |
| Test | Writing and maintaining the test, CI time, flake risk; catches regressions in code, not agent behaviour |
| Observation + recovery | Logging, someone reading it, a recovery path; loss is accepted once, then bounded |
| Avoided loss | Probability x severity x (1 - recoverability), plus the cost of detection delay |

Comparison:

| Control | Good when | Weak when |
|---|---|---|
| Test | The risk is a code regression; behaviour is deterministic | The risk is agent input diversity; cannot enumerate shapes |
| Refusing guard | Severe or unrecoverable, plain shape is cheap to match, low false-positive rate | Ordinary legitimate commands share the shape; matching needs a growing parser |
| Log-only guard + recovery | Moderate severity, recoverable, rate unknown | Severe or unrecoverable events, where the first occurrence is the loss |
| No added control | Low severity, recoverable, loud, unreachable in ordinary use, and control cost exceeds expected loss | Loss floor cases (section 5) |

Log-only is the cheapest information-purchase: it prices the hypothesis (a rate and a false-positive profile) before paying for a refusing control. Expected value of information (Howard 1966) frames the choice: collect data when the decision could change and the data is cheap.

## 5. Cases that need minimum protection regardless of frequency

These are protections to keep at a floor, justified by severity and irrecoverability, not by a measured rate. For each, "minimum" means a refusing control on the plain, ordinary shape, not unbounded hunting of every compound bypass.

- Credential or secret exposure (disclosure cannot be revoked by undoing the call; the existing secret-file, secret-print and secret-copy guards fit this).
- Data loss that cannot be recovered: destructive deletes, force-push over shared history, removing a worktree with unmerged work.
- Destructive control-plane commands whose effect spans sessions (whole-server tmux kill, daemon stop).
- Writes to protected paths that govern the guard itself (hooks, trust anchors).

Why the floor holds: rare-event data cannot show a very low rate (section 2), and an unrecoverable loss at probability 1/1000 per exposure still occurs at fleet scale. The floor is a decision about severity, set once, not recomputed from observations. The floor does not imply the maximal version of the guard. After the plain shape is refused, more rounds on contrived bypass shapes need the section 8 rubric, not the floor.

## 6. Trusty grounding

### 6.1 What the guards are

- pm-guard is the `tm hook --pm-guard` PreToolUse hook. Bash rules live in `crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/` (about 90 files: credential_print*, destructive_delete, force_push, secret_file_copy, worktree_remove*, read_only_*, tmux_exact_target, and others).
- Outcomes: a rule either denies the call (a refusal with a reason) or, for the tmux kill-server family under owner ruling Dac, returns `KillVerdict::LogOnly`, a record of what it would refuse with no block. `tmux_kill_server_tests.rs` lines 3 and 39-43 state it: "`refused` rows refuse, `logged` rows are log-only". Shape detail is in `tmux_kill_server*.rs` (present on branch `fix/10706-phase2-c2-h1`).
- Every deny is appended to a denial store: `pm-guard-denials.jsonl` under the trusty-mpm data directory (`crates/trusty-mpm/src/daemon/bug_report/denials.rs`, `PM_GUARD_DENIALS_FILE`; `pm_guard_deny_log.rs`). Design is in `pm_guard_deny_log.rs` lines 1-12 (#8722).
- A guard is also an ALLOW-list in places. `pm_guard_bash/persistence.rs` documents that an allow-list with an unrecognised-construct hole is worse than a deny-list hole, and that the fix was to invert to default-deny rather than add a third enumerated bypass. That is the same "stop chasing shapes" lesson #10706 repeats.
- CLAUDE.md test ladder: rungs 1-6 by change class (docs, test-only, local behaviour, cross-crate, contract/persistence/security/release, UI/API), risk Low/Normal/High, with "scope down, never scope away". The rubric below extends it; it does not replace it. CLAUDE.md fix bar: a `code-critic` finding below HIGH, or a self-improvement finding, is not a new issue; one that meets the Completion Standard fix bar is fixed in the surfacing PR, otherwise it becomes one comment, is dropped, or is logged in the rollup (#8021).

### 6.2 Worked case: #10706

Read with `gh issue view 10706 --comments` (read-only). Branch `fix/10706-phase2-c2-h1`, three review rounds on kill-server bypass shapes. I classify by reachability for a non-adversarial agent, by my own reading. These are judgments, not measurements.

| Finding | Shape | Ordinary-use reachable? | Class |
|---|---|---|---|
| C1 first | `cat <<EOF \| tmux kill-server ... sh`, `{ echo 'tmux kill-server'; } \| sh` | An agent has no reason to echo a kill command into a shell | Contrived compound |
| Round 1 HIGH | `{ echo ...; } > x.sh; sh x.sh`, `done > >(sh)`, `exec > >(sh)`, `coproc` | Needs a script written to be run, with kill text inside | Contrived compound |
| Round 2 HIGH (1) | `(echo 'tmux kill-server'; true) > x.sh; sh x.sh` | Same | Contrived compound |
| Round 2 HIGH (2) | `f() { echo 'tmux kill-server'; }; f > >(sh)` | Same | Contrived compound |
| Round 2 HIGH (3) | `{ exec > >(sh); }; echo 'tmux kill-server'`; `command exec`, `\exec`, `X=1 exec` variants | Same | Contrived compound |
| Below fix bar | `time coproc {...}; sh <&"${COPROC[0]}"` | Same | Contrived compound |
| Below fix bar | Ordinary commits that mention the kill words in a message produce log-only records (`substitutions_printed_only`, `pipes_into_readers_only`) | Yes: commit messages, docs and issue text mention kill-server routinely | Ordinary use, false positive |
| Below fix bar | Test gaps (`&&`/`||` operator rows, `fi > >(sh)`, `esac >> f`, `>\|`, `&>`, `3>`) | Mixed; mostly contrived | Coverage gap |

What the rubric says about this record:

- Severity: whole-server tmux kill takes down every session of the fleet. Recoverability is partial and slow (sessions and in-flight work lost). That meets the section 5 floor for the plain `tmux kill-server`, which #10705 already shipped. It does not extend the floor to every shell indirection.
- The shapes that consumed the review rounds (every HIGH after round 1) need an agent to construct a program whose output is piped or redirected into a shell, containing the kill text. A non-adversarial agent has no path to produce them unprompted. By the pm-guard's own threat model, expected frequency is near zero and the benefit of closing each shape is the severity times that frequency, which is small, while each round has a measured cost (a review round per shape family, growing parser logic, more log-only records, as the critic itself notes). Those shapes are an adversarial-axis concern. Whether that axis applies is an owner decision that this report does not make; it only notes that the pm-guard was framed against the error-prone agent.
- The one ordinary-use finding is the false-positive stream from commit messages. It is the only item on the list a normal session will hit repeatedly, and it was recorded as "noted, not fixed". The rubric points the other way: the item that can occur at real frequency was deferred while the contrived items were fixed.
- Log-only is already the chosen outcome for most of this family (ruling Dac), which is the cheap observation control from section 4. That makes the ranking of the three review rounds an empirical question the denial data can answer: how many log-only records came from kill words in commit text, and how many from any of the listed contrived shapes.

### 6.3 Data Trusty already has

- `pm-guard-denials.jsonl`, in the trusty-mpm data directory (on this machine `~/Library/Application Support/trusty-mpm/pm-guard-denials.jsonl`). Snapshot taken read-only on 2026-11-06: 8257 records, timestamps 2026-09-27 to 2026-11-06. Top checks: read-only-dispatch 4382, trust-anchor-write 1150, secret-file-read 1048, main-checkout-commit 818, secret-file-copy 186, main-checkout-write 101, worktree-remove 61, subagent-fanout 59, worktree-remove-recheck 57, destructive-delete 56. Only 6 records mention `kill-server`.
- Data quality caveat: at least 825 records carry a temporary-directory working directory (`/var/folders` or `/tmp`), which looks like test and live-check runs mixed into the same store. Separate those before computing any rate.
- Each record carries check slug, tool, cwd, session id, command and reason. It records denials only; I did not find an exposure count (calls allowed) or a label for whether a denial was legitimate. Both are needed for a rate and a false-positive proportion. Whether log-only (would-refuse) kill-server records go to the same file is not established from my read; UNVERIFIED, check in the pilot.
- Hook audit POST to the daemon `/hooks` (per `pm_guard_deny_log.rs`) may hold allowed-call counts; not inspected.
- GitHub issue and PR history (the review-round counts and finding classes in #10706 and siblings #10665, #10704, #10705), CI run history and test counts per crate via `gh` and the test-ladder docs, unrun here.

## 7. Pilot proposal

Aim: measure, for a small set of guards, ordinary-use hit rate, false-positive share, and review cost, then compare against what the rubric predicted at design time. No change to any control during the pilot. Decide afterwards.

1. **Pick scope.** Two guards: one from the section 5 floor (for example secret-file-read) as the control group, and the kill-server family as the test case. Pilot window 6 to 8 weeks.
2. **Fix the exposure count.** Add (or locate) a per-guard count of hook invocations that the guard evaluated, so rate = hits / exposures. If none exists, the first deliverable is that counter in the hook audit. This is the only new instrumentation.
3. **Separate the store.** Tag records from test and live-check runs (temporary cwd) and exclude them from rates.
4. **Label a sample.** Draw a random sample of hits per guard. Classify each as legitimate-use hit (false positive), true prevented error, or contrived. Reviewer: a person or a single agent, with a second reviewer on 10% for agreement. Record the sample size and report the interval (Wilson or Jeffreys, Brown-Cai-DasGupta), not a point value.
5. **Record cost per guard.** Review rounds, files and lines changed, test count added, and retries imposed by false positives, taken from the PR and issue history (retrospective for #10665, #10704, #10705, #10706).
6. **Record the prediction.** Before reading the data, write for each guard: reachability class, severity, recoverability, expected hits per 1000 exposures with a stated range.
7. **Report.** For each guard: rate with interval, false-positive share with interval, cost, severity and recoverability, compared with the prediction. Include prior-versus-observed misses.
8. **Expected value of information check.** Before extending the pilot, ask whether any plausible result would change the decision. If not, stop.

What the pilot cannot show: rates below about 1/n of the exposures observed (section 2), anything about an adversary who has not yet acted, or correlated bursts if only the pooled rate is reported. Report burst structure per session (hits per session, not just per call).

Deliverable at the end: one table per guard and a short owner-facing recommendation for each of: keep, demote to log-only, promote to refuse, stop adding shapes, remove.

## 8. Decision rubric (no universal cutoff)

Apply per risk, in order. Record the answers; the record is the output.

1. **Floor check.** Is the loss irreversible, undetectable, or cross-session (section 5)? If yes, a minimum refusing control on the plain shape is required; skip steps 2 to 4 for that shape. Go to step 5 for bypass shapes beyond the plain one.
2. **Reachability.** Which population produces this input, and how? Ordinary use by the error-prone agent / needs a deliberate construction / needs an adversary. Write the path in one sentence. If you cannot, treat it as unreachable and say so.
3. **Rate and uncertainty.** Observed hits and exposures with an interval, or a stated prior range if no data. Mark if the estimate rests on fewer than a few dozen events (interval too wide to rank) or on zero events (use 3/n as the upper bound, not 0).
4. **Loss and recoverability.** Severity, time to undo, who notices and when.
5. **Control cost.** Estimated build and review cost, parser or rule complexity added, false-positive profile on ordinary commands, ongoing maintenance.
6. **Compare controls** (section 4): test, refusing guard, log-only plus recovery, none. Pick the one whose expected cost (control cost plus expected residual loss) is lowest, with the tie going to the cheaper-to-reverse control. Document why each rejected option was rejected.
7. **Stopping rule for bypass shapes.** After the plain and ordinary shapes are covered, a further shape enters scope only if (a) step 2 finds an ordinary-use path, or (b) the owner states the adversarial axis applies to this guard. Otherwise log it as a log-only record or an observed-not-fixed note.
8. **Review.** Re-run steps 2 to 4 when log-only data arrives, when the control changes, or when a miss occurs. Misses are data: add them to the sample.

Interaction with the test ladder: the ladder sets how much verification a change needs (what rung). The rubric sets whether a new control or an extra bypass-shape round is worth building at all. Both apply to the same PR.

## 9. Assumptions and gaps

- Reachability classes in section 6.2 are my reading of the issue text. They need the step 4 sampling to confirm.
- I did not read private material about prompt review and did not contact anyone.
- I did not run the code, any build or any test. The denial-store figures are a read-only parse of a local file and reflect this machine only.
