Documentation

- The `version-control` agent asset's "Opening every PR" table row now documents `tm pr open`'s exit 3 (`EXIT_PARTIAL`, #7869): the PR exists but some metadata could not be applied, and the printed line names the PR number, URL, and the missing field(s) to finish by hand (Refs #7868).
