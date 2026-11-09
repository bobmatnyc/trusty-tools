Added
- Internal plumbing for `tm tui` to open, rename, create and delete a session the way `tm ls` does: the `tm` binary registers its `tm ls` functions as session hooks, and the tui suspends and restores its terminal around an attach. No key reaches these actions yet, so nothing changes for the user (Refs [#10754](https://github.com/bobmatnyc/trusty-tools/issues/10754))
