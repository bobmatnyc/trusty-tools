Removed

- `Dreamer::start`, the shutdown-less spawn path, which nothing in the workspace called and which would have reintroduced the un-staggered, un-capped loop (#7106). Use `start_with_shutdown`, which now takes a first-tick stagger.
