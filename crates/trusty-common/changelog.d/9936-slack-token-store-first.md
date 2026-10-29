Added
- `credentials::resolve_env_var_bounded_store_first` and its hermetic core `resolve_provider_bounded_store_first_with` resolve a credential from the bounded store first, then the process environment (which reflects `.env.local`). A store read failure is returned as `Timeout` or `Store`, never answered from the environment. `resolve_env_var_bounded` keeps its env-first order (#9936).
