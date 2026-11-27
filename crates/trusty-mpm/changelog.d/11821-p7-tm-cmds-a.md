Changed
- The pm-guard trust-anchor check names the `.trusty-mpm` and `.trusty-tools` dirs through the shared `install_paths` constants, and the guard deny text that names a home path for the reader carries a `path-literal: allow` waiver; the anchor set and every deny message are byte-identical (#11821).
