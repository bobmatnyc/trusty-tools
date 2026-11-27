Fixed
- `tm project decommission` no longer unloads another project's memory palace. When another active project derives the same palace id (for example `/a/code/app` and `/b/code/app` both derive `code-app`), or its own palace id cannot be resolved, the palace unload is skipped and the report names the other project. The decommission still completes.
