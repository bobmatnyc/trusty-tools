Documentation
- `ManifestLoad` and `AgentManifest::load_checked` referred to
  `quarantine::sweep_locked` as a rustdoc link. That function is private to its
  own module, so no path resolves to it from `manifest` and the link rendered as
  dead text. It is plain code text now (#5973).
