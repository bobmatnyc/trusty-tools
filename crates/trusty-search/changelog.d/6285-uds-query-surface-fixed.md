Fixed

- **A cold-parked index reports the retryable `-32002` end to end, proved through both transports** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). Slice 2 proved only the permanent half of the 503 split against a live daemon; the retryable half is the one a consumer acts on, because such an index is registered, built, and one search away from serving.
