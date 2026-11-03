Fixed

- The Session Manager delegate's evidence scan no longer accepts failed or empty results as proof (Refs [#10721](https://github.com/bobmatnyc/trusty-tools/issues/10721)). A failed session record is `Failed` even when its pane shows evidence. Test evidence needs a positive pass count and zero failures, and any failing summary in the pane vetoes other evidence. A write claim must name a written path. A PR URL counts only when the delegated task named that PR or `gh pr create` in the pane reports opening it, and the task text echoed into the pane is ignored.
