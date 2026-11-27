Fixed

- `derive_github_path` and `derive_remote_repo` now ignore an inherited `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE` or other repository-redirecting variable, so they always read the directory's own `origin`. The redirect list and its scrub, `git::GIT_ENV_REDIRECTS` and `git::remove_redirects`, are now public in trusty-common (#11217).
