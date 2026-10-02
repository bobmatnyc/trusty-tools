Changed

- `tctl ensure` writes the trusty-mpm `.mcp.json` entry as `tm serve --stdio`
  instead of `trusty-mpm serve --stdio`. An existing `trusty-mpm` entry keeps
  working through the alias and is rewritten on the next `tctl ensure`.
