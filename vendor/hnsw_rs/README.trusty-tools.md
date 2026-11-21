# hnsw_rs 0.3.4, patched (#10682)

Upstream: crates.io `hnsw_rs` 0.3.4, checksum
`43a5258f079b97bf2e8311ff9579e903c899dcbac0d9a138d62e9a066778bd07`. The tree
is the published crate, unchanged except for the diff below. Upstream licenses
are kept: `LICENSE-MIT`, `LICENSE-APACHE`.

Wiring: `[patch.crates-io]` in the workspace `Cargo.toml` points `hnsw_rs` here,
and `[workspace] exclude` keeps this directory out of the members. Remove both
once upstream ships the fix.

Why: `reverse_update_neighborhood_simple` held the new point's neighbour read
lock while it took each neighbour's write lock. Two parallel inserts that are
mutual neighbours each hold one read lock and wait for the other's write lock,
so they deadlock (#10682).

What the patch changes: in that function only, the neighbour list for each
level is copied under the read lock, the read lock is dropped, and then each
neighbour's write lock is taken. Nothing else differs from upstream.

Drop this copy, the `[patch.crates-io]` entry, the `exclude` entry and the
three `vendor/hnsw_rs` rows in `.line-cap-allowlist.tsv` once upstream releases
the fix.

Upstream fix: https://github.com/jean-pierreBoth/hnswlib-rs/commit/95f9023838a6d6a9af0b4e05f4a9a4e73bc0dcc6
("fix deadlock in reverse update by cloning", 2026-10-28, on upstream main as
0.3.6, unreleased). It applies the same clone-then-write change. Drop this copy
when hnsw_rs 0.3.6 or later is on crates.io.

```diff
--- a/src/hnsw.rs
+++ b/src/hnsw.rs
@@ -1234,7 +1234,12 @@
         );
         let level = new_point.p_id.0;
         for l in (0..level + 1).rev() {
-            for q in &new_point.neighbours.read()[l as usize] {
+            // #10682: copy the neighbour list and drop the read lock before
+            // taking any neighbour's write lock. Holding new_point's read lock
+            // across q's write lock deadlocks two parallel inserts that are
+            // mutual neighbours (upstream 0.3.4 lock-order inversion).
+            let neighbours_at_l = new_point.neighbours.read()[l as usize].clone();
+            for q in &neighbours_at_l {
                 if new_point.p_id != q.point_ref.p_id {
                     // as new point is in global table, do not loop and deadlock!!
                     let q_point = &q.point_ref;
```
