Security
- A read-only consent (`setup --scopes readonly`, `add_account {scopes:"readonly"}`) now checks the scopes Google granted against the read-only preset. A grant with any other scope is refused, nothing is stored, and the error names the extra scopes (#10873). Full-scope consent is unchanged.
- A read-only consent now requires `--profile` (`add_account`: `profile`). Without one it is refused before any side effect, so the default `gworkspace-mcp` profile keeps its full-scope token (#10873).
