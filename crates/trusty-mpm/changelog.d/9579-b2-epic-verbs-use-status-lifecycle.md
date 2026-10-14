Fixed
- In a repo with no `issue-state.yaml`, `tm issue epic sync` and `tm issue epic create` now render a `status:*`-labelled phase through the built-in `status:*` lifecycle, as `tm issue transition` does, instead of resetting its row to `open` under the Unicorn default. A model read from a file is never replaced (#9579).
