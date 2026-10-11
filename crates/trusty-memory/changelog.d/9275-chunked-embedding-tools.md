Changed
- `palace_verify_embedded` reports a drawer with missing chunk rows under `missing`, and `verified` is false while the chunk-count check could not run. `palace_reembed` reports and repairs short drawers (`short`, `still_short_ids`, `chunk_check`), and `palace_embed_sweep` rows carry `short` and `chunk_check`. The kuzu import rewrites a drawer's whole chunk group (#9275).
