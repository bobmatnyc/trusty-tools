Security

- A session relaunched in place no longer receives a `github.token_env` binding's token as a `GH_TOKEN` value in its environment, where any process running as the same user could read it (`ps -E`). tm now proves the token with `GET /user`, stores it in gh's keyring entry for that account, and gives the session the per-call gh shim from #9750. The session's environment also drops the variable `token_env` names. If any step fails, the relaunch is refused, and the token never goes into the environment instead (#10487).
