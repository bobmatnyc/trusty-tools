Fixed
- `tm projects decommission` no longer refuses with "indexes may still be loaded" when the project's checkout is gone (#11551).
- With no checkout, decommission derives the palace id from the project's remote or recorded path, asks trusty-memory whether that palace exists, and unloads it. An absent palace is skipped and reported as skipped; a failed lookup still refuses the decommission (#11551).
