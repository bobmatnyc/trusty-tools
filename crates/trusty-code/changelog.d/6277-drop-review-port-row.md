Changed

- trusty-review is removed from `DEFAULT_HTTP_PORT`'s port-collision guard table. It binds a Unix socket rather than a TCP port since #6277 (ADR-0032), so reserving 7891 against it would only forbid a future daemon a free port. Comment-and-table only; `tcode serve --http` still defaults to 7882 ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
