Fixed

- The desktop shell compiles again, and puts its work root where the CLI puts
  its own. #5915 gave `WorkDir::resolve` a `home` argument and updated the CLI,
  but not the Tauri shell in `ui/src-tauri`, which no longer built. The shell
  now reads the home directory the same way `main.rs` does, so with nothing in
  the environment both front ends open `~/.trusty-tools/trusty-audit/work`
  rather than the shell landing beside the current directory.
