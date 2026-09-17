SECURITY VERDICT: PASS — zero CRITICAL, zero HIGH, zero MEDIUM, zero LOW findings in the scoped passage implementation. Stage 5 gate satisfied for the hashes below and the stated operator-controlled run boundary.

## Scope and findings

Reviewed `experiments/trusty-memory-passages/{passage_policy.py,passage_evaluate.py,test_passage.py,README.md}` against the research, interface and protocol under `docs/research/trusty-memory-passages-2026-09-17/`. Followed the existing adapter, helper, private writers, offline tokenizer and packet-integrity boundaries where reused. The prior real-validation security review supplied context; current adapter/evaluator hashes match that review.

No security findings require remediation. This is a focused source review and synthetic execution result, not production or real-input validation. No actual private snapshot, prompt sample, gold, rankings, live database, model artifact or credentials were read. No Git commands, ticket changes, Rust builds, dependency audits, live export or production mutations were performed. Reviewer writes are task-specific artifacts under `/tmp` and synthetic temporary files, which were removed after the checks.

## Observed protections

- `passage_evaluate.py:238` resolves TMPDIR and rejects missing, non-directory, group/world-accessible and enclosing-Git locations before tokenizer/index initialization. It sets Python temporary storage to that root. The inherited helper receives a scratch directory from the native-index adapter; its graph/index files remain beneath the private root.
- `passage_evaluate.py:256` uses the inherited exclusive private-directory writer. Fixed artifact names at `:287` and `:292` use exclusive mode-0600 writes. The reused writer rejects resolved Git ancestry and refuses existing regular/symlink targets. The synthetic run verified mode 0700/0600, refusal to overwrite, Git/symlink rejection and native scratch cleanup.
- `passage_evaluate.py:315` prints only the exception class on ordinary evaluation failure. Success prints the operator-supplied output path at `:317`. Direct `evaluate` emitted no stdout. A source scan found no credential literals or eval/exec, unsafe deserialization, shell construction, URL or network calls in the new implementation.
- `passage_policy.py:352` computes eligibility over the complete original corpus. `:357` rejects ineligible or forged seeds; `:359` rejects partly ineligible source expansion. `:363` rebuilds passage registration from authoritative original sources, then compares the entire expected packed result and invokes the unchanged packet validator. Tests reject changed bodies, missing sources, forged packets/sidecars, superseded facts, scope mismatch and invalid byte spans.
- Stored text is parsed as JSON and handled as strings. Regex anchors are escaped. Neither note text nor prompt text becomes a filename, shell command or Python program. An independent synthetic corpus included shell substitution and Python-like execution instructions; the text survived into packets while its filesystem sentinel remained absent.
- The tokenizer verifies the pinned local cache SHA256 and has no download fallback. The helper runs as one explicit executable with an argv list and private pipes. Python model construction and network access were denied during the guarded run without any attempted violation.

## Verification results

Final command:

```text
PYTHONDONTWRITEBYTECODE=1 /Users/masa/trusty-search-experiment/venv/bin/python /tmp/passage-security-checks.py
EXIT=0
24 passed, 1 deselected in 2.02s
PASS: injection text preserved without execution; silent evaluate; private modes; invalid/missing/Git/symlink TMPDIR rejection; native scratch cleaned
PASS: source/helper hashes unchanged; guards {"cli_calls": 3, "forbidden_attempts": 0, "helper_execs": 5}
```

Log: `/tmp/passage-security-checks-final.log`. Harness: `/tmp/passage-security-checks.py`. The guard denies Python socket operations, model construction, real private-root/database/model reads and execution other than the exact frozen native helper. It runs the existing CLI test through `main()` in-process so those guards remain active. Its three calls exercise success, output collision and input-hash mismatch. The independent evaluator run exercises all three arms and both budgets against invented records, including command-like stored text.

`test_exact_quarter_support_across_hash_seeds` was deliberately deselected from this restricted run because it starts Python subprocesses. This is an explicit guard boundary, not an unobserved skip. Directly inspected final engineer logs provide the separate unguarded subprocess test evidence:

```text
/tmp/passage-engineer-tests-final.txt
25 passed in 2.81s
/tmp/passage-engineer-mypy-final.txt
Success: no issues found in 2 source files
```

That full suite includes the hash-seed regression for seeds 1, 7 and 99 and the real CLI subprocess smoke. The security rerun used the final sorted-term/math.fsum source; no threshold change or new runtime dependency was introduced. An earlier provisional guarded run also passed 24 tests before this determinism correction.

## Coverage and limits

OWASP coverage: A01/A04/A05 reviewed for scope eligibility and private filesystem boundaries; A02/A09 for hashes, content-bearing output and ordinary diagnostics; A03/A08 for injection and source/data integrity; A10 for network/URL paths. A06 dependency CVE/license scanning was not repeated because no dependencies changed and the parent excluded that work. A07 service authentication is not applicable to this local operator CLI. This does not certify the whole repository against OWASP.

Python guards do not sandbox native child syscalls. The frozen native helper was source-reviewed; this run does not prove system-wide network isolation or native memory safety. The review assumes the operator controls executable/cache/input paths, parent directories and environment, and preserves reviewed code and input hashes during execution. Same-user directory races, hostile replacements of the supplied helper, resource exhaustion from a much larger corpus and crash/panic diagnostics were not exercised. Preserve private stdout/stderr logs for the actual run, use umask 077, explicit private TMPDIR and the pinned offline cache, and audit owned temporary residue on failure.

The parent continues with independent input/gold checks and the authorized private measurement. Recheck security if a reviewed implementation/helper hash changes. No live-behavior or retrieval-quality claim is made here.

## Reviewed SHA256

All new/reused Python modules in the five experiment directories and the helper were hashed before and after the final guarded run. Full manifest: `/tmp/passage-security-hashes.json`. Selected hashes follow.

| File | SHA256 |
| --- | --- |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-passages/test_passage.py` | `4554805e6717b9681346c6cbe78f2b043b6d4478915687bce2ca5975357c62bf` |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-passages/passage_policy.py` | `cdd884f24cb377f837731fcb4aeb909ab3f7512c3ad08b2b0ef97c51b8b598e4` |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-passages/passage_evaluate.py` | `30cc941090ce2183fb4a2d00c9ee9749753e9821fb15d430a3b33731e909178b` |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-real-validation/real_evaluate.py` | `fa890920e0269c77564247eecd6531d8d27e5b21e14e52897327a8f876840f88` |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-real-validation/real_adapter.py` | `7088a00b608612ac544c6ebebc7e9ca3f5c10eff7491b98b0944446f459661cd` |
| `/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe` | `6e30d87ddd012025db474cfa67d6607bfebd1584368f9076ff0812fee204af25` |
| `/Users/masa/trusty-search-experiment/worktree/experiments/trusty-memory-passages/README.md` | `f3d2c0d0645368175dab2caacece1da93df13fbcc32d3a05d9419c660913a553` |
