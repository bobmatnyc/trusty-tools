Fixed

- Exclude Terraform state blobs from indexing by default: `*.tfstate` (any stem — `terraform.tfstate`, `dev.tfstate`, `prod.tfstate`) and `*.tfstate.backup`, matched on file name/extension at any depth. A repo-wide grep for a common word once returned noise dominated by a large `.tfstate` blob instead of relevant source lines. A directory merely named `tfstate` is unaffected — only the basename is matched (#7722).
