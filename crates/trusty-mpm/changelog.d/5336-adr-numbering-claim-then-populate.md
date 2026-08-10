Documentation

- The `tm-adr` bundled skill's numbering convention now names a claim-then-populate
  protocol: check `docs/adr/` on `origin/main` (not a possibly-stale local
  checkout) and any in-flight branch or open PR before picking a number, create
  the ADR file first as a stub reserving that number, then populate it. Also
  states what to do on discovering a collision — report it, never silently
  renumber another author's ADR. Closes a gap two same-day collisions exposed:
  ADR-0021 (pre-existing, out of scope here) and a same-day ADR-0038 collision
  caught only by an alert author.
