Changed

- The default working directory is `~/.trusty-tools/trusty-audit/work`, and the
  packaged launcher no longer pins `--work-dir` beside itself. The tree used to
  land wherever the recipient unzipped the package, and `trusty-search` refuses
  to index a checkout under `/tmp`, `/var/folders`, `~/Downloads`, `~/Desktop`
  or `~/Documents` — which is where an emailed zip gets opened — so the
  placement cost the code analysis its data on an ordinary machine. `--work-dir`
  and `TRUSTY_AUDIT_WORKDIR` still override, and the launcher forwards them.

  Two costs, both stated in the package README: approving a clone writes a row
  to `trusty-search`'s own allowlist, outside the work root, so deleting the
  root is no longer a complete uninstall (`trusty-search index remove <path>`
  undoes it); and an existing operator's tree does not move itself — pass
  `--work-dir` at the old location, or move it.
