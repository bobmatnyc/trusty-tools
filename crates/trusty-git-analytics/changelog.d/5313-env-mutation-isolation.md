Fixed

- Tilde expansion, `${VAR}` placeholder expansion, and the GitHub-token
  validation check each take the value they used to read from the process
  environment as a parameter, so their tests no longer mutate `std::env`.
  Two of those tests both set `HOME`, and one would read the other's value:
  `alias_file_path_expansion` failed twice in 200 paired runs with
  `failed to read aliases file /home/testuser/aliases.yaml`. Behaviour and the
  public API are unchanged — `expand_path`, `AliasFile::load`,
  `database_path::resolve`, and `expand_env_var` still read the same variables
  and still return the same results.
