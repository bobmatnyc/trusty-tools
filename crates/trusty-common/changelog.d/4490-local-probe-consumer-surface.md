Added

- `local_probe` gained `list_models` (the served model ids, from the same `/v1/models` request the liveness probe issues) and `local_host` (the one reader of `OLLAMA_HOST`), so a caller that needs the catalog or the host no longer keeps its own HTTP call or its own default. `config keys test local` now answers what an operator actually asked — whether the local model server is up, as the new `ProbeOutcome::Reachable` naming the endpoint, or a failure carrying the probe's own text — instead of Bedrock's "authenticates via the AWS credential chain", which it inherited by being keyless
