Added

- `tga audit` — a strictly non-interactive acquisition-diligence command taking `--org`, `--title`, `--analyst`, `--client`, `--output`, and `--weeks`. Once started it never prompts, confirms, or waits for input (#5235, DOC-67 §2).
- `tga::audit::run_full_sweep` — a library entry point that drives the eight data-collection subcommands (collect, classify, jira sync, deployments, incidents, dora, pr-metrics, report) end to end with no TTY and no clap. It returns per-stage outcomes, so a failed stage is named rather than aborting the run or reading as a clean pass (#5217, DOC-67 §9). `tga audit` calls it instead of re-sequencing the subcommands itself (#5237).
- `tga::commands` is now part of the library rather than private to the binary, so the sweep reuses each subcommand's existing `run` function instead of a second copy of its logic.
