Documentation

- `tm-workflow`'s "Shipped Defaults on the PR" section now documents `tm pr open`'s exit 3 (`EXIT_PARTIAL`, #7869): the PR was created but a metadata field still failed after one retry, and the caller should finish it by hand rather than treat the run as failed (Refs #7868).
