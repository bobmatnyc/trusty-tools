SECURITY VERDICT: PASS — no credential findings after triage in the full branch diff and the two new passage directories. Credential-only pre-push gate; this does not expand the earlier code review.

## Exact scan scope

- Base `origin/main`: `90b6aeb944e1d010f3690281ec24efe7b89435c3`.
- Head `HEAD`: `af5ecf8c0b67badc989fe24d02bf6dd3c42b359c`.
- Merge base: `90b6aeb944e1d010f3690281ec24efe7b89435c3`.
- `git diff --no-ext-diff --no-textconv --binary origin/main...HEAD`: 437 changed files, 17,759,516 bytes. Diff SHA256: `c83e9332ad2ec61c6c5ce01c98d0c1db70e85aa2715c85cf084b5e15e3aa6357`.
- The diff includes 161 gzip binary artifacts from preserved prior experiments. Scanned their decoded HEAD blob contents separately: 147,405,154 bytes, 33 batches of at most five files.
- Current `experiments/trusty-memory-passages/` and `docs/research/trusty-memory-passages-2026-09-17/`: 27 public code/docs/evidence files, 179,289 bytes including path headers. Combined scan-input SHA256: `cf1d2e1aae6893634f26b2d095156c201e0520e185a8be9c71bd552661736d95`.

The two directories were staged additions, not yet part of HEAD, and were therefore scanned separately. Python bytecode caches were excluded. Source refs remained unchanged during scanning. No Git writes, source changes, dependency audit, tests, network calls, private-input reads or baseline edits were performed.

## Scanner results and triage

Used installed Gitleaks `8.30.1` with its default rule set explicitly enabled, full redaction, no baseline, no project ignore file, and inline allow comments disabled. Raw diff stayed in process memory. Logs/reports used a mode-0700 task scratch directory with umask 077. No credential values were emitted.

```text
branch-diff: exit_code=1; 6 generic-api-key matches
passage-directories: exit_code=0; 0 findings
decoded gzip: gzip_files=161; decoded_bytes=147405154; nonzero_batches=0; findings=[]
```

All six raw-diff matches are public tokenizer/cache digest metadata. Each location was reread in the current checkout. These are false positives, with no secret value reproduced below:

| File | Line | Classification |
| --- | ---: | --- |
| `docs/research/trusty-memory-prompt-enrichment-2026-09-17/interface.md` | 212 | Documented tokenizer SHA256, 64 hexadecimal characters |
| `experiments/trusty-memory-deterministic/offline_encoding.py` | 10 | Pinned tokenizer cache filename digest, `CACHE_KEY`, 40 hexadecimal characters |
| `experiments/trusty-memory-prompt-enrichment/adapters.py` | 16 | Pinned artifact SHA256, `TOKENIZER_HASH`, 64 hexadecimal characters |
| `experiments/trusty-memory-prompt-enrichment/offline_encoding.py` | 10 | Pinned tokenizer cache filename digest, `CACHE_KEY`, 40 hexadecimal characters |
| `experiments/trusty-memory-prompt-enrichment/results/run-01/provenance.json` | 63 | `model_tokenizer_hash` artifact SHA256 |
| `experiments/trusty-memory-prompt-enrichment/results/run-01/provenance.json` | 393 | `prompt_tokenizer_hash` artifact SHA256 |

No exceptions, suppressions or baseline modifications were added. `git status --porcelain .secrets.baseline` returned empty output.

## Evidence and limits

- `/tmp/passage-security-branch-scan.py` and `/tmp/passage-security-branch-scan.json`: commands, refs, counts, input hashes, safe finding metadata and full path inventory.
- `/tmp/passage-security-branch-gzip.py` and `/tmp/passage-security-branch-gzip.json`: decoded-file counts/hashes, per-batch status and safe finding metadata.
- Redacted Gitleaks JSON/logs: `/var/folders/7s/g9twvy8j0wl58ffgzsmrccv40000gp/T/passage-branch-credentials-qi1721s6/`.

This is a credential-pattern scan, not proof that arbitrary private prose or every possible credential format is absent. The separately completed passage public-artifact review covers its requested content boundary. Prior experiment implementation quality and dependencies were not re-reviewed. Git binary patches were covered as diff bytes and their resulting gzip blobs were scanned after bounded decompression; superseded decoded binary content in other history was not a full-history scan target.

Parent/version-control continues with preservation and push for this checked branch state and the separately scanned staged passage additions. No further scan is required unless those contents change.
