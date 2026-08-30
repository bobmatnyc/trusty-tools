Fixed

- `resolve_palace` / `resolve_palace_with_remote` no longer return a pin file's
  `palace` field unvalidated. The env-override, git `owner/repo`, and
  `parent/dir` levels all run their result through `clamp_palace_id`, so each
  returns an id `palace_id_is_valid` accepts; the pin level returned its field
  verbatim, which was the one way an id the daemon's creation gate would refuse —
  a dotted `tripbot.tours`, a `../evil` traversal shape, an over-long slug —
  could reach a palace directory name and a Unix-socket filename. The pin's value
  is now trimmed and checked before any level decides, and an id that fails the
  check is reported as the new `PalaceResolveError::PinInvalid`, naming the pin
  file and the rejected value. The pin is never rewritten to a different palace:
  an untrustworthy pin fails closed here exactly as an unparseable or empty one
  already did. Every well-formed pin resolves unchanged. (#6418)
