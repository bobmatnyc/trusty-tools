Changed
- The `tm-delegation-patterns` skill says who applies a local Terraform root whose state lives in the main checkout: the operator or `local-ops` dispatched there, while a worktree agent may run `terraform plan|apply -state=<that state>` (#8660).
