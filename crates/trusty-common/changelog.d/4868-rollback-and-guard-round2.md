Fixed

- Rollback no longer reports success while the service is down. `bootstrap`
  boots out first, so on the "no previous plist but the label was loaded" path
  the running job is already gone by the time rollback executes — deleting the
  plist and returning "nothing was taken down" produced service down, plist
  gone, and a message denying both. That path now keeps the plist just written
  and bootstraps it, because the displaced job had no plist on disk and cannot be
  reconstructed; a failed revival reports the outage instead of swallowing it.
  The effects are injected so the outcome is asserted rather than the plan value,
  which is why the previous tests passed while the goal was unmet (#4868)
- `LaunchdConfig` renders a `WorkingDirectory` when one is set, so a regenerated
  unit can preserve the installed one's (#4868)
- `evict_legacy` is public, so `service uninstall` can remove a unit registered
  under an old label rather than reporting "nothing to do" (#4868)
