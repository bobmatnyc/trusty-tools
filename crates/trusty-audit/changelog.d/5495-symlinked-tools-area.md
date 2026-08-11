Fixed
- Installing refuses a `tools/` area that is a symlink or a file rather than a real directory. A pre-planted symlink would send the recipient's binaries outside the working directory and survive the `rm -rf` the README documents as a complete uninstall; the hazard was inert while nothing installed there ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
