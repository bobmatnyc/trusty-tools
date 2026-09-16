Fixed

- A launch-on-main session no longer drops `TASK.md` into the operator's live checkout; the brief reaches the runtime through the spawn and stays on the session record, so the checkout's `git status --porcelain` is unchanged (refs [#7879](https://github.com/bobmatnyc/trusty-tools/issues/7879))
