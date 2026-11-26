Added
- `config_read`, and so the console Config tab's `GET /api/console/config/mpm`, returns a read-only `install_paths` object: the resolved project home (`projects_root`) and config dir (`config_dir`), each with the source that set it. With a broken `install.toml` both paths are `null` and `error` names the file, so the console never shows the `$HOME` default in its place (#11821).
