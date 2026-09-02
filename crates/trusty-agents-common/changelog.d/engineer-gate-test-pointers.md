Fixed

- `rust-engineer.md` and `BASE-AGENT.md` now name `check_test_pointers.sh`
  alongside `check_line_cap.sh` and `check_changelog_fragment.sh` in the
  pre-return doc gates. Three engineer PRs (#6656, #6659, #6670) went red on
  the required "Doc-comment pointer lint (Why/What/Test)" CI job because no
  engineer's gate list named the script.
- `security.md`'s secret-detection protocol no longer tells the agent to pass
  `--baseline .secrets.baseline` with a partial file list — `detect-secrets
  scan --baseline <path> <files>` rewrites the baseline at `<path>`, dropping
  every entry for a file not in the list, and it truncated the tracked
  baseline from 4240 lines to 2 twice in one day. The protocol now scans
  against a scratch copy of the baseline, or with no `--baseline` at all, and
  ends with `git status --porcelain .secrets.baseline` confirmed empty.
