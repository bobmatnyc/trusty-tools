# Parity: golden capture, harness, shadow runs

## Golden capture from the Python

Record one case per input. Each case stores:

- argv (after the program name), working directory and the env vars the
  program reads (not the whole environment)
- stdin bytes
- stdout bytes, stderr bytes, exit code
- the files written or changed, as paths plus content hashes
- the input files themselves, copied into the corpus

Store cases as JSON or directories under `tests/golden/`. Commit the corpus.
Pick inputs from real use first, then add edge cases: empty input, huge
input, non-UTF-8 bytes, Unicode names, a missing file, a bad flag.

```python
# capture.py: run the Python and record one case
import json, subprocess, sys
proc = subprocess.run([sys.executable, "-m", "tool", *argv],
                      input=stdin, capture_output=True)
case = {"argv": argv, "exit": proc.returncode,
        "stdout": proc.stdout.decode("latin-1"),
        "stderr": proc.stderr.decode("latin-1")}
```

Use `latin-1` to keep the bytes exact. Store base64 when a byte string is not
text at all.

## Side-by-side harness

The harness runs both implementations on each case, in the same working
directory, with the same env subset. It then compares fields in a fixed
order: exit code, stdout, stderr, then the written files.

- Copy the input tree for each run. Two implementations must not share a
  scratch directory.
- Run in a clean temp directory per case. Remove it after the run.
- Print the first differing line for each failing field. A diff summary alone
  hides the cause.
- Exit non-zero on any unexplained difference.

## Normalizing known-irrelevant diffs

Some output differs on purpose or by nature: timestamps, PIDs, temp paths,
random IDs, help text layout, log line prefixes. Normalize these, and only
these.

- Keep the normalization list in the harness as named rules, one per field,
  with a reason.
- Replace the value with a placeholder, such as `<TS>` or `<PID>`. Do not
  delete the line. A missing line is a difference.
- Review the list at each port. A growing list is a sign that parity is
  weakening.

## Property tests

Golden cases cover the inputs you thought of. Property tests cover the rest.
Hypothesis strategies do not translate to proptest; the shared artifact is
the corpus, not the test code.

- Python: `hypothesis` generates inputs, shrinks a failure to a minimal case,
  and saves failing examples in its example database.
- Rust: `proptest` generates inputs for in-process tests, and saves the seeds
  of failing cases under `proptest-regressions/`. Commit those files.
- For a cross-language check, write a Hypothesis `@given` test that writes
  each generated input into the corpus, then run both implementations on the
  written cases. Fix the generation with `@seed(n)` or
  `settings(derandomize=True)` so the same corpus regenerates. Do not use
  `strategy.example()` for this; Hypothesis warns that it is for interactive
  use only.
- Add a Hypothesis or proptest failure to the golden corpus as a fixed case,
  so it stays covered after the generators change.
- Test properties the port must keep: round trips, ordering, idempotence,
  and the rejection of invalid input. Compare outputs, not internals.

## The parity bar

The port passes parity when all of these hold:

1. Every golden case matches on exit code, stdout, stderr and written files,
   after the listed normalizations.
2. Property tests run with no unexplained difference over an agreed count of
   generated inputs.
3. Each intentional difference is in the port ledger with its reason.
4. The live check ran once in the real environment with a real input.

Set the generated-input count and the shadow period per project. Write them
in the ledger row before the port starts, so the bar does not move later.

## Shadow runs

A shadow run executes the new implementation next to the live one, on real
input, without effects.

- Run the new version read-only, or direct its writes to a scratch location.
- Log the comparison: both outputs and a diff, one record per run.
- Keep the live implementation as the source of effects until cutover.
- Investigate every difference. Classify each one as a bug in the port, a
  bug in the Python, or an intentional change to record.
- A shadow run ends when it holds clean for the agreed period.

Shadow runs catch what the corpus misses: real data shapes, load, and timing.
They do not replace the corpus, because they only see what happened to run.
