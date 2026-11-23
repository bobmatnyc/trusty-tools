Fixed

- The `gh` CLI ticketing backend reads every label on a repository. `list_available_tags`
  ran `gh label list` with no `--limit`, so `gh` returned its default first 30 and said
  nothing about the rest — on a repo with more labels than that, every label past the first
  page read as absent and the ensure-labels path re-created labels that already existed.
  The listing now asks for 1000 labels through one named argv builder, and a page that comes
  back exactly that full is an error rather than a set treated as complete
  ([#6953](https://github.com/bobmatnyc/trusty-tools/issues/6953)).
