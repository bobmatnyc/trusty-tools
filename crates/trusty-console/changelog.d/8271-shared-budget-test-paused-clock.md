Fixed

- `a_slow_open_and_a_silent_first_frame_share_one_budget` no longer fails on a loaded host or CI runner: it runs on tokio's paused clock, so encoding its 4 MiB request frame no longer eats the 400 ms margin inside the 1 s open budget. The test still fails when the open and the first-frame read take separate deadlines. No production behaviour changed (refs [#8271](https://github.com/bobmatnyc/trusty-tools/issues/8271))
