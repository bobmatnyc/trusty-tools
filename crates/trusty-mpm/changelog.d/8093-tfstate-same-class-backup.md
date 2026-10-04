Fixed
- pm-guard allows `cp` of a secret-bearing file to a sibling in the same directory whose name is itself in the secret class, such as a dated Terraform state backup beside the state file or `.env` to `.env.bak`. A dated `*.tfstate.*` backup is now in the secret class, so reading it is refused like the state itself, and so is naming it in `terraform state pull > …` (#8093).
