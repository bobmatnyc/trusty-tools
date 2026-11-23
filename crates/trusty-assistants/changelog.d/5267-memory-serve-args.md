Changed

- The bundled `trusty-memory` MCP service entry now uses `args = ["serve"]`; the
  `--stdio` flag became redundant when bare `serve` started meaning MCP stdio
  (#5267). The `trusty-mpm` entry keeps `--stdio`, whose semantics are unchanged.
