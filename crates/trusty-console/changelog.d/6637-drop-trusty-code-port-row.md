Changed
- The `DEFAULT_PORT` collision guard no longer reserves 7882 for trusty-code or names the deleted `DEFAULT_HTTP_PORT`. `tcode serve` binds no TCP port, and trusty-code-gui's bridge binds an ephemeral one ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637))
