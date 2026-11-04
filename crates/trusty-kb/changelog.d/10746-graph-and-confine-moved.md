Changed
- `graph` (the tree graph reader), `store::summarise` and the lexical `roots::confine` check now live in trusty-okf and are re-exported or wrapped under their old paths; `structure::GENERATED_MARKER` takes its text from trusty-okf (#10746). A `confine` refusal message no longer names the rejected component.
