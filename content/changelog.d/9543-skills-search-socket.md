Documentation
- `tm-cli-operations` says `tm services` probes trusty-search by calling `search.health` on its Unix socket (`TRUSTY_SEARCH_SOCKET`, else the data directory): UP only when the socket answers, DOWN with the dial error otherwise. `tm services port|url trusty-search` exit 1 naming the socket, and an old static port-7878 `services.yaml` entry is read as the socket probe with one `WARN` ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-doctor` names the trusty-search check as reached over its Unix socket, not port 7878 ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
- `tm-circuit-breaker` shows a raw socket probe, not a `curl` to port 7878, as the forbidden PM Bash example ([#9543](https://github.com/bobmatnyc/trusty-tools/issues/9543))
