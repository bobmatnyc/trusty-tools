Fixed

- `grep` over an idle-evicted index no longer rehydrates the whole corpus to learn which files to scan: it lists the file set straight from the durable corpus, decoding only each row's `file` field, and the index stays evicted. A one-file grep on a large cold index no longer pays the full rehydrate, and an unreadable corpus still answers `503 index_corpus_unavailable` (refs [#8266](https://github.com/bobmatnyc/trusty-tools/issues/8266))
