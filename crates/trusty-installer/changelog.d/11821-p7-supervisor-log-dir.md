Changed
- The trusty-mpm supervisor's log directory follows the configured config dir (`<config_dir>/.trusty-mpm/logs`), and the plist's `StandardOutPath`/`StandardErrorPath` point into it; the default path is unchanged. A broken `install.toml` now fails the supervisor install closed instead of logging under home (#11821).
