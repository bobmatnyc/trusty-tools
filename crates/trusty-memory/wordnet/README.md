# Vendored WordNet data

`lemma-pos.txt` is a **projection** of Princeton WordNet 3.1, not a copy of it.
`src/wordnet_pos.rs` embeds it with `include_str!` and binary-searches it in
place; `src/kg_extract.rs` uses the result to tell a property word (`hard`)
from an entity word (`requirement`) when it extracts KG triples (#5399).

## What was projected away

WordNet's `index.{noun,verb,adj,adv}` carry, per record: the lemma, a POS
character, sense and pointer counts, the pointer symbols, a tagsense count, and
one synset offset per sense. The extractor reads the **first field and nothing
else**. So the projection keeps one line per lemma — `<lemma>\t<mask>` — and
drops the rest, along with every multi-word (`_`-joined) lemma, which the
extractor could never match because it only ever looks up single
whitespace-delimited tokens.

| | bytes | records |
|---|---|---|
| upstream `index.*` (4 files) | 6,305,332 | 91,092 lines |
| `lemma-pos.txt` | 979,462 | 83,253 lemmas |

6.44× smaller, and it is the whole of what ships in the binary.

`mask` is a decimal bitfield: `NOUN 1`, `VERB 2`, `ADJ 4`, `ADV 8`. So
`hard 12` is adjective-or-adverb-but-never-noun, and `fast 15` is all four.

## Regenerating

Do this when WordNet publishes a new release, or if the file is ever suspected
of corruption. There is deliberately **no `build.rs` step** — this crate's
`build.rs` already owns the Svelte UI build, and a second job in it would make
every `SKIP_UI_BUILD=1` developer build depend on data that changes once a
decade.

```bash
curl -O https://wordnetcode.princeton.edu/wn3.1.dict.tar.gz
tar xzf wn3.1.dict.tar.gz          # -> dict/index.{noun,verb,adj,adv}

SKIP_UI_BUILD=1 cargo run --release -p trusty-memory --example wordnet_project \
    -- dict crates/trusty-memory/wordnet/lemma-pos.txt
```

The generator prints a per-file byte/record report and the reduction ratio.
Then re-run the crate's tests: `the_shipped_table_is_sorted_and_parseable`
re-checks the sort order, the mask range, and the multi-word exclusion against
the committed file, and `shipped_table_answers_the_four_pos_classes` pins the
record count — update that count in the same commit as the data.

## Licence

WordNet 3.1, Copyright 2011 Princeton University. SPDX `WordNet` — permissive,
MIT-compatible, no copyleft. Its one substantive obligation is that the
copyright notice appear on **all** copies, so the notice lives in two places
here: verbatim in [`LICENSE`](LICENSE), and again as the `#` header of
`lemma-pos.txt`, because the projection discards the upstream files' own
headers. Any future re-projection must keep carrying it — the generator writes
the header from a constant for exactly that reason.
