Fixed
- Session prep writes the #1939 palace alias only to the trusty-memory
  registry its caller names. `maybe_register_palace_alias` takes the registry
  dir, and the launch threads it from its host inputs, so a launch with an
  injected registry no longer registers the alias in the real one. Production
  launches still use the registry the trusty-memory daemon reads.
