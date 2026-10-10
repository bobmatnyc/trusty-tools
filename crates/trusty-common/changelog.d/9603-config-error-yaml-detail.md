Breaking
- `crate_config::ConfigError::Yaml { path, message: String }` is now `ConfigError::Yaml { path, detail: YamlErrorDetail }`, and `YamlErrorKind` is `#[non_exhaustive]`. A crate outside trusty-common that reads or builds the `message` field must use `detail` (kind, key path, line, column) instead; no crate in this workspace does (#9603).
