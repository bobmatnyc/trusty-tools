Fixed

- A session in a local-only repository gets `GH_TOKEN` and `GH_ENTERPRISE_TOKEN` set empty instead of the `tm-local-only-repo-no-gh` placeholder, so MCP children such as trusty-review no longer send the placeholder to GitHub; gh stays disabled.
