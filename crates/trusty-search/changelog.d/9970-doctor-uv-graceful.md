Fixed
- On Apple Silicon with `TRUSTY_EMBEDDER` unset or `auto`, `trusty-search doctor` reports an unusable uv (`TRUSTY_UV_FETCH=0` with no uv, a bad `TRUSTY_UV_BIN`) and an unresolvable venv layout as warnings, as it already did for a missing launcher: the daemon stays on ort. With `TRUSTY_EMBEDDER=python` they stay errors (#9970).
