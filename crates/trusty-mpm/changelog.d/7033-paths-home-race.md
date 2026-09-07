Fixed

- Serialize the two `core::paths` tests that read `$HOME` twice and compare the results, so a concurrent `$HOME`-repointing writer in the `--lib` binary can no longer split the two reads and fail the run (#7033).
