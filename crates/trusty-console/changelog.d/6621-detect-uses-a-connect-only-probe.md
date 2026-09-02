Changed
- The Trusty Analyze card's status now comes from a connect-only socket probe
  rather than an `analyze.health` RPC on every poll. The version is still shown:
  it is read once per daemon lifetime and cached against the socket's inode, so
  a respawned daemon is re-read while an open dashboard no longer dials the
  daemon four times a minute (#6621).
