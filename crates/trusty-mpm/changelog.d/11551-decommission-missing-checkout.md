Fixed
- `tm projects decommission` no longer refuses with "indexes may still be loaded" when the project's checkout is gone: a missing checkout and a project with nothing to resolve now count as not loaded (#11551).
