Fixed

- Spec-docs discovery now reports `unavailable`, naming the index, when that index is listed without a `root_path`; it used to drop every absolute search hit silently and look as if it had found nothing (refs [#9593](https://github.com/bobmatnyc/trusty-tools/issues/9593))
