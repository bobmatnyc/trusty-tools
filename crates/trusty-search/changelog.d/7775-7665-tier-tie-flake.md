Fixed

- `search`: same-tier filename matches are ordered by their fused lane score instead of by chunk id, so a `src/lib.rs`-shaped query no longer floors the alphabetically-first eight files above everything the semantic lanes ranked; a strictly better path-suffix match still outranks a basename-only one (refs [#7775](https://github.com/bobmatnyc/trusty-tools/issues/7775))
- `config_over_the_socket_matches_the_http_body` no longer flakes under parallel runs: it pins both process-global memory limits through the daemon's own setter seam and is serialized against every writer of those cells (refs [#7665](https://github.com/bobmatnyc/trusty-tools/issues/7665))
