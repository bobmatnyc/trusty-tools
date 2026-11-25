Fixed
- On Linux, the build-lease census counts a lone multi-threaded `rustc` (or any compiler with no build driver above it) as one foreign build, not one per thread. The process sampler now folds sysinfo's per-thread rows into their process before grouping, which also stops a build's CPU figure and `rustc ×N` count from including its threads (Refs #11867)
