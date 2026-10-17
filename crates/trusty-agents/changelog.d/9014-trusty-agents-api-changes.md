Breaking
- `agents::bundled::deploy_bundled_agents`, `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents` take the tree to deploy as a new first argument, `&BundledTree`, read with `BundledTree::load` or `BundledTree::from_content`. `agents::bundled::content` (`BundledTree`, `BundledTreeKind`, `BUNDLE_DIR`) and `agents::bundled::log_deploy_failure` are new (#9014).
