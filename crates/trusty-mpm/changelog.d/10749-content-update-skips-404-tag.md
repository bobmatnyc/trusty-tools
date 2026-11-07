Fixed
- `tm content update` passes over a highest `content-v*` tag whose release is a draft or missing (GitHub answers 404) and installs the next lower stable release, instead of failing until the draft is published or deleted. A 403 or 5xx on the release lookup still fails the update and keeps the pin (#10749).
