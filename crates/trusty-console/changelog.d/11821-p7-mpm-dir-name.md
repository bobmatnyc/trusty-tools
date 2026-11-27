Changed
- The secrets project picker's fallback tm dir, used when `install.toml` does not parse, takes its name from `trusty_common::install_paths::MPM_DIR_NAME` instead of a copied literal; the path is unchanged (#11821).
