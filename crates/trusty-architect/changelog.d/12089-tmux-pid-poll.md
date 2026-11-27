Fixed
- `system_tmux_times_out_and_reaps_the_child` no longer flakes on a loaded host: the test polls (bounded, 5 s) for the child's pid file to be non-empty before parsing it. Test-only; no production change (#12089).
