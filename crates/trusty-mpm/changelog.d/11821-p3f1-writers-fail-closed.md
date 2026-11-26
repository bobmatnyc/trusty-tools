Fixed
- With a broken `install.toml`, `--account-token-stdin` (`tm sessions new`, `tm launch`, `tm <url>`) refuses with an error naming the file before storing the token, instead of storing it under `$HOME/.trusty-mpm/gh-accounts` (#11821).
- With a broken `install.toml`, the console auto-resume toggle (`auto_resume_set`) refuses with an error naming the file instead of writing `$HOME/.trusty-mpm/auto_resume` (#11821).
- With a broken `install.toml`, building a per-login gh account dir (`tm run --account`, `tm projects register --account`, the daemon's account clone) fails with an error naming the file instead of creating it under `$HOME/.trusty-mpm/gh-accounts` (#11821).
- With a broken `install.toml`, `tm session archive` refuses with an error naming the file instead of moving session data into `$HOME/.trusty-tools/archive` (#11821).
- With a broken `install.toml`, a session launch whose project sets `github.token_env` refuses with an error naming the file before it proves or stores the token, instead of writing its account dir and keyring entry under `$HOME/.trusty-mpm/gh-accounts` (#11821).
