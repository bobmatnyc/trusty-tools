Fixed

- A failed Azure DevOps work-item fetch now fails the collection stage instead of being swallowed (#5219). `collect` caught the error from the `workitemsbatch` call, logged it at warn level, and returned success, so `tga collect` exited 0 with an empty `work_items` table and no way to tell that from a run that legitimately found no `AB#` references. The failure is now recorded as a stage failure, which `tga collect` turns into a non-zero exit (#5655). Linear already did this at every one of its failure arms; ADO — the writer everyone treats as precedent — did not.
