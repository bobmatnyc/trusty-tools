Fixed

- The workflow engine's post-plan and post-code relocation helpers no longer
  treat a failed existence probe as "the destination is absent"
  (closes [#5551](https://github.com/bobmatnyc/trusty-tools/issues/5551)).
  Eight `try_exists(...).unwrap_or(false)` gates in
  `workflow/engine/helpers.rs` collapsed *undeterminable* into *absent*, so a
  transient `EIO`/`ETIMEDOUT`/`ESTALE` on the destination could rename a stray
  file from the project root over a real generated output, over `out_dir`'s
  authoritative `assignments.json`, or merge-copy over `out_dir/stubs/` and
  then `remove_dir_all` the source tree — each one logged as a successful
  relocation. An undeterminable probe now aborts that item's relocation
  (nothing moved, overwritten, or deleted) and surfaces as an error; a genuine
  "does not exist" is unchanged. The same pass stops counting an unresolvable
  file as a clean skip: it is reported instead of vanishing from both `moved`
  and `skipped_too_old`. A failed `copy_dir_all` fallback on `stubs/` also
  stops firing the "relocated to out_dir" warning it never earned. Governed by
  ADR-0045.
- An unstattable stray at the project root is recorded the same way rather than
  skipped silently: `metadata` failing there means the recency gate cannot be
  evaluated, so the file was previously counted in neither `moved` nor
  `skipped_too_old`.
