Fixed

- The launcher calls `trusty_agents::run_to_completion()` instead of driving `run()` from `#[tokio::main]`, so it inherits the bounded runtime shutdown. Under the old shape a background task stuck in a syscall made the process un-exitable, because the runtime drop waits on the blocking pool with no ceiling ([#3655](https://github.com/bobmatnyc/trusty-tools/issues/3655))
