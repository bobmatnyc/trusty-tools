Added

- Trusty Console carries a robot brand identity in the Trusty Agents family: an
  operator UNIT seated at a dashboard panel, drawn on the same head geometry as
  the agents mark so the two read as one machine doing different jobs. Assets
  are `docs/design/UI/icons/trusty-console-{mark,favicon,logo,logo-reversed}.svg`.
- The header shows that mark with the "Trusty Console" wordmark and a
  `UNIT-05 · SERVICE CONSOLE` descriptor, replacing the gradient heading and the
  "Unified service dashboard" subtitle. The Foundry identity is flat, so the
  gradient is gone rather than restyled.
- The overview panel shows the mark while services are still being detected, and
  the app ships a favicon — a browser tab showed the generic page icon before.
- One mark serves both palettes: its chassis and face read from
  `--trusty-accent` and the new `--trusty-mark-face` token, so it recolors with
  the theme instead of shipping a reversed twin.
