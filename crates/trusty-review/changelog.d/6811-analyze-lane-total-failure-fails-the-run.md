Changed

- `trusty-review report --analyze` now FAILS, with a non-zero exit and a message
  naming the lane and both counts, when the analyze lane assessed nothing at all
  (`0 of N` applications). Such a report carries a finding count, a complexity
  figure and a health factor for every application and not one of them was
  measured, which #6783 shipped across 59 repositories and downstream readers
  took for "static analysis ran and found nothing" (#6811).
- The new `--allow-degraded` flag writes that report anyway, with the `0 of N`
  coverage line still in it. Partial degradation (`M of N`, `M > 0`) stays a
  warning at every setting and never fails the run: a 58-of-59 run carries 58
  assessed applications (#6811).
