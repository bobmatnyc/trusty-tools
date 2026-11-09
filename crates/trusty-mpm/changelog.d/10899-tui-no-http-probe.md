Fixed
- `tm tui` no longer probes the Agents and Console tiles over HTTP. Both services have no UDS endpoint yet, so their tiles read `no UDS endpoint` in grey with the server-side gap as detail, and are not counted as down (#10899).
