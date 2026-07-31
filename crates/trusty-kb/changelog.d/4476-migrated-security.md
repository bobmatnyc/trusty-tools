Security

- Doc-store ingestion is confined to operator-configured roots (see
  `okg::policy` above). Without it, `okg_ingest_docstore` — reachable from the
  default base assistant — was an arbitrary local-file-read primitive: a path
  supplied by the model could walk `/etc` or `~/.ssh` into a KB tree that is then
  searchable and quotable in chat. Because ingested content is itself untrusted,
  a prompt-injected document could also name the next path to read.
