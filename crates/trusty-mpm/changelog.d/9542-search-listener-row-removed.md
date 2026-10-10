Changed
- `tm doctor`'s `tcp_listeners` row fails, not warns, on a trusty-search TCP listener: its allowlist row is gone, because no trusty-search process binds TCP since #9214 and #9542. A trusty-search older than 0.59.0 still binding 7878 now grades FAIL; upgrade it.
