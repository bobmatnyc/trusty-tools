Added

- `tga audit --no-render` writes the manifest and stops. tga renders the report
  in the same process that writes the manifest, so a caller that grounds the
  manifest afterwards — trusty-audit does — could only ever ground a file the
  report had already been built from. This flag lets that caller ground first
  and render second. A bare `tga audit` still renders.
