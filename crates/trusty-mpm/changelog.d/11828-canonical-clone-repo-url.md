Fixed

- `tm run <owner>/<repo>` now registers its managed clone with `repo_url` `https://github.com/<owner>/<repo>` instead of the checkout path, so `tm tui` lists the project under its owner and the project row counts its sessions; a session with no `repo_url` in that clone still binds to the row. Other checkouts, and every checkout while `install.toml` is broken, keep the path form. ([#11828](https://github.com/bobmatnyc/trusty-tools/issues/11828))
- `tm run` refuses an unreachable daemon before it clones, so no unregistered clone is left under the repos root. ([#11828](https://github.com/bobmatnyc/trusty-tools/issues/11828))
