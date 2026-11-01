Fixed
- `tctl install` and `tctl upgrade` no longer report success when the content `tm content update` pinned is older than a placed trusty-review: after the update they run the placed binary's `content check`, and a failure exits non-zero naming the missing file. A trusty-review older than that verb is reported as not checked (#10588).
