Fixed

- The bundled `version-control` agent's conflict-detection instructions now name `git merge-tree --write-tree HEAD origin/main` as the check, with GitHub's `mergeable` field as the tiebreak — the legacy three-argument `git merge-tree <base> <a> <b>` form reported a merge clean when both flagged it as conflicting.
