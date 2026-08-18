Added

- `trusty-audit audit` runs a whole engagement in one invocation — install the
  pinned tools, clone the registered repository targets, sweep them with `tga
  audit`, and assemble the return package — instead of `install`, `clone`, `run`
  and `package` in order. The four separate verbs are unchanged.
- The chained run is resumable: interrupting it and running it again carries
  over installed tools, complete checkouts and audited repositories.
- A phase that fails names the phase it failed in — install, materialize,
  collect or package — rather than reporting "the audit failed".
- A sweep that audited nothing stops before packaging, so a failed collection
  cannot produce a zip that looks like a finished engagement. A sweep that
  partly failed still packages, names what it omits, and exits non-zero.
